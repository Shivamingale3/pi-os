//! Collects one Snapshot. Keeps the previous CPU and network counters between calls, so rates
//! cover the time since the last snapshot and no collector has to sleep.

pub mod cpu;
pub mod disk;
pub mod docker;
pub mod host;
pub mod memory;
pub mod network;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::error::Result;
use crate::types::{Cpu, Snapshot, now_ms};

const DOCKER_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Collector {
    prev_ticks: cpu::Ticks,
    prev_net: network::Counters,
    prev_at: Instant,
    temp_sensor: Option<PathBuf>,
    docker_socket: PathBuf,
}

impl Collector {
    /// Takes the baseline counters. The first `collect` should come about a second later.
    pub fn new(docker_socket: PathBuf) -> Result<Self> {
        Ok(Self {
            prev_ticks: cpu::read_ticks()?,
            prev_net: network::read_counters()?,
            prev_at: Instant::now(),
            temp_sensor: cpu::find_temp_sensor(),
            docker_socket,
        })
    }

    pub fn collect(&mut self) -> Result<Snapshot> {
        let ticks = cpu::read_ticks()?;
        let net = network::read_counters()?;
        let now = Instant::now();
        let secs = now.duration_since(self.prev_at).as_secs_f64();

        let percent = cpu::percent(self.prev_ticks, ticks);
        let network = network::rates(&self.prev_net, &net, secs);
        self.prev_ticks = ticks;
        self.prev_net = net;
        self.prev_at = now;

        Ok(Snapshot {
            ts: now_ms(),
            host: host::read()?,
            cpu: Cpu {
                percent,
                temp_c: cpu::read_temp(self.temp_sensor.as_deref()),
            },
            memory: memory::read()?,
            disk: disk::read("/")?,
            network,
            docker: docker::read(&self.docker_socket, DOCKER_TIMEOUT).ok(),
        })
    }
}
