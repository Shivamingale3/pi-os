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
| `GET /alerts?after=ID` · `GET /alerts/status` · `POST /alerts/prefs?…` · `POST /alerts/test` | the phone-alert outbox for `pios web` (settings applied), whether sending works, the screen's Alerts settings, a test message |
| `GET /events` | last 7 days of SSH logins (and failures), sudo commands, Jenkins deploys and boots, newest first; repeated logins merged (×N) |
| `GET /details/TILE` | `cpu`, `temps`, `memory`, `disk` or `services`, collected on request (only while that screen is open) |
| `POST /containers/ID/restart` | the screen's one action: restarts (or starts) a container from the latest snapshot, at most once a minute each; answers 202 at once, progress shows in `/details/services` |

Trend metrics: `cpu.percent` · `cpu.tempC` · `mem.usedPercent` · `disk.usedPercent` · `load.1m` ·
`net.rxBytesPerSec` · `net.txBytesPerSec`.

## Incidents

| Kind | Opens when | Closes when |
|---|---|---|
| CPU %, RAM %, disk %, CPU °C | ≥ 80 · 85 · 85 · 75 (critical at ≥ 95 · 95 · 95 · 85) | below the threshold − 5 |
| container | a container seen running in this run is no longer running | it's running again, or removed |
| Docker | Docker was reachable in this run and no longer is | it's reachable again |
| host service | a `host` service from `/etc/pios/checks` isn't running | it's running again |
| website | a `web` hostname failed 2 checks in a row (warn: an unexpected code; critical: no answer or 5xx) | it answers the expected code |
| data disk | the `mount` disk isn't mounted or turned read-only (critical); its space ≥ 85 % (critical ≥ 95); its health report: failing or ≥ 60 °C (critical), unreadable or new bad sectors, new cable errors, a failed self-test or ≥ 55 °C (warn) | the condition clears |
| Tailscale | its login ends within 14 days (warn), has expired, or it's logged out (critical) | logged in with time to spare |
| voltage | the Pi's under-voltage alarm is on (warn) | the alarm clears |

An incident **opens after 30 s** of the bad condition without a break, and **closes after 60 s**
of the clear condition without a break, so short spikes and flapping are ignored. Start and end
times are the first bad and first clear readings.

## Host and web checks

What to watch lives in one file on the Pi, `/etc/pios/checks`, never in this repo
(`pios setup` installs a commented `checks.example` next to it). Both readers pick up edits
within a minute.

```text
host nginx                    # a systemd service that should be running
web status.example.com        # https://HOST/ should answer 200
web s3.example.com 403        # …or the code given (redirects aren't followed)
web cloud.example.com/status.php   # a path: an app's own health page
mount /mnt/data               # a data disk that should be mounted there
```

- **Host services** are read from their cgroup (`/sys/fs/cgroup/system.slice/NAME.service`):
  running, memory, CPU, uptime, main process. No D-Bus, no `systemctl`, no extra permissions.
- **Websites** are fetched by `pios web` (`pios-web.service`) every 60 s with one
  `curl --parallel`, from outside in (DNS, Cloudflare, the tunnel, the app). It is the only PiOS
  process with network access, under its own throwaway user (`DynamicUser`); it writes
  `/run/pios-web/web.json`, which the agent reads (results older than 3 minutes are ignored).
  The agent itself stays on unix sockets only.
- **A data disk** (`mount`, the first one; the Disk screen's *Data* tab): mounted and writable
  (`/proc/self/mountinfo`), space, I/O and idle time (sysfs), label (udev's database), USB link
  speed. Its health report (SMART: verdict, bad sectors, cable errors, temperature, last
  self-test) comes from the helper running `smartctl -n standby` every 30 min, so a sleeping disk
  is never woken (needs `smartmontools`). Cable-error and bad-sector rises count as new for a day.
- **Tailscale** (when `tailscaled` runs): its state and login expiry from its local API socket,
  every 10 min, shown on the `tailscaled` host row.
- **Power**: the `rpi_volt` under-voltage alarm every 5 s (an event when it trips), and after each
  boot whether the one before ended with a clean shutdown; if not, History shows **Power lost**
  and a problem alert goes out.

## Phone alerts (Telegram)

The agent decides *what* to send; `pios web`, the only PiOS process with network, sends it.

- **What**: problems (an incident opens, turns critical, recovers), failed SSH logins, deploys and
  reboots, and optionally every SSH login and sudo command. Switches in **Settings → Alerts**;
  *Quiet at night* holds back everything but red problems (and their recovery) inside the Night
  window. A burst becomes one message (it waits up to 10 s for more, at most a minute).
- **How**: the agent queues alerts in SQLite (kept 7 days) and applies the screen's settings;
  `pios web` asks `GET /alerts?after=ID` every 15 s and sends them with one `curl`, retrying with
  backoff (up to 5 min) until Telegram accepts. A restart (a deploy) doesn't re-send a problem
  that's still the same one, and a problem that cleared meanwhile gets its "✅ Recovered".
- **Setup**: create a bot with @BotFather, put its token on line 1 of `/etc/pios/telegram`
  (`pios setup` creates it empty, root-only), and send the bot any message once: `pios web`
  learns the chat from it (or put the chat ID on line 2). Settings → Alerts → *Send test message*.
  The token reaches `pios web` as a systemd credential and goes to curl on its stdin, never on a
  command line or in a log.

## Screen (in progress)

`pios ui` draws the 480 × 320 screens from the agent's API with **tiny-skia** (anti-aliased 2D) and
**ab_glyph** (TrueType text), using the bundled Inter and JetBrains Mono fonts (SIL OFL, in
`assets/fonts/`). It draws **Home**, and a tap on a Home tile opens its detail screen: **CPU** (per-core load and
frequency, load average, 24 h graph, busiest processes), **Temperatures** (every sensor with its
limits, the fan), **Memory** (apps / kernel / cache / free, zram or swap, biggest users) and **Disk**
(filesystems, the drive, live read/write, Docker disk usage). **History** has two tabs. **Incidents** lists the last 7 days of
incidents; a row opens its start, peak and end snapshots. **Events** lists SSH logins (and
failures), sudo commands, Jenkins deploys and boots from the journal (read once a minute by the
agent, which is in the `systemd-journal` group for this), one day per page. The big status card opens **Services**,
on the first tab with a problem: **Apps · Host · Web**. Apps lists one row per container (full name, health or uptime, memory), problems first (red: not running; amber: unhealthy, restarting or paused),
4 per page, or 3 with the ‹ › pager; a row opens the container (state, uptime or exit code, CPU and memory with its limit,
image, Compose project, ports) and its **Restart** button (**Start** when stopped): hold it 2 s;
lifting or sliding away cancels, and a tap does nothing. **Host** lists one row per host service
(its detail shows the unit, main process and tasks; no Restart). **Web** lists each hostname with
its response time and HTTP code, problems first, 4 per page. **Fans** (tap the fan card on Temperatures, or from the menu) shows the case fan driven by
[fan_ctl](https://github.com/Shivamingale3/fan_ctl) (its speed from the PWM, whether fan_ctl is
running, and which of NVMe or CPU it follows) beside the Active Cooler (rpm, level, where it starts).
With fan_ctl ≥ 0.2 each fan has **Auto / Manual** and **− / +** (case fan Off or its floor … 100 %
in 5 % steps, cooler level 0 … 4), and **Manual lasts** 30 min, 1 hour or 4 hours, then fan_ctl goes
back to auto. PiOS only asks: the helper writes fan_ctl's override file, and fan_ctl keeps its own
safety rules (too hot or a sensor fault: case fan 100 %, cooler back to the kernel). Manual changes
show in History → Events.
The **menu** button on Home opens Settings: the **LED ring** screen chooses the pattern for each
state (all good: breathe · steady · off; attention: pulse · blink · steady; down: blink · pulse ·
steady) and its brightness; a tap saves it and plays it on the ring for 6 s.

**Settings** (menu on Home) holds everything adjustable, saved at once in `/var/lib/pios-ui/`:
**Display** (screen off after 1 min … 1 hour or never; night face after 1–30 min; 12- or 24-hour
clock), **Night** (night face on/off, start and end in half-hour steps, the ring at night: faint,
off or as by day), **LED ring** (a pattern per state and the brightness) and **Fans**.

**Night face**: inside the night window, after the chosen time without a touch, Home becomes a big
dim clock with one status line (amber or red when something's wrong); a tap brings Home back. The
backlight can only be on or off, so the night face is dark pixels, and the screen still turns off
after the Display timeout. At night the ring keeps its patterns by default; *Faint* tops them at
about 40 %, *Off* darkens it while all is good (problems still show).

**Boot screen**: in the first 5 minutes after the Pi boots, until systemd has finished starting
and the agent answers, the screen shows *Starting up*: how long ago it booted, the system, the
agent, and how many containers are running so far; then Home. (Later, a silent agent shows as
*Waiting for data* on Home: that's a fault, not a boot.)

**Shutdown screen**: when systemd stops the screen because the whole system is going down
(`systemctl is-system-running` = stopping), it shows *Shutting down* or *Restarting*, then the
containers stopping one by one (their cgroups), for up to 45 s; the ring fades with them and goes
dark when PiOS is done. The last frame stays on the panel until power-off.

**LED ring**: the button's white ring says what Home says, by rhythm (breathing, a double pulse,
a minute of blinking then the pulse; rising ramps while it starts; it fills with a Restart hold and
flashes twice when it fires). A thread in `pios ui` drives the PWM, writing only when the level
changes (at most 25 times a second, gamma-corrected). It keeps going while the screen is dark.

Each screen has a 44 px back button and, top right, a refresh button that fetches what it shows
again at once (website results themselves are taken once a minute by `pios web`). Times are
12-hour ("8:27 PM").
Detail data is collected only while its screen is open (`GET /details/{cpu,temps,memory,disk,services}`).

Output goes to the panel (`--fb`), to a PNG, or to a laptop preview window, where a mouse click acts
as a tap.

```bash
pios ui --png home.png                          # one frame from the running agent
pios ui --sample worst --screen cpu --png cpu.png  # examples: good, attention, down, waiting, worst
                                                # screens: home, cpu, temps, memory, disk, history, incident
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

Tests run the check on every screen with thousands of combinations of extreme data (long names,
100 %, 105 °C, petabytes, 256 cores, 12 open problems, empty and missing data) and compare each
example with a golden PNG in `tests/golden/`. After an intended visual change, run `UPDATE_GOLDEN=1 cargo test`.

## On the Pi

PiOS runs **natively as systemd services**, with no container. All the files below ship inside the
binary and are installed by `pios setup`.

| Unit | Runs as | What |
|---|---|---|
| `pios-agent.service` | `pios` (no shell, no Docker group; in `systemd-journal` to read the journal) | the agent. Nice 10, idle IO, `CPUQuota=15%`, `MemoryHigh=24M` (trims the journal's page cache early), `MemoryMax=80M`, unix sockets only, read-only system, syscall filter |
| `pios-helper.socket` + `.service` | root, sandboxed, no network | the **only** process that touches Docker. It forwards exactly `GET /containers/json?all=true`, `GET /system/df` and one action, `POST /containers/ID/restart?t=10` for a full 64-hex ID (rebuilt from the ID, at most 2 at once); anything else gets 403. It also runs `smartctl` for `GET /smart/sdX` (an sd disk name only), with `CAP_SYS_RAWIO` and read access to sd* block devices and nothing else |
| `pios-web.service` | throwaway (`DynamicUser`) | the website checks: one `curl --parallel` a minute, results in `/run/pios-web/`. The only unit with network access. `MemoryMax=48M`, `CPUQuota=10%`, read-only system |
| `pios-ui.service` (installed; **you** enable it) | `pios-ui` (groups video, input, pios) | the screen: Home on `/dev/fb0`, sending only changed rows; resistive touch with the libinput calibration matrix; backlight off after 30 min idle (a touch wakes it); the LED ring (its PWM channel is opened to `pios-ui` at start, dark again at stop). `Conflicts=tft-btop.service case-ring-breathe.service`. `MemoryMax=60M`, `CPUQuota=10%`, only `/dev/fb0` and input devices allowed |

`systemd-analyze security` rates the agent 1.0 ("OK"; it may read other processes' CPU use for the CPU screen), the helper 0.7 and the screen 0.8 ("SAFE").

```bash
sudo systemctl start pios-ui     # PiOS takes the panel (stops tft-btop)
sudo systemctl start tft-btop    # back to the old session (stops pios-ui)

# make it the screen at boot (setup never changes this; uninstall disables pios-ui)
sudo systemctl disable tft-btop && sudo systemctl enable pios-ui
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
