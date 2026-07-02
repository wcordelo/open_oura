//! Headless accelerometer → JSONL logger shared by `oura log`.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use oura_link::transport::Transport;
use oura_link::OuraClient;

/// Stream live ACM samples for `seconds` and append JSONL lines to `output`.
/// Returns the number of samples written.
pub async fn log_to_jsonl<T: Transport>(
    client: &OuraClient<T>,
    seconds: u64,
    output: &Path,
) -> Result<u64> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(output)
        .with_context(|| format!("opening {}", output.display()))?;

    let mut count = 0u64;
    client
        .stream_accelerometer(Duration::from_secs(seconds.max(1)), |s| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let line = format!(
                "{{\"t\":{now},\"x\":{},\"y\":{},\"z\":{}}}\n",
                s.x, s.y, s.z
            );
            let _ = file.write_all(line.as_bytes());
            count += 1;
        })
        .await?;

    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::time::Duration;

    use super::log_to_jsonl;
    use oura_link::transport::mock::MockTransport;
    use oura_link::OuraClient;
    use oura_protocol::protocol;

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

    #[tokio::test]
    async fn log_to_jsonl_writes_mock_samples() {
        let mock = Arc::new(MockTransport::new());
        let on_hex = hex::encode(protocol::req_set_realtime(protocol::realtime::ACM, 1, 0));
        mock.on(&on_hex, &[]);
        mock.on("060400000000", &[]);

        let mock_feed = mock.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            mock_feed.inject_frame(acm_frame(7, 8, 1024));
            mock_feed.inject_frame(acm_frame(9, 10, 1030));
        });

        let client = OuraClient::new(mock);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("headless.jsonl");

        let count = log_to_jsonl(&client, 1, &path).await.expect("log");
        assert!(count >= 2, "expected samples, got {count}");

        let body = fs::read_to_string(&path).expect("read");
        assert!(body.contains("\"x\":7"));
        assert!(body.contains("\"z\":1024"));
    }
}
