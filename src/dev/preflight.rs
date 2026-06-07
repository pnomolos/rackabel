//! The dev-loop preflight gates (DESIGN §3.6 / §6.2, SPEC D §7).
//!
//! OWNED BY THE DAEMON-CORE AGENT. Before the daemon launches a host it must confirm
//! the environment the loop needs: (1) the Live *app* is running, and (2) Developer
//! Mode is on / the host is reachable. Neither is statically readable (Preferences.cfg
//! is binary — `dev_mode_detection`), so detection is BEHAVIORAL: Live running can be
//! seen with `pgrep`; Dev-Mode-on is inferred as "Live running + no Live-spawned host"
//! (with Dev Mode off Live auto-spawns its OWN host as a child of Live).
//!
//! Interactive runs BLOCK-AND-WAIT, polling every 1s and continuing the moment the
//! condition flips (the §6.2 "I'll continue automatically" promise). Under `--no-input`
//! neither blocks: each becomes a deterministic `exit 3` frame (`RK0303`/`RK0306`), so a
//! CI run never hangs (§7 `--no-input`).

use std::io::IsTerminal;
use std::time::Duration;

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::ui;

/// How often the block-and-wait gates re-check (DESIGN §3.6: "poll every 1s").
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Run the dev-loop preflight: Live-running, then Dev-Mode/host-reachable. Returns once
/// both pass (after blocking-and-waiting if interactive), or a framed `exit 3` error
/// under `--no-input` / a non-TTY when a gate is not satisfied.
///
/// The host-reachability half (the actual connect) is proven by the host launch itself
/// (SPEC H §4 — a stuck host that never greets is the slot-taken/Dev-Mode-off signal),
/// so this preflight covers the two cheap behavioral checks that produce a clear
/// navigational message rather than a launch timeout.
pub fn ensure_ready(ctx: &Ctx) -> CmdResult<()> {
    gate_live_running(ctx)?;
    gate_dev_mode(ctx)?;
    Ok(())
}

/// (1) Live app running. If not: block-and-wait (interactive) or `RK0303` (no-input).
fn gate_live_running(ctx: &Ctx) -> CmdResult<()> {
    if is_live_running() {
        return Ok(());
    }
    if !can_block(ctx) {
        return Err(live_not_running_error());
    }
    if ctx.echo_on() {
        ui::frame::emit(
            ui::Symbol::Warn,
            "Ableton Live doesn't appear to be running",
            ctx,
        );
        ui::frame::note(
            "open Ableton Live (the app) and leave it running — the dev loop\n\
             connects to it. I'll continue automatically once it's up.",
            ctx,
        );
        println!("  waiting for Live…   (ctrl-c to stop)");
    }
    loop {
        std::thread::sleep(POLL_INTERVAL);
        if is_live_running() {
            return Ok(());
        }
    }
}

/// (2) Developer Mode on / host reachable. Inferred behaviorally: Live running + NO
/// Live-spawned host ⇒ Dev Mode ON. If a host is present that rackabel does not own, we
/// treat it as Live-spawned (Dev Mode likely off) and block/error.
fn gate_dev_mode(ctx: &Ctx) -> CmdResult<()> {
    if dev_mode_on() {
        return Ok(());
    }
    if !can_block(ctx) {
        return Err(dev_mode_off_error());
    }
    if ctx.echo_on() {
        ui::frame::emit(
            ui::Symbol::Warn,
            "Developer Mode is OFF — the dev loop can't run without it",
            ctx,
        );
        ui::frame::note(
            "open Live → Settings/Preferences → Extensions → enable Developer Mode\n\
             (it appears once you've joined the Extensions beta), then this command\n\
             continues automatically.",
            ctx,
        );
        println!("  waiting for Developer Mode…   (ctrl-c to stop)");
    }
    loop {
        std::thread::sleep(POLL_INTERVAL);
        if dev_mode_on() {
            return Ok(());
        }
    }
}

/// Whether we may block-and-wait: only on an interactive TTY without `--no-input`.
/// `--no-input` or a non-TTY → deterministic error (never hang) per §7.
fn can_block(ctx: &Ctx) -> bool {
    !ctx.no_input && std::io::stdin().is_terminal()
}

/// Whether the Ableton Live app appears to be running. Behavioral + a deterministic test
/// seam (`RACKABEL_DOCTOR_LIVE_RUNNING`=`0`/`1`, shared with doctor).
pub fn is_live_running() -> bool {
    if let Some(v) = test_bool_env("RACKABEL_DOCTOR_LIVE_RUNNING") {
        return v;
    }
    if !cfg!(target_os = "macos") {
        return false;
    }
    pgrep_matches("Ableton Live")
}

/// Whether an Extension Host process (any) is running. Test seam:
/// `RACKABEL_DOCTOR_BARE_HOST`=`0`/`1` (shared with doctor).
pub fn is_host_running() -> bool {
    if let Some(v) = test_bool_env("RACKABEL_DOCTOR_BARE_HOST") {
        return v;
    }
    if !cfg!(target_os = "macos") {
        return false;
    }
    pgrep_matches("ExtensionHostNodeModule.node")
}

/// Behavioral Dev-Mode inference (dev_mode_detection): Live running + no Live-spawned
/// host ⇒ Dev Mode ON. A separate seam (`RACKABEL_DEV_MODE`=`0`/`1`) pins it in tests
/// where the host-process distinction can't be faked.
pub fn dev_mode_on() -> bool {
    if let Some(v) = test_bool_env("RACKABEL_DEV_MODE") {
        return v;
    }
    // With Dev Mode OFF, Live auto-spawns its own host as a child of Live. We can't
    // cheaply read PPIDs here without a heavier ps parse, so the conservative behavioral
    // rule is: Live up + no host process ⇒ Dev Mode on (rackabel hasn't started one
    // yet at preflight time). If a host is already present it's foreign (Live-spawned or
    // a stray dev host) and the daemon's own launch will surface the slot-taken case.
    is_live_running() && !is_host_running()
}

/// The `RK0303`-style Live-not-running error (environment, exit 3). DESIGN §6.2.
fn live_not_running_error() -> RkError {
    RkError::of(
        ErrorCode::NoLiveInstall,
        "Ableton Live doesn't appear to be running",
        "open Ableton Live (the app) and leave it running — the dev loop connects to\n\
         it, then rerun `rackabel dev`. (Running with --no-input, so I won't wait.)",
    )
}

/// The `RK0306` Developer-Mode-off error (environment, exit 3). DESIGN §3.6/§6.2.
fn dev_mode_off_error() -> RkError {
    RkError::of(
        ErrorCode::DeveloperModeOff,
        "Developer Mode is OFF — the dev loop cannot run without it",
        "open Live → Settings/Preferences → Extensions → enable Developer Mode\n\
         (it appears once you've joined the Extensions beta), then rerun `rackabel dev`.\n\
         (Running with --no-input, so I won't wait for the toggle.)",
    )
}

/// Read a `0`/`1` boolean probe-override env var. `None` if unset.
fn test_bool_env(key: &str) -> Option<bool> {
    match std::env::var(key).ok()?.as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// Best-effort `pgrep -fl <needle>`: true if any process command line matches.
fn pgrep_matches(needle: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-fl", needle])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(no_input: bool) -> Ctx {
        Ctx {
            no_input,
            json: false,
            quiet: false,
            verbose: false,
            raw: false,
            color: crate::ui::color::ColorMode::Never,
            color_err: crate::ui::color::ColorMode::Never,
            cwd: std::path::PathBuf::from("/"),
            rackabel_home: std::path::PathBuf::from("/tmp/.rackabel"),
            home: std::path::PathBuf::from("/tmp"),
            ableton_app: None,
            ableton_user_library: None,
            ableton_eh_mod: None,
            ableton_eh_node: None,
            ableton_extensions_dir: None,
            ableton_storage_base: None,
            rackabel_host_cmd: None,
        }
    }

    // These tests mutate process-global env (the seams), so they share a guard to avoid
    // cross-test interference under the default parallel runner.
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn no_input_live_down_is_environment_error() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("RACKABEL_DOCTOR_LIVE_RUNNING", "0") };
        let err = ensure_ready(&ctx(true)).unwrap_err();
        assert_eq!(err.code, ErrorCode::NoLiveInstall);
        assert_eq!(err.class, crate::error::ExitClass::Environment);
        unsafe { std::env::remove_var("RACKABEL_DOCTOR_LIVE_RUNNING") };
    }

    #[test]
    fn no_input_dev_mode_off_is_environment_error() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("RACKABEL_DOCTOR_LIVE_RUNNING", "1");
            std::env::set_var("RACKABEL_DEV_MODE", "0");
        }
        let err = ensure_ready(&ctx(true)).unwrap_err();
        assert_eq!(err.code, ErrorCode::DeveloperModeOff);
        unsafe {
            std::env::remove_var("RACKABEL_DOCTOR_LIVE_RUNNING");
            std::env::remove_var("RACKABEL_DEV_MODE");
        }
    }

    #[test]
    fn ready_when_live_up_and_dev_mode_on() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::set_var("RACKABEL_DOCTOR_LIVE_RUNNING", "1");
            std::env::set_var("RACKABEL_DEV_MODE", "1");
        }
        assert!(ensure_ready(&ctx(true)).is_ok());
        unsafe {
            std::env::remove_var("RACKABEL_DOCTOR_LIVE_RUNNING");
            std::env::remove_var("RACKABEL_DEV_MODE");
        }
    }
}
