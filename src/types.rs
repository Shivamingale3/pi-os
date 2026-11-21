//! The Snapshot: everything collected in one round. Served live by `/now` and stored
//! (as JSON) with each incident. Field names are camelCase in JSON.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub hostname: String,
    pub uptime_sec: f64,
    /// 1, 5, 15 min
    pub load: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Cpu {
    /// 0–100, busy share since the previous snapshot
    pub percent: f64,
    /// None when there is no sensor
    pub temp_c: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    /// of "/", same formula as `df`
    pub used_percent: f64,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetIface {
    pub name: String,
    pub rx_bytes_per_sec: f64,
    pub tx_bytes_per_sec: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    /// real interfaces only: eth*, en*, wl*, tailscale*
    pub interfaces: Vec<NetIface>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    pub name: String,
    /// Docker's State: "running", "exited", "restarting", …
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Docker {
    pub running: u32,
    pub total: u32,
    pub containers: Vec<Container>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// ms since the Unix epoch
    pub ts: i64,
    pub host: Host,
    pub cpu: Cpu,
    pub memory: Memory,
    pub disk: Disk,
    pub network: Network,
    /// None when Docker isn't reachable
    pub docker: Option<Docker>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
