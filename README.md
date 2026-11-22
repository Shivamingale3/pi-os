# PiOS

Monitoring for a Raspberry Pi 5 home server, built to cost the server almost nothing.
Later it will also drive the case's 480 × 320 touch screen and the LED ring on its button.

The **core agent** runs today. Every 5 s it collects host, CPU, memory, disk, network and Docker
data, keeps the latest snapshot in memory, and stores two things in SQLite:

- a **rolling 24 h trend** (the latest reading of 7 metrics once a minute)
- **incidents** (kept 7 days), each with full snapshots at its start, peak and end

On a laptop it measures about **4.5 MB RSS, 2 threads, and a 1.6 MB binary**.

## Usage

```bash
pios agent      # run forever (Ctrl+C / SIGTERM stops cleanly)
pios snapshot   # print one snapshot as JSON
pios version
```

| Variable | Default |
|---|---|
| `PIOS_DATA_DIR` | `./data` (holds `pios.db`) |
| `PIOS_SOCKET` | `$PIOS_DATA_DIR/pios.sock` (max 107 bytes) |
| `PIOS_DOCKER_SOCKET` | `/var/run/docker.sock` |

## API

The agent serves plain HTTP on a **unix socket** (mode 0660), so it opens no TCP port.

```bash
curl --unix-socket data/pios.sock http://pios/now
```

| Route | Returns |
|---|---|
| `GET /now` | the latest snapshot (503 until the first one) |
| `GET /trend?metric=NAME` | last 24 h: `{"metric", "windowMs", "points": [[ts, value], …]}` |
| `GET /incidents` | the last 7 days, newest first, without snapshots |
| `GET /incidents/ID` | one incident with `snapshots.start`, `.peak` and `.end` |

Trend metrics: `cpu.percent` · `cpu.tempC` · `mem.usedPercent` · `disk.usedPercent` · `load.1m` ·
`net.rxBytesPerSec` · `net.txBytesPerSec`.

## Incidents

| Kind | Opens when | Closes when |
|---|---|---|
| CPU %, RAM %, disk %, CPU °C | ≥ 80 · 85 · 85 · 75 (critical at ≥ 95 · 95 · 95 · 85) | below the threshold − 5 |
| container | a container seen running in this run is no longer running | it's running again, or removed |
| Docker | Docker was reachable in this run and no longer is | it's reachable again |

An incident **opens after 30 s** of the bad condition without a break, and **closes after 60 s**
of the clear condition without a break, so short spikes and flapping are ignored. Start and end
times are the first bad and first clear readings.

## Build

```bash
cargo test && cargo clippy --all-targets -- -D warnings
cargo build --release
```

Dependencies: `rusqlite` (SQLite bundled), `serde`, `serde_json`, `libc`. No async runtime and no
HTTP crate.

## Layout

| Path | What |
|---|---|
| `src/collect/` | one reader + pure parser per source (`/proc`, `/sys`, `statvfs`, the Docker socket) |
| `src/parse.rs` | strict number parsing: anything that isn't a plain decimal is an error |
| `src/rules.rs` | incident engine: pure logic, no clock, no I/O |
| `src/store.rs` | SQLite (WAL): trend, incidents, pruning, reads |
| `src/api.rs` | the HTTP server on the unix socket |
| `src/main.rs` | CLI, the loop, signal handling |

## License

MIT
