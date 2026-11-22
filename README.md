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

## Screen (in progress)

`pios ui` draws the 480 × 320 screens from the agent's API with **tiny-skia** (anti-aliased 2D) and
**ab_glyph** (TrueType text), using the bundled Inter and JetBrains Mono fonts (SIL OFL, in
`assets/fonts/`). So far it draws **Home**: on the panel (`--fb`), to a PNG, or in a laptop preview window
(a mouse click acts as a tap).

```bash
pios ui --png home.png                          # one frame from the running agent
pios ui --sample worst --png worst.png          # a built-in example: good, attention, down, waiting, worst
pios ui --check                                 # prove the spacing rules on every example
cargo run --features preview -- ui --preview    # live window on a laptop (never in the Pi build)
```

Every element is placed as a box before anything is drawn, and `layout::check` proves these rules on
the boxes:

| Rule | Value |
|---|---|
| distance from every screen edge | 12 px |
| between any two blocks or touch targets | 8 px |
| inside cards / tiles and buttons | 16 px / 12 px |
| between any two elements inside a block | at least 4 px |
| touch targets | at least 44 × 44 px |
| text | must fit its slot: big numbers shrink (down to 20 px), other text is cut with "…" |

Tests run the check on more than 5,000 combinations of extreme data (long hostnames, 100 %, 105 °C,
1023 GB, 120 containers, 12 open problems) and compare each example with a golden PNG in
`tests/golden/`. After an intended visual change, run `UPDATE_GOLDEN=1 cargo test`.

## On the Pi

PiOS runs **natively as systemd services**, with no container. All the files below ship inside the
binary and are installed by `pios setup`.

| Unit | Runs as | What |
|---|---|---|
| `pios-agent.service` | `pios` (no shell, no Docker group) | the agent. Nice 10, idle IO, `CPUQuota=15%`, `MemoryMax=80M`, unix sockets only, read-only system, syscall filter |
| `pios-helper.socket` + `.service` | root, sandboxed, no network | the **only** process that touches Docker. It forwards exactly `GET /containers/json?all=true`; anything else gets 403 |
| `pios-ui.service` (**installed, not enabled**) | `pios-ui` (groups video, input, pios) | the screen: Home on `/dev/fb0`, sending only changed rows; resistive touch with the libinput calibration matrix; backlight off after 30 min idle (a touch wakes it). `Conflicts=tft-btop.service`. `MemoryMax=60M`, `CPUQuota=10%`, only `/dev/fb0` and input devices allowed |

`systemd-analyze security` rates the agent 0.8, the helper 0.7 and the screen 0.8 ("SAFE").

```bash
sudo systemctl start pios-ui     # PiOS takes the panel (stops tft-btop)
sudo systemctl start tft-btop    # back to the old session (stops pios-ui)
```

```bash
# first install, as root: download a release, check it, install
curl -fsSLO https://github.com/Shivamingale3/pi-os/releases/download/v0.2.0/pios-aarch64-linux-musl
curl -fsSLO https://github.com/Shivamingale3/pi-os/releases/download/v0.2.0/SHA256SUMS
sha256sum -c SHA256SUMS && install -m 0755 pios-aarch64-linux-musl /usr/local/bin/pios
pios setup                       # users pios + pios-deploy, units, sudoers line; starts the agent

pios update v0.3.0               # later: download, checksum, swap, health check, auto-rollback
pios update --rollback           # back to the previous binary
pios uninstall [--keep-data]     # remove everything setup created
```

**Deploys from Jenkins** (`deploy/Jenkinsfile`): GitHub's "release published" webhook triggers a
job that SSHes to `pios-deploy` with a key restricted to `pios deploy-ssh`. That command accepts only
`update vX.Y.Z` or `rollback`, and runs `sudo pios update` (the only sudo rule: `deploy/sudoers-pios-deploy`).
`pios setup --deploy-key key.pub` installs that key.

## Build

```bash
cargo test && cargo clippy --all-targets -- -D warnings
cargo build --release
```

Dependencies: `rusqlite` (SQLite bundled), `serde`, `serde_json`, `libc`, `tiny-skia`, `ab_glyph`
(and `minifb` for the laptop preview only). No async runtime and no HTTP crate.

## CI and releases

- **CI** (`.github/workflows/ci.yml`), on every PR and every push to `main`: `cargo fmt --check`,
  clippy (warnings are errors), tests, then a **static arm64 musl** build (`cargo zigbuild`). The
  binary is kept as a workflow artifact for 14 days.
- **Release** (`.github/workflows/release.yml`), when a `v*` tag is pushed: checks that the tag
  matches the `Cargo.toml` version, runs the tests, builds the arm64 binary, and publishes a GitHub
  release with `pios-aarch64-linux-musl` and `SHA256SUMS`.

```bash
# after bumping the version in Cargo.toml and merging:
git tag v0.1.0 && git push origin v0.1.0
```

## Layout

| Path | What |
|---|---|
| `src/collect/` | one reader + pure parser per source (`/proc`, `/sys`, `statvfs`, the Docker socket) |
| `src/parse.rs` | strict number parsing: anything that isn't a plain decimal is an error |
| `src/rules.rs` | incident engine: pure logic, no clock, no I/O |
| `src/store.rs` | SQLite (WAL): trend, incidents, pruning, reads |
| `src/api.rs` | the HTTP server on the unix socket |
| `src/http.rs` | the small HTTP client for unix sockets (Docker, the agent's API) |
| `src/ui/` | the screens: `theme` (tokens and spacing rules), `canvas` (drawing), `layout` (box tree, check, render), `home` |
| `src/helper.rs` | the root helper: filtered Docker access |
| `src/setup.rs`, `src/update.rs` | install, update with rollback, uninstall, the deploy-key command |
| `deploy/` | systemd units, the sudoers line, the Jenkinsfile |
| `src/main.rs` | CLI, the loop, signal handling |

## License

MIT
