//! `rk-fakehost` — the hermetic stand-in for the real Ableton Extension Host (SPEC D §6).
//!
//! The daemon-lifecycle integration tests point the daemon at this binary via the
//! `RACKABEL_HOST_CMD` seam (`HostConfig::host_cmd_override`), so no test ever launches
//! real Live or real node. It mimics the host's observable behavior (SPEC H §1/§2/§3):
//!
//!   - On launch it prints the synthetic startup banner + connected marker to stdout
//!     (`Started: Extension Host 1.0.0`, `FlipMessageStreamSocket send success`) and a
//!     per-extension liveness line (`info: [<ext>]: …`) for each `RK_FAKEHOST_EXT`
//!     name, then sleeps forever.
//!   - On SIGHUP it re-prints a reload banner + the liveness lines (the daemon should
//!     NOT rely on this — real reload is kill+respawn — but it lets a test assert the
//!     signal was observed if it ever sends one).
//!   - On SIGTERM it exits 0 (the bare-node host exits 143 under SIGTERM, but a clean
//!     0 keeps the fixture's own success path unambiguous for tests that wait on it).
//!   - `RK_FAKEHOST_CRASH=<ms>` makes it exit with code 1 after `<ms>` milliseconds —
//!     the crash-loop / crash-recovery fixture (SPEC D §6, SPEC H §6/§9).
//!   - `RK_FAKEHOST_HANG=1` makes it print the banner but NEVER the connected marker
//!     (the single-connection "stuck at Started" case, SPEC H §4) so a test can drive
//!     the connect-timeout path.
//!
//! It is intentionally dependency-free (no nix): signal handling uses raw libc via a
//! tiny `extern "C"` handler and an atomic flag, so the fixture builds fast and is not
//! coupled to the crate's daemon internals.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static GOT_HUP: AtomicBool = AtomicBool::new(false);
static GOT_TERM: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_signal(sig: i32) {
    const SIGHUP: i32 = 1;
    const SIGTERM: i32 = 15;
    if sig == SIGHUP {
        GOT_HUP.store(true, Ordering::SeqCst);
    } else if sig == SIGTERM {
        GOT_TERM.store(true, Ordering::SeqCst);
    }
}

#[cfg(unix)]
fn install_handlers() {
    // Cast through a fn pointer first to avoid the "function item into integer" lint,
    // then to libc's sighandler_t.
    let handler = on_signal as extern "C" fn(i32) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGHUP, handler);
        libc::signal(libc::SIGTERM, handler);
    }
}

#[cfg(not(unix))]
fn install_handlers() {}

fn now_iso() -> String {
    // A microsecond-ish timestamp in the host's `YYYY-MM-DDTHH:MM:SS.ffffff` shape is
    // not needed for the tests (they glob it), so emit a fixed-format placeholder that
    // still starts with a `<ts>:` token the host-log parser recognizes.
    "1970-01-01T00:00:00.000000".to_string()
}

fn print_banner() {
    println!(
        "{}: info: #######################################",
        now_iso()
    );
    println!("{}: info: Started: Extension Host 1.0.0", now_iso());
    println!(
        "{}: info: #######################################",
        now_iso()
    );
}

fn print_connected() {
    println!("{}: info: Extension Host sends greeting to Live", now_iso());
    println!("{}: info: FlipMessageStreamSocket send success", now_iso());
}

fn print_liveness(exts: &[String]) {
    for ext in exts {
        println!("{}: info: [{ext}]: {ext} active (fakehost)", now_iso());
    }
}

fn main() {
    install_handlers();

    // The extensions to advertise come from RK_FAKEHOST_EXT (comma-separated). If
    // unset, advertise a single default so a bare launch still emits one liveness line.
    let exts: Vec<String> = std::env::var("RK_FAKEHOST_EXT")
        .ok()
        .filter(|s| !s.is_empty())
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_else(|| vec!["fake-ext".to_string()]);

    print_banner();

    // Crash mode: exit non-zero after the configured delay (crash-loop fixture).
    if let Ok(ms) = std::env::var("RK_FAKEHOST_CRASH")
        && let Ok(ms) = ms.parse::<u64>()
    {
        std::thread::sleep(Duration::from_millis(ms));
        // Mimic an uncaughtException-style abort.
        eprintln!(
            "{}: error: Uncaught exception (uncaughtException)",
            now_iso()
        );
        eprintln!("{}: info: Process is exiting with code: 1", now_iso());
        std::process::exit(1);
    }

    // Hang mode: never print the connected marker (the stuck-at-Started case).
    let hang = std::env::var("RK_FAKEHOST_HANG")
        .map(|v| v == "1")
        .unwrap_or(false);
    if !hang {
        print_connected();
        print_liveness(&exts);
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();

    // Sleep until a signal arrives. On SIGTERM exit 0; on SIGHUP re-emit the reload
    // banner + liveness and keep running.
    loop {
        std::thread::sleep(Duration::from_millis(20));
        if GOT_TERM.load(Ordering::SeqCst) {
            std::process::exit(0);
        }
        if GOT_HUP.swap(false, Ordering::SeqCst) {
            println!("{}: info: reload requested (fakehost)", now_iso());
            print_liveness(&exts);
            let _ = std::io::stdout().flush();
        }
    }
}
