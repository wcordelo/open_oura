# Berendo Labs POC — open_oura

A proof-of-concept for evaluating **open_oura**: a cloud-free Oura ring client that
reads data directly over BLE. This POC combines two capabilities in one workflow:

1. **Motion visualizer** — real-time 3D orientation and trajectory from the ring's
   live accelerometer stream (~50 Hz).
2. **Raw data logger** — timestamped JSONL capture of every x/y/z sample for
   offline analysis.

## Quick start

Prerequisites: a paired Oura ring (Ring 3/4/5), Bluetooth, and the ring's 16-byte
auth key file.

```bash
cargo build --release

# Combined POC dashboard (recommended)
./target/release/oura --key-file key.hex poc
# → open http://127.0.0.1:8080
# → click Start, move your hand, click Download JSONL when done
```

### Headless logging (no browser)

```bash
./target/release/oura --key-file key.hex log --seconds 30 --output session.jsonl
```

### Existing tools (still available)

```bash
./target/release/oura --key-file key.hex viz    # visualizer only (port 8088)
./target/release/oura --key-file key.hex accel  # terminal preview (15 s)
./target/release/oura --key-file key.hex sync   # history events → SQLite
```

## JSONL format

Each line is one accelerometer sample:

```json
{"t":1719345678901,"x":-42,"y":1024,"z":156}
```

| Field | Meaning |
| --- | --- |
| `t` | Host Unix timestamp in milliseconds |
| `x`, `y`, `z` | Signed raw accelerometer counts from the ring |

At rest, magnitude is roughly 1024 counts (≈1 g). Sample rate is ~50 Hz when the
ring is worn and streaming.

## POC commands

| Command | Description |
| --- | --- |
| `oura poc` | Web dashboard: motion viz + live JSONL log + download |
| `oura log` | Headless JSONL logger (no web UI) |
| `oura viz` | Visualizer only (no file logging) |

### `oura poc` flags

| Flag | Default | Description |
| --- | --- | --- |
| `--port` | `8080` | Local HTTP port |
| `--minutes` | `5` | Max stream duration per Start click |
| `--output` | `poc-<timestamp>.jsonl` | Log file path |

## Architecture

```
Ring (BLE) ──► oura-link (ACM stream) ──► motion_server
                                              ├── SSE → browser (viz)
                                              └── JSONL → disk (logger)
```

The POC reuses the same live-ACM pipeline as `oura viz` and `oura game`. The only
addition is optional server-side JSONL logging with `/stats` and `/download`
endpoints.

## Limitations (by design)

- **Accel only on live BLE** — gyroscope data requires RData bulk sampling, which
  is not exposed in this POC (see `docs/native-decoder.md`).
- **Trajectory drifts** — without live gyro, yaw is unobservable and integrated
  position drifts. Orientation (pitch/roll) from gravity is reliable.
- **Ring must be worn** — the ACM stream only delivers samples when the ring detects
  motion on a finger.
- **Auth key required** — live streaming is auth-gated. Pair first or extract the
  key from the official app.

## Berendo Labs context

This POC is part of the Berendo Labs initiative to evaluate independent wearable
data access. open_oura demonstrates that ring sensor data (HR, SpO2, temperature,
motion, sleep stages) is recoverable without Oura's cloud — useful for research,
custom analytics, and privacy-sensitive applications.

For the full protocol reference and decoder status, see the main
[`README.md`](../README.md) and [`crates/README.md`](../crates/README.md).

## Safety

- The POC only enables the real-time accelerometer stream (read-only sensor path).
- Stop streaming before disconnecting (the UI sends `/stop` on tab close; Ctrl-C
  also tears down cleanly).
- Never commit auth keys or capture files (both are gitignored).
