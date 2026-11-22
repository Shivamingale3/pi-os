//! Ctrl+C / SIGTERM → a flag the loops check, so the agent and the screen stop cleanly.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

pub fn install() {
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

pub fn stopping() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// Sleeps until `deadline`, waking early on Ctrl+C / SIGTERM.
pub fn sleep_until(deadline: Instant) {
    while !stopping() {
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        std::thread::sleep((deadline - now).min(Duration::from_millis(200)));
    }
}
