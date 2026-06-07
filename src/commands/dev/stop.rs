//! `rackabel dev stop` — stop the daemon cleanly (DESIGN §2, §3.1).
//!
//! OWNED BY THE DAEMON-CORE AGENT. Sends `Shutdown` over the control socket; the daemon
//! killpg's the host group, unlinks its socket + pidfile, and exits. We then verify the
//! daemon PID is actually gone (the whole group down — no orphaned node host, the
//! verified orphan trap) before reporting success. With no daemon up it is a clean
//! `RK0309` (exit 3): nothing to stop.

use std::time::{Duration, Instant};

use nix::sys::signal::kill;
use nix::unistd::Pid;

use crate::context::Ctx;
use crate::dev::daemon;
use crate::dev::ipc::{self, Request};
use crate::dev::{pid_path, resolve, sock_path};
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::ui;

pub fn run(ctx: &Ctx) -> CmdResult<()> {
    let target = resolve::resolve(ctx)?;
    let app = target.app();
    let sock = sock_path(ctx, app);

    // Read the daemon PID first so we can verify it dies.
    let pid = daemon::read_pid(ctx, app);

    // No live daemon → RK0309 (nothing to stop), unless a stale pidfile/socket lingers,
    // which we just clean up and report.
    if !daemon::is_running(ctx, app) {
        let cleaned =
            std::fs::remove_file(&sock).is_ok() | std::fs::remove_file(pid_path(ctx, app)).is_ok();
        if cleaned && ctx.echo_on() {
            ui::frame::emit(ui::Symbol::Good, "cleaned up a stale dev host", ctx);
            return Ok(());
        }
        return Err(RkError::of(
            ErrorCode::NoDaemon,
            "no dev host is running",
            "nothing to stop; start one with `rackabel dev`",
        ));
    }

    // Ask the daemon to shut down.
    match ipc::Client::connect(&sock) {
        Ok(mut client) => {
            let _ = client.call(Request::Shutdown);
        }
        Err(_) => {
            // Socket gone but pid alive: fall through to the kill verification.
        }
    }

    // Verify the daemon (and thus its host group) is gone.
    if let Some(pid) = pid {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if !matches!(kill(Pid::from_raw(pid), None), Ok(())) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if matches!(kill(Pid::from_raw(pid), None), Ok(())) {
            // Graceful shutdown didn't land — escalate (killpg the daemon's group).
            let _ = nix::sys::signal::killpg(Pid::from_raw(pid), nix::sys::signal::Signal::SIGTERM);
            std::thread::sleep(Duration::from_millis(300));
            let _ = nix::sys::signal::killpg(Pid::from_raw(pid), nix::sys::signal::Signal::SIGKILL);
        }
    }

    // Clean up any lingering socket/pidfile (the daemon normally removes them).
    let _ = std::fs::remove_file(&sock);
    let _ = std::fs::remove_file(pid_path(ctx, app));

    if ctx.echo_on() {
        ui::frame::emit(ui::Symbol::Good, "dev host stopped", ctx);
    }
    Ok(())
}
