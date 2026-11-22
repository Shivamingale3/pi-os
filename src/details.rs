//! Tile details, collected **on request** (`GET /details/{cpu,temps,memory,disk}`), only while
//! that screen is open on the panel. Scanning every process every 5 s all day would cost CPU
//! for nothing, so none of this runs in the 5 s loop.
//!
//! Everything comes from /proc and /sys, readable without root; Docker disk usage comes from
//! the helper (one allowed read-only request) and is cached for 10 minutes.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::collect::cpu::{Ticks, parse_stat, percent};
use crate::error::{Result, err, read_file};
use crate::parse::{parse_f64, parse_u64};
use crate::types::Snapshot;

/// How long rates (per-core %, process %, disk I/O) are measured over, per request.
const SAMPLE: Duration = Duration::from_millis(300);

// ---------------- shapes (also read by the UI) ----------------

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Core {
    pub percent: f64,
    pub mhz: u32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub name: String,
    /// % of one core (CPU) or bytes (memory)
    pub value: f64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CpuDetails {
    pub percent: f64,
    pub cores: Vec<Core>,
    pub load: [f64; 3],
    pub min_mhz: u32,
    pub max_mhz: u32,
    pub governor: String,
    /// busiest processes now, % of one core
    pub top: Vec<Usage>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sensor {
    pub name: String,
    pub celsius: f64,
    /// where the fan starts (CPU only)
    pub fan_on: Option<f64>,
    /// the hardware's "max" / slowdown point
    pub limit: Option<f64>,
    pub critical: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fan {
    pub rpm: Option<u32>,
    pub step: u32,
    pub steps: u32,
    pub starts_at: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TempDetails {
    pub sensors: Vec<Sensor>,
    pub fan: Option<Fan>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Swap {
    pub total: u64,
    pub used: u64,
    /// "zram · zstd", "file", "partition"
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemDetails {
    pub total: u64,
    pub available: u64,
    pub apps: u64,
    pub kernel: u64,
    pub cache: u64,
    pub free: u64,
    pub swap: Option<Swap>,
    /// biggest users (containers, else processes), bytes
    pub top: Vec<Usage>,
    /// "containers" or "processes"
    pub top_kind: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fs {
    pub mount: String,
    pub used: u64,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Drive {
    pub model: String,
    pub size: u64,
    pub kind: String,
    pub celsius: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DockerDf {
    pub images: u32,
    pub images_bytes: u64,
    pub reclaimable: u64,
    pub containers: u32,
    pub containers_bytes: u64,
    pub volumes: u32,
    pub volumes_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskDetails {
    pub filesystems: Vec<Fs>,
    pub drive: Option<Drive>,
    pub read_bps: f64,
    pub write_bps: f64,
    pub docker: Option<DockerDf>,
}

// ---------------- CPU ----------------

/// "cpu…" lines of /proc/stat → ticks per line, in order (cpu, cpu0, cpu1, …).
pub fn parse_all_ticks(text: &str) -> Vec<Ticks> {
    text.lines()
        .filter(|l| l.starts_with("cpu"))
        .filter_map(|l| {
            let rest = l.split_once(' ')?.1;
            parse_stat(&format!("cpu {rest}")).ok()
        })
        .collect()
}

/// /proc/<pid>/stat → (comm, utime + stime). The name is in parentheses and may contain spaces.
pub fn parse_proc_stat(text: &str) -> Option<(String, u64)> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let name = text.get(open + 1..close)?.to_string();
    let fields: Vec<&str> = text.get(close + 2..)?.split_whitespace().collect();
    let t = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
    Some((name, t))
}

fn proc_times() -> HashMap<u32, (String, u64)> {
    let mut out = HashMap::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in dir.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            && let Some(v) = parse_proc_stat(&text)
        {
            out.insert(pid, v);
        }
    }
    out
}

/// Per-name CPU use between two process scans, % of one core, busiest first.
pub fn busiest(
    a: &HashMap<u32, (String, u64)>,
    b: &HashMap<u32, (String, u64)>,
    total_ticks: u64,
    cores: usize,
    n: usize,
) -> Vec<Usage> {
    if total_ticks == 0 {
        return vec![];
    }
    let per_core = total_ticks as f64 / cores.max(1) as f64;
    let mut by_name: HashMap<&str, u64> = HashMap::new();
    for (pid, (name, t1)) in b {
        if let Some((_, t0)) = a.get(pid) {
            *by_name.entry(name.as_str()).or_default() += t1.saturating_sub(*t0);
        }
    }
    let mut v: Vec<Usage> = by_name
        .into_iter()
        .filter(|(_, d)| *d > 0)
        .map(|(name, d)| Usage {
            name: name.to_string(),
            value: d as f64 / per_core * 100.0,
        })
        .collect();
    v.sort_by(|x, y| y.value.total_cmp(&x.value).then(x.name.cmp(&y.name)));
    v.truncate(n);
    v
}

fn read_khz(path: String) -> u32 {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|k| (k / 1000) as u32)
        .unwrap_or(0)
}

pub fn cpu() -> Result<CpuDetails> {
    let a = parse_all_ticks(&read_file("/proc/stat")?);
    let pa = proc_times();
    std::thread::sleep(SAMPLE);
    let b = parse_all_ticks(&read_file("/proc/stat")?);
    let pb = proc_times();
    if a.is_empty() || a.len() != b.len() {
        return Err(err("/proc/stat changed shape"));
    }
    let n = b.len() - 1;
    let cores = (0..n)
        .map(|i| Core {
            percent: percent(a[i + 1], b[i + 1]),
            mhz: read_khz(format!(
                "/sys/devices/system/cpu/cpu{i}/cpufreq/scaling_cur_freq"
            )),
        })
        .collect();
    let load = crate::collect::host::parse_loadavg(&read_file("/proc/loadavg")?)?;
    Ok(CpuDetails {
        percent: percent(a[0], b[0]),
        cores,
        load,
        min_mhz: read_khz("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_min_freq".into()),
        max_mhz: read_khz("/sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq".into()),
        governor: std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
            .unwrap_or_default()
            .trim()
            .to_string(),
        top: busiest(&pa, &pb, b[0].total.saturating_sub(a[0].total), n, 3),
    })
}

// ---------------- temperatures ----------------

fn read_milli(p: &Path) -> Option<f64> {
    let t = std::fs::read_to_string(p).ok()?;
    parse_f64(&t, "sensor").ok().map(|v| v / 1000.0)
}

/// Friendly name for a hwmon chip; None = not a temperature worth showing.
pub fn sensor_name(chip: &str) -> Option<(&'static str, u8)> {
    Some(match chip {
        "cpu_thermal" | "coretemp" | "k10temp" | "zenpower" => ("CPU", 0),
        "nvme" => ("NVMe", 1),
        "rp1_adc" => ("RP1 I/O chip", 2),
        "acpitz" => ("Board", 3),
        "iwlwifi_1" | "iwlwifi" => ("Wi-Fi", 4),
        "amdgpu" | "nouveau" => ("GPU", 2),
        _ => return None,
    })
}

pub fn temps() -> Result<TempDetails> {
    // CPU trip points: the first "active" one is where the fan starts
    let mut fan_on = None;
    if let Ok(zones) = std::fs::read_dir("/sys/class/thermal") {
        for z in zones.flatten() {
            let p = z.path();
            let kind = std::fs::read_to_string(p.join("type")).unwrap_or_default();
            if !matches!(kind.trim(), "cpu-thermal" | "x86_pkg_temp") {
                continue;
            }
            for i in 0..16 {
                let ty = std::fs::read_to_string(p.join(format!("trip_point_{i}_type")))
                    .unwrap_or_default();
                if ty.trim() == "active"
                    && let Some(t) = read_milli(&p.join(format!("trip_point_{i}_temp")))
                {
                    fan_on = Some(fan_on.map_or(t, |f: f64| f.min(t)));
                }
            }
        }
    }

    let mut found: Vec<(u8, Sensor)> = Vec::new();
    let mut fan = None;
    if let Ok(chips) = std::fs::read_dir("/sys/class/hwmon") {
        for c in chips.flatten() {
            let p = c.path();
            let chip = std::fs::read_to_string(p.join("name"))
                .unwrap_or_default()
                .trim()
                .to_string();
            if chip == "pwmfan" {
                let rpm = std::fs::read_to_string(p.join("fan1_input"))
                    .ok()
                    .and_then(|s| s.trim().parse().ok());
                fan = Some(Fan {
                    rpm,
                    step: 0,
                    steps: 0,
                    starts_at: fan_on,
                });
                continue;
            }
            let Some((name, prio)) = sensor_name(&chip) else {
                continue;
            };
            let Some(celsius) = read_milli(&p.join("temp1_input")) else {
                continue;
            };
            if found.iter().any(|(_, s)| s.name == name) {
                continue;
            }
            let limit = read_milli(&p.join("temp1_max"));
            let critical = read_milli(&p.join("temp1_crit"));
            let is_cpu = name == "CPU";
            found.push((
                prio,
                Sensor {
                    name: name.to_string(),
                    celsius,
                    fan_on: if is_cpu { fan_on } else { None },
                    limit,
                    critical,
                },
            ));
        }
    }
    // fan speed step from the matching cooling device
    if let (Some(f), Ok(devs)) = (fan.as_mut(), std::fs::read_dir("/sys/class/thermal")) {
        for d in devs.flatten() {
            let p = d.path();
            if std::fs::read_to_string(p.join("type"))
                .unwrap_or_default()
                .trim()
                == "pwm-fan"
            {
                f.step = std::fs::read_to_string(p.join("cur_state"))
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0);
                f.steps = std::fs::read_to_string(p.join("max_state"))
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                    .unwrap_or(0);
            }
        }
    }
    found.sort_by_key(|(prio, _)| *prio);
    Ok(TempDetails {
        sensors: found.into_iter().map(|(_, s)| s).collect(),
        fan,
    })
}

// ---------------- memory ----------------

pub fn parse_meminfo_kb(text: &str) -> HashMap<String, u64> {
    text.lines()
        .filter_map(|l| {
            let (k, rest) = l.split_once(':')?;
            let v = parse_u64(rest.split_whitespace().next()?, k).ok()?;
            Some((k.trim().to_string(), v * 1024))
        })
        .collect()
}

pub fn breakdown(m: &HashMap<String, u64>) -> Result<MemDetails> {
    let g = |k: &str| m.get(k).copied().unwrap_or(0);
    let total = m
        .get("MemTotal")
        .copied()
        .ok_or_else(|| err("MemTotal missing"))?;
    let apps = g("AnonPages");
    let kernel = g("Slab") + g("PageTables") + g("KernelStack");
    let cache = g("Cached") + g("Buffers");
    Ok(MemDetails {
        total,
        available: g("MemAvailable"),
        apps,
        kernel,
        cache,
        free: total.saturating_sub(apps + kernel + cache),
        ..Default::default()
    })
}

/// /proc/swaps → the first device; zram gets its compression algorithm.
pub fn parse_swaps(text: &str, zram_algo: impl Fn(&str) -> Option<String>) -> Option<Swap> {
    let line = text.lines().skip(1).find(|l| !l.trim().is_empty())?;
    let f: Vec<&str> = line.split_whitespace().collect();
    let (name, ty) = (*f.first()?, *f.get(1)?);
    let total = f.get(2)?.parse::<u64>().ok()? * 1024;
    let used = f.get(3)?.parse::<u64>().ok()? * 1024;
    let kind = match name.strip_prefix("/dev/").filter(|d| d.starts_with("zram")) {
        Some(dev) => match zram_algo(dev) {
            Some(a) => format!("zram · {a}"),
            None => "zram".into(),
        },
        None => ty.to_string(),
    };
    Some(Swap { total, used, kind })
}

/// "lzo-rle lzo lz4 [zstd]" → "zstd"
pub fn selected_algo(text: &str) -> Option<String> {
    let s = text.find('[')?;
    let e = text[s..].find(']')? + s;
    Some(text[s + 1..e].to_string())
}

fn cgroup_memory(id: &str) -> Option<u64> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    for p in [
        format!("/sys/fs/cgroup/system.slice/docker-{id}.scope/memory.current"),
        format!("/sys/fs/cgroup/docker/{id}/memory.current"),
    ] {
        if let Some(v) = std::fs::read_to_string(&p)
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            return Some(v);
        }
    }
    None
}

fn top_processes_rss(n: usize) -> Vec<Usage> {
    let mut by_name: HashMap<String, u64> = HashMap::new();
    if let Ok(dir) = std::fs::read_dir("/proc") {
        for e in dir.flatten() {
            let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let rss = std::fs::read_to_string(format!("/proc/{pid}/statm"))
                .ok()
                .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok());
            let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok();
            if let (Some(rss), Some(name)) = (rss, name) {
                *by_name.entry(name.trim().to_string()).or_default() += rss * 4096;
            }
        }
    }
    let mut v: Vec<Usage> = by_name
        .into_iter()
        .map(|(name, b)| Usage {
            name,
            value: b as f64,
        })
        .collect();
    v.sort_by(|x, y| y.value.total_cmp(&x.value));
    v.truncate(n);
    v
}

pub fn memory(live: Option<&Snapshot>) -> Result<MemDetails> {
    let mut d = breakdown(&parse_meminfo_kb(&read_file("/proc/meminfo")?))?;
    d.swap = std::fs::read_to_string("/proc/swaps").ok().and_then(|t| {
        parse_swaps(&t, |dev| {
            std::fs::read_to_string(format!("/sys/block/{dev}/comp_algorithm"))
                .ok()
                .and_then(|a| selected_algo(&a))
        })
    });
    let mut containers: Vec<Usage> = live
        .and_then(|s| s.docker.as_ref())
        .map(|dk| {
            dk.containers
                .iter()
                .filter_map(|c| {
                    Some(Usage {
                        name: c.name.clone(),
                        value: cgroup_memory(&c.id)? as f64,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if containers.is_empty() {
        d.top = top_processes_rss(5);
        d.top_kind = "processes".into();
    } else {
        containers.sort_by(|x, y| y.value.total_cmp(&x.value));
        containers.truncate(5);
        d.top = containers;
        d.top_kind = "containers".into();
    }
    Ok(d)
}

// ---------------- disk ----------------

/// Real filesystems from /proc/mounts: (device, mount point), "/" first, one per device.
pub fn real_mounts(text: &str) -> Vec<(String, String)> {
    const FS: [&str; 7] = ["ext4", "ext3", "ext2", "xfs", "btrfs", "vfat", "f2fs"];
    let mut out: Vec<(String, String)> = Vec::new();
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        let (Some(dev), Some(mnt), Some(fs)) = (f.first(), f.get(1), f.get(2)) else {
            continue;
        };
        if !dev.starts_with("/dev/") || !FS.contains(fs) || out.iter().any(|(d, _)| d == dev) {
            continue;
        }
        let mnt = mnt.replace("\\040", " ");
        out.push((dev.to_string(), mnt));
    }
    out.sort_by_key(|(_, m)| (m != "/", m.len()));
    out
}

/// nvme0n1p2 → nvme0n1, mmcblk0p2 → mmcblk0, sda2 → sda
pub fn parent_disk(part: &str) -> String {
    let p = part.trim_start_matches("/dev/");
    if p.starts_with("nvme") || p.starts_with("mmcblk") {
        // partitions end in "p<digits>"; the whole disk doesn't
        if let Some(i) = p.rfind('p')
            && i > 0
            && !p[i + 1..].is_empty()
            && p[i + 1..].bytes().all(|b| b.is_ascii_digit())
        {
            return p[..i].to_string();
        }
        return p.to_string();
    }
    p.trim_end_matches(|c: char| c.is_ascii_digit()).to_string()
}

/// (sectors read, sectors written) for one disk from /proc/diskstats.
pub fn diskstats(text: &str, disk: &str) -> Option<(u64, u64)> {
    text.lines().find_map(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        (f.get(2) == Some(&disk))
            .then(|| Some((f.get(5)?.parse().ok()?, f.get(9)?.parse().ok()?)))?
    })
}

#[derive(Deserialize)]
struct DfImage {
    #[serde(rename = "Size", default)]
    size: i64,
    #[serde(rename = "Containers", default)]
    containers: i64,
}
#[derive(Deserialize)]
struct DfContainer {
    #[serde(rename = "SizeRw", default)]
    size_rw: i64,
}
#[derive(Deserialize, Default)]
struct DfUsage {
    #[serde(rename = "Size", default)]
    size: i64,
}
#[derive(Deserialize)]
struct DfVolume {
    #[serde(rename = "UsageData", default)]
    usage: Option<DfUsage>,
}
#[derive(Deserialize)]
struct DfResponse {
    #[serde(rename = "LayersSize", default)]
    layers_size: i64,
    #[serde(rename = "Images", default)]
    images: Vec<DfImage>,
    #[serde(rename = "Containers", default)]
    containers: Vec<DfContainer>,
    #[serde(rename = "Volumes", default)]
    volumes: Vec<DfVolume>,
}

pub fn parse_docker_df(body: &str) -> Result<DockerDf> {
    let r: DfResponse = serde_json::from_str(body)?;
    let pos = |v: i64| v.max(0) as u64;
    Ok(DockerDf {
        images: r.images.len() as u32,
        images_bytes: pos(r.layers_size),
        // image sizes count shared layers more than once: never claim more than the total
        reclaimable: r
            .images
            .iter()
            .filter(|i| i.containers == 0)
            .map(|i| pos(i.size))
            .sum::<u64>()
            .min(pos(r.layers_size)),
        containers: r.containers.len() as u32,
        containers_bytes: r.containers.iter().map(|c| pos(c.size_rw)).sum(),
        volumes: r.volumes.len() as u32,
        volumes_bytes: r
            .volumes
            .iter()
            .filter_map(|v| v.usage.as_ref())
            .map(|u| pos(u.size))
            .sum(),
    })
}

static DOCKER_DF: Mutex<Option<(Instant, DockerDf)>> = Mutex::new(None);
const DOCKER_DF_TTL: Duration = Duration::from_secs(600);

fn docker_df(socket: &Path) -> Option<DockerDf> {
    if let Ok(g) = DOCKER_DF.lock()
        && let Some((at, v)) = g.as_ref()
        && at.elapsed() < DOCKER_DF_TTL
    {
        return Some(v.clone());
    }
    let (status, body) =
        crate::http::get(socket, crate::helper::DOCKER_DF, Duration::from_secs(10)).ok()?;
    let v = (status == 200)
        .then(|| parse_docker_df(&body).ok())
        .flatten()?;
    if let Ok(mut g) = DOCKER_DF.lock() {
        *g = Some((Instant::now(), v.clone()));
    }
    Some(v)
}

pub fn disk(docker_socket: &Path) -> Result<DiskDetails> {
    let mounts = real_mounts(&read_file("/proc/mounts")?);
    let mut filesystems = Vec::new();
    for (_, mnt) in mounts.iter().take(3) {
        if let Ok(d) = crate::collect::disk::read(mnt) {
            filesystems.push(Fs {
                mount: mnt.clone(),
                used: d.used_bytes,
                size: d.total_bytes,
            });
        }
    }
    let root_dev = mounts
        .iter()
        .find(|(_, m)| m == "/")
        .map(|(d, _)| parent_disk(d));
    let (mut read_bps, mut write_bps) = (0.0, 0.0);
    let mut drive = None;
    if let Some(dev) = &root_dev {
        let a = diskstats(&read_file("/proc/diskstats")?, dev);
        std::thread::sleep(SAMPLE);
        let b = diskstats(&read_file("/proc/diskstats")?, dev);
        if let (Some(a), Some(b)) = (a, b) {
            let s = SAMPLE.as_secs_f64();
            read_bps = b.0.saturating_sub(a.0) as f64 * 512.0 / s;
            write_bps = b.1.saturating_sub(a.1) as f64 * 512.0 / s;
        }
        let sys = format!("/sys/block/{dev}");
        let model = std::fs::read_to_string(format!("{sys}/device/model"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let size = std::fs::read_to_string(format!("{sys}/size"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0)
            * 512;
        let kind = if dev.starts_with("nvme") {
            "NVMe"
        } else if dev.starts_with("mmcblk") {
            "SD card"
        } else {
            "drive"
        };
        let celsius = if kind == "NVMe" {
            temps()
                .ok()
                .and_then(|t| t.sensors.into_iter().find(|s| s.name == "NVMe"))
                .map(|s| s.celsius)
        } else {
            None
        };
        drive = Some(Drive {
            model: if model.is_empty() { dev.clone() } else { model },
            size,
            kind: kind.into(),
            celsius,
        });
    }
    Ok(DiskDetails {
        filesystems,
        drive,
        read_bps,
        write_bps,
        docker: docker_df(docker_socket),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ticks_per_core() {
        let t = parse_all_ticks(
            "cpu  10 0 0 90 0 0 0 0 0 0\ncpu0 5 0 0 45 0 0 0 0 0 0\ncpu1 5 0 0 45 0 0 0 0 0 0\nintr 1 2\n",
        );
        assert_eq!(t.len(), 3);
        assert_eq!(t[1].total, 50);
    }

    #[test]
    fn proc_stat_with_spaces_in_name() {
        let s = "1234 (Web Content) S 1 2 3 4 5 6 7 8 9 10 150 50 0 0 20 0 1 0";
        assert_eq!(parse_proc_stat(s), Some(("Web Content".into(), 200)));
        assert_eq!(parse_proc_stat("garbage"), None);
    }

    #[test]
    fn busiest_groups_by_name() {
        let mut a = HashMap::new();
        let mut b = HashMap::new();
        a.insert(1, ("php-fpm".to_string(), 100));
        b.insert(1, ("php-fpm".to_string(), 110));
        a.insert(2, ("php-fpm".to_string(), 50));
        b.insert(2, ("php-fpm".to_string(), 60));
        a.insert(3, ("java".to_string(), 0));
        b.insert(3, ("java".to_string(), 5));
        b.insert(4, ("new".to_string(), 99)); // started between scans: no baseline
        // 4 cores, 400 ticks total → 100 ticks per core
        let top = busiest(&a, &b, 400, 4, 3);
        assert_eq!(
            top[0],
            Usage {
                name: "php-fpm".into(),
                value: 20.0
            }
        );
        assert_eq!(
            top[1],
            Usage {
                name: "java".into(),
                value: 5.0
            }
        );
        assert_eq!(top.len(), 2);
        assert!(busiest(&a, &b, 0, 4, 3).is_empty());
    }

    #[test]
    fn memory_breakdown_from_pi() {
        let m = parse_meminfo_kb(
            "MemTotal: 8255792 kB\nMemFree: 5205856 kB\nMemAvailable: 6922992 kB\nBuffers: 161456 kB\nCached: 1604096 kB\nAnonPages: 972896 kB\nSlab: 137232 kB\nPageTables: 20160 kB\nKernelStack: 9328 kB\n",
        );
        let d = breakdown(&m).unwrap();
        assert_eq!(d.apps, 972896 * 1024);
        assert_eq!(d.kernel, (137232 + 20160 + 9328) * 1024);
        assert_eq!(d.cache, (1604096 + 161456) * 1024);
        assert_eq!(d.free, d.total - d.apps - d.kernel - d.cache);
    }

    #[test]
    fn swaps_and_zram() {
        let t = "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n/dev/zram0                              partition\t2097136\t\t1024\t\t100\n";
        let s = parse_swaps(t, |d| (d == "zram0").then(|| "zstd".to_string())).unwrap();
        assert_eq!(
            (s.total, s.used, s.kind.as_str()),
            (2097136 * 1024, 1024 * 1024, "zram · zstd")
        );
        assert_eq!(selected_algo("lzo-rle lzo lz4 [zstd]"), Some("zstd".into()));
        assert!(parse_swaps("Filename Type Size Used Priority\n", |_| None).is_none());
        let f = parse_swaps("h\n/swapfile file 1000 0 -2\n", |_| None).unwrap();
        assert_eq!(f.kind, "file");
    }

    #[test]
    fn mounts_and_disks() {
        let m = "/dev/nvme0n1p2 / ext4 rw 0 0\nproc /proc proc rw 0 0\n/dev/nvme0n1p1 /boot/firmware vfat rw 0 0\noverlay /var/lib/docker/x overlay rw 0 0\n/dev/nvme0n1p2 /var/lib/docker ext4 rw 0 0\n";
        assert_eq!(
            real_mounts(m),
            [
                ("/dev/nvme0n1p2".into(), "/".into()),
                ("/dev/nvme0n1p1".into(), "/boot/firmware".into())
            ]
        );
        assert_eq!(parent_disk("/dev/nvme0n1p2"), "nvme0n1");
        assert_eq!(parent_disk("mmcblk0p1"), "mmcblk0");
        assert_eq!(parent_disk("sda2"), "sda");
        assert_eq!(parent_disk("nvme0n1"), "nvme0n1");
        let ds = " 259       0 nvme0n1 100 0 4000 50 200 0 8000 90 0 0 0\n 259       1 nvme0n1p1 1 0 2 0 0 0 0 0 0 0 0\n";
        assert_eq!(diskstats(ds, "nvme0n1"), Some((4000, 8000)));
        assert_eq!(diskstats(ds, "sda"), None);
    }

    #[test]
    fn docker_df() {
        let body = r#"{"LayersSize":11430000000,"Images":[{"Size":1000,"Containers":1},{"Size":500,"Containers":0}],"Containers":[{"SizeRw":30},{"SizeRw":4}],"Volumes":[{"UsageData":{"Size":45}},{"UsageData":{"Size":-1}}]}"#;
        let d = parse_docker_df(body).unwrap();
        assert_eq!(
            (
                d.images,
                d.images_bytes,
                d.reclaimable,
                d.containers,
                d.containers_bytes,
                d.volumes,
                d.volumes_bytes
            ),
            (2, 11430000000, 500, 2, 34, 2, 45)
        );
        assert!(parse_docker_df("not json").is_err());
        let shared = r#"{"LayersSize":100,"Images":[{"Size":80,"Containers":0},{"Size":80,"Containers":0}]}"#;
        assert_eq!(parse_docker_df(shared).unwrap().reclaimable, 100);
    }

    #[test]
    fn sensor_names() {
        assert_eq!(sensor_name("cpu_thermal"), Some(("CPU", 0)));
        assert_eq!(sensor_name("ads7846"), None);
        assert_eq!(sensor_name("rpi_volt"), None);
    }
}
