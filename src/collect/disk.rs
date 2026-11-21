//! Disk use of "/" ← statvfs(2), same formula as `df`.

use std::ffi::CString;

use crate::error::{Result, err};
use crate::types::Disk;

pub fn compute(blocks: u64, bfree: u64, bavail: u64, frsize: u64) -> Disk {
    let used = blocks.saturating_sub(bfree);
    let denom = used + bavail;
    Disk {
        used_percent: if denom == 0 {
            0.0
        } else {
            used as f64 * 100.0 / denom as f64
        },
        used_bytes: used * frsize,
        total_bytes: blocks * frsize,
    }
}

pub fn read(path: &str) -> Result<Disk> {
    let c_path = CString::new(path).map_err(|_| err(format!("bad path {path:?}")))?;
    // SAFETY: statvfs only writes into the struct we pass; the path is a valid C string.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut s) };
    if rc != 0 {
        return Err(err(format!(
            "statvfs {path}: {}",
            std::io::Error::last_os_error()
        )));
    }
    #[allow(clippy::unnecessary_cast)] // field widths differ between targets
    Ok(compute(
        s.f_blocks as u64,
        s.f_bfree as u64,
        s.f_bavail as u64,
        s.f_frsize as u64,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_df() {
        // df: 332186648 1K-blocks, 65479700 used, 249759544 avail → 21 %
        let d = compute(332186648, 332186648 - 65479700, 249759544, 1024);
        assert!((d.used_percent - 20.77).abs() < 0.01, "{}", d.used_percent);
    }

    #[test]
    fn empty_fs_is_zero() {
        assert_eq!(compute(0, 0, 0, 4096).used_percent, 0.0);
    }

    #[test]
    fn real_root_works() {
        let d = read("/").unwrap();
        assert!(d.total_bytes > 0);
    }
}
