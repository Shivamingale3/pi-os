//! PiOS: monitoring for the Pebble Raspberry Pi 5.
//!
//!   pios snapshot   collect one snapshot (1 s apart from the baseline) and print it as JSON
//!   pios version

mod collect;
mod error;
mod parse;
mod types;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use error::Result;

fn docker_socket() -> PathBuf {
    std::env::var_os("PIOS_DOCKER_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/run/docker.sock"))
}

fn snapshot() -> Result<()> {
    let mut c = collect::Collector::new(docker_socket())?;
    std::thread::sleep(Duration::from_secs(1));
    let s = c.collect()?;
    println!("{}", serde_json::to_string_pretty(&s)?);
    Ok(())
}

fn main() -> ExitCode {
    let cmd = std::env::args().nth(1);
    let result = match cmd.as_deref() {
        Some("snapshot") => snapshot(),
        Some("version" | "--version") => {
            println!("pios {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("usage: pios <snapshot|version>");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pios: {e}");
            ExitCode::FAILURE
        }
    }
}
