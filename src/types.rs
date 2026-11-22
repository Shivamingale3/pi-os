//! The Snapshot: everything collected in one round. Served live by `/now` and stored
//! (as JSON) with each incident, and read back by the UI. Field names are camelCase in JSON.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    pub hostname: String,
    pub uptime_sec: f64,
    /// 1, 5, 15 min
    pub load: [f64; 3],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cpu {
    /// 0–100, busy share since the previous snapshot
    pub percent: f64,
    /// None when there is no sensor
    pub temp_c: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Memory {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Disk {
    /// of "/", same formula as `df`
    pub used_percent: f64,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NetIface {
    pub name: String,
    pub rx_bytes_per_sec: f64,
    pub tx_bytes_per_sec: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Network {
    /// real interfaces only: eth*, en*, wl*, tailscale*
    pub interfaces: Vec<NetIface>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Container {
    /// Docker's full container ID (empty in snapshots stored before v0.4)
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// Docker's State: "running", "exited", "restarting", …
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Docker {
    pub running: u32,
    pub total: u32,
    pub containers: Vec<Container>,
}

/// A host service from /etc/pios/checks (`host NAME`), running or not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Unit {
    pub name: String,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Units {
    pub running: u32,
    pub total: u32,
    pub units: Vec<Unit>,
}

/// One public hostname from /etc/pios/checks (`web HOST CODE`), as `pios web` last saw it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebCheck {
    pub host: String,
    pub expect: u16,
    /// None: no answer (timeout, DNS, TLS…)
    pub code: Option<u16>,
    pub ms: Option<u32>,
    pub error: Option<String>,
    pub ok: bool,
    /// failed checks in a row (an incident opens at 2)
    pub fails: u32,
}

impl WebCheck {
    /// No answer or a server error is down; another unexpected code needs attention.
    pub fn down(&self) -> bool {
        self.code.is_none_or(|c| c >= 500)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Web {
    pub ok: u32,
    pub total: u32,
    /// ms since the Unix epoch
    pub checked_at: i64,
    pub every_sec: u32,
    pub checks: Vec<WebCheck>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// None when no `host` lines are set up
    #[serde(default)]
    pub units: Option<Units>,
    /// None when no `web` lines are set up, or `pios web` hasn't reported for 3 minutes
    #[serde(default)]
    pub web: Option<Web>,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
