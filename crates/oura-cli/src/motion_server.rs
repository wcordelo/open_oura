//! Shared local web server for the live-motion pages (`viz`, `game`, `poc`).
//!
//! Streams the ring's accelerometer over Server-Sent Events to a self-contained
//! HTML page (no external scripts/CDN), and exposes `/start` and `/stop` to arm
//! the BLE stream. Each caller supplies its own page via `index_html`; everything
//! else — parsing, fan-out, optional JSONL logging, and the loopback/CSRF
//! defences — is shared.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;

use oura_link::client::AcmSample;
use oura_protocol::protocol;
use oura_link::transport::Transport;
use oura_link::OuraClient;

/// Optional JSONL logging for live accelerometer samples.
#[derive(Clone, Default)]
pub struct LogOptions {
    /// Append samples to this file as JSONL (`{"t":…,"x":…,"y":…,"z":…}`).
    pub path: Option<PathBuf>,
}

/// Shared state for an active log session.
struct LogState {
    path: PathBuf,
    samples: u64,
}

/// Serve `index_html` at `127.0.0.1:port`. Streaming is toggled from the page;
/// each "start" arms the ring for `minutes` (so it auto-stops if the page closes).
pub async fn run<T: Transport + 'static>(
    client: OuraClient<T>,
    port: u16,
    minutes: u16,
    index_html: &'static str,
    log: LogOptions,
) -> Result<()> {
    let client: Arc<OuraClient<T>> = Arc::new(client);
    let (tx, _) = broadcast::channel::<String>(512);
    // Count of live SSE clients: when the last one drops (tab closed), stop the
    // ring so we don't keep streaming (and draining battery) until its timer.
    let clients = Arc::new(AtomicUsize::new(0));

    let log_state: Option<Arc<Mutex<LogState>>> = if let Some(path) = log.path {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            }
        }
        // Truncate on each server start so a fresh POC session gets a clean file.
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .with_context(|| format!("opening log {}", path.display()))?;
        println!("Logging samples to {}", path.display());
        Some(Arc::new(Mutex::new(LogState { path, samples: 0 })))
    } else {
        None
    };

    // Always-on parser: raw ring notifications -> ACM samples -> JSON to the page.
    let mut raw_rx = client.transport().subscribe();
    let tx_parse = tx.clone();
    let log_parse = log_state.clone();
    tokio::spawn(async move {
        loop {
            match raw_rx.recv().await {
                Ok(frame) => {
                    for s in AcmSample::parse_frame(&frame) {
                        if let Some(ls) = &log_parse {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let line = format!(
                                "{{\"t\":{now},\"x\":{},\"y\":{},\"z\":{}}}\n",
                                s.x, s.y, s.z
                            );
                            if let Ok(mut guard) = ls.lock() {
                                if let Ok(mut f) = OpenOptions::new().append(true).open(&guard.path) {
                                    let _ = f.write_all(line.as_bytes());
                                }
                                guard.samples += 1;
                            }
                        }
                        let _ = tx_parse.send(format!("{{\"x\":{},\"y\":{},\"z\":{}}}", s.x, s.y, s.z));
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(_) => break,
            }
        }
    });

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    println!("Ready — open http://127.0.0.1:{port}  (use Start in the page)");

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                let _ = client.transport().write(&protocol::req_realtime_off()).await;
                if let Some(ls) = &log_state {
                    let guard = ls.lock().unwrap();
                    println!("\nStopped streaming. Logged {} samples to {}", guard.samples, guard.path.display());
                } else {
                    println!("\nStopped streaming, exiting.");
                }
                break;
            }
            accept = listener.accept() => {
                if let Ok((sock, _)) = accept {
                    let rx = tx.subscribe();
                    let c = client.clone();
                    let cl = clients.clone();
                    let ls = log_state.clone();
                    tokio::spawn(async move { let _ = handle(sock, rx, c, cl, port, minutes, index_html, ls).await; });
                }
            }
        }
    }
    Ok(())
}

/// Case-insensitive lookup of an HTTP header value in the raw request.
fn header<'a>(req: &'a str, name: &str) -> Option<&'a str> {
    req.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn handle<T: Transport + 'static>(
    mut sock: TcpStream,
    mut rx: broadcast::Receiver<String>,
    client: Arc<OuraClient<T>>,
    clients: Arc<AtomicUsize>,
    port: u16,
    minutes: u16,
    index_html: &'static str,
    log_state: Option<Arc<Mutex<LogState>>>,
) -> Result<()> {
    let mut buf = [0u8; 2048];
    let n = sock.read(&mut buf).await?;
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req.split_whitespace().nth(1).unwrap_or("/");

    // Defend the local server against DNS-rebinding and cross-site (CSRF) calls:
    // require a loopback Host on every request, and a same-origin Origin on the
    // control endpoints (browsers attach Origin to cross-site fetches).
    let host_ok = header(&req, "host").is_some_and(|h| {
        h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}")
    });
    if !host_ok {
        return forbidden(&mut sock).await;
    }
    if matches!(path, "/start" | "/stop") {
        // Require a custom header. Same-origin fetch (our page) can set it; an
        // <img>/<form>/navigation cannot add headers, and a cross-origin fetch
        // that tries is blocked by the CORS preflight we never approve. This
        // closes the no-Origin GET CSRF vector that an Origin check alone misses.
        if header(&req, "x-oura-viz").is_none() {
            return forbidden(&mut sock).await;
        }
        // Defence in depth: also reject a mismatched Origin when present.
        let origin_ok = header(&req, "origin").is_none_or(|o| {
            o == format!("http://127.0.0.1:{port}") || o == format!("http://localhost:{port}")
        });
        if !origin_ok {
            return forbidden(&mut sock).await;
        }
    }

    match path {
        "/stream" => {
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                  Cache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
            )
            .await?;
            clients.fetch_add(1, Ordering::SeqCst);
            loop {
                match rx.recv().await {
                    Ok(line) => {
                        if sock
                            .write_all(format!("data: {line}\n\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
            // This client is gone; if it was the last one, stop the ring stream.
            if clients.fetch_sub(1, Ordering::SeqCst) == 1 {
                let _ = client.transport().write(&protocol::req_realtime_off()).await;
            }
        }
        "/stats" => {
            let body = if let Some(ls) = &log_state {
                let (path, samples, bytes) = {
                    let guard = ls.lock().unwrap();
                    let bytes = fs::metadata(&guard.path).map(|m| m.len()).unwrap_or(0);
                    (guard.path.display().to_string(), guard.samples, bytes)
                };
                format!(r#"{{"path":"{path}","samples":{samples},"bytes":{bytes}}}"#)
            } else {
                r#"{"path":null,"samples":0,"bytes":0}"#.to_string()
            };
            json_ok(&mut sock, &body).await?;
        }
        "/download" => {
            if let Some(ls) = &log_state {
                let (data, filename) = {
                    let guard = ls.lock().unwrap();
                    let data = fs::read(&guard.path).unwrap_or_default();
                    let filename = guard
                        .path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("oura-accel.jsonl")
                        .to_string();
                    (data, filename)
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/jsonl\r\n\
                     Content-Disposition: attachment; filename=\"{filename}\"\r\n\
                     Content-Length: {}\r\n\r\n",
                    data.len()
                );
                sock.write_all(resp.as_bytes()).await?;
                sock.write_all(&data).await?;
            } else {
                not_found(&mut sock).await?;
            }
        }
        "/start" => {
            let _ = client
                .transport()
                .write(&protocol::req_set_realtime(protocol::realtime::ACM, minutes, 0))
                .await;
            ok(&mut sock, "started").await?;
        }
        "/stop" => {
            let _ = client.transport().write(&protocol::req_realtime_off()).await;
            ok(&mut sock, "stopped").await?;
        }
        _ => {
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Cache-Control: no-store\r\nContent-Length: {}\r\n\r\n{}",
                index_html.len(),
                index_html
            );
            sock.write_all(resp.as_bytes()).await?;
        }
    }
    Ok(())
}

async fn ok(sock: &mut TcpStream, msg: &str) -> Result<()> {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
        msg.len(),
        msg
    );
    sock.write_all(resp.as_bytes()).await?;
    Ok(())
}

async fn json_ok(sock: &mut TcpStream, body: &str) -> Result<()> {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    sock.write_all(resp.as_bytes()).await?;
    Ok(())
}

async fn forbidden(sock: &mut TcpStream) -> Result<()> {
    sock.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
        .await?;
    Ok(())
}

async fn not_found(sock: &mut TcpStream) -> Result<()> {
    sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    use oura_link::transport::mock::MockTransport;
    use oura_link::OuraClient;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    fn acm_frame(x: i16, y: i16, z: i16) -> Vec<u8> {
        vec![
            0x33, 0x0c, 0x32, 0x01,
            (x as u8),
            (x >> 8) as u8,
            (y as u8),
            (y >> 8) as u8,
            (z as u8),
            (z >> 8) as u8,
            0,
            0,
            0,
            0,
            0,
            0,
        ]
    }

    fn http_body(resp: &str) -> &str {
        resp.split("\r\n\r\n").nth(1).unwrap_or("")
    }

    async fn http_get(host: &str, path: &str, headers: &[(&str, &str)]) -> String {
        let mut stream = TcpStream::connect(host).await.expect("connect");
        let mut req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        stream.write_all(req.as_bytes()).await.expect("write");
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await.expect("read");
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[tokio::test]
    async fn poc_e2e_logs_stats_sse_and_download() {
        let mock = Arc::new(MockTransport::new());
        let start_hex = hex::encode(protocol::req_set_realtime(protocol::realtime::ACM, 5, 0));
        mock.on(&start_hex, &[]);
        mock.on("060400000000", &[]);

        let client = OuraClient::new(mock.clone());
        let dir = tempfile::tempdir().expect("tempdir");
        let log_path = dir.path().join("e2e.jsonl");

        let port = 19081u16;
        let log_path_spawn = log_path.clone();
        let server = tokio::spawn(async move {
            let _ = run(
                client,
                port,
                5,
                "<html><body>poc</body></html>",
                LogOptions {
                    path: Some(log_path_spawn),
                },
            )
            .await;
        });

        tokio::time::sleep(Duration::from_millis(150)).await;
        let host = format!("127.0.0.1:{port}");

        // Index page is served.
        let index = http_get(&host, "/", &[]).await;
        assert!(index.contains("200 OK"));
        assert!(http_body(&index).contains("poc"));

        // CSRF: /start without header is forbidden.
        let forbidden = http_get(&host, "/start", &[]).await;
        assert!(forbidden.contains("403"));

        // Start streaming (mock transport accepts the request).
        let origin = format!("http://{host}");
        let started = http_get(
            &host,
            "/start",
            &[("X-Oura-Viz", "1"), ("Origin", &origin)],
        )
        .await;
        assert!(started.contains("200 OK"), "start: {started}");

        // Inject accelerometer frames as if from the ring.
        mock.inject_frame(acm_frame(1, 2, 1024));
        mock.inject_frame(acm_frame(10, 20, 1030));
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Stats reflect logged samples.
        let stats_resp = http_get(&host, "/stats", &[]).await;
        assert!(stats_resp.contains("200 OK"), "stats status: {stats_resp}");
        let stats_body = http_body(&stats_resp);
        let stats: serde_json::Value = serde_json::from_str(stats_body.trim()).expect("json");
        assert_eq!(stats["samples"].as_u64(), Some(4)); // 2 frames × 2 samples each
        assert!(stats["bytes"].as_u64().unwrap_or(0) > 0);

        // SSE stream delivers JSON samples (connect first, then inject).
        let mut stream = TcpStream::connect(&host).await.expect("sse connect");
        stream
            .write_all(
                format!("GET /stream HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes(),
            )
            .await
            .expect("sse write");
        mock.inject_frame(acm_frame(100, 200, 1024));
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut buf = vec![0u8; 4096];
        let n = stream.read(&mut buf).await.expect("sse read");
        let sse = String::from_utf8_lossy(&buf[..n]);
        assert!(sse.contains("text/event-stream"), "sse headers: {sse}");
        assert!(sse.contains("\"x\":"), "sse data: {sse}");

        // Download returns the JSONL file.
        let dl = http_get(&host, "/download", &[]).await;
        assert!(dl.contains("200 OK"), "download: {dl}");
        let dl_body = http_body(&dl);
        assert!(dl_body.contains("\"t\":"));
        assert!(dl_body.contains("\"x\":1"));
        assert!(dl_body.lines().count() >= 4);

        // Stop and verify file on disk.
        let stopped = http_get(
            &host,
            "/stop",
            &[("X-Oura-Viz", "1"), ("Origin", &origin)],
        )
        .await;
        assert!(stopped.contains("200 OK"));

        let on_disk = fs::read_to_string(&log_path).expect("read log");
        assert!(on_disk.contains("\"z\":1024"));

        server.abort();
    }
}
