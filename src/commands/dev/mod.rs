//! The `rackabel dev` command group dispatch (DESIGN §2 dev table, §3).
//!
//! FOUNDATION-OWNED dispatch + the name-vs-verb routing + STUBS for every verb. The
//! five 0.3 agents fill their own command files (each listed below). The dispatch here
//! is frozen: it resolves the bare-`dev` vs verb split (clap already enforces "a verb
//! token wins"), routes `--only`/`-- <NAME…>` through the registry name matcher
//! (never the verb table, §3.3), and degrades cleanly on non-Unix.
//!
//! Per-command ownership (SPEC D §3):
//!   - start/stop/status  → DAEMON-CORE
//!   - watch + bare-dev    → WATCH-LOOP
//!   - register/unregister/enable/disable/list/reload → REGISTRY
//!   - logs                → LOGS
//!   - test                → DEV-TEST

pub mod disable;
pub mod enable;
pub mod list;
pub mod logs_cmd;
pub mod register;
pub mod reload;
pub mod start;
pub mod status;
pub mod stop;
pub mod test_cmd;
pub mod unregister;
pub mod watch_cmd;

use crate::cli::{DaemonArgs, DevArgs, DevCommand};
use crate::context::Ctx;
use crate::dev::ipc::Client;
use crate::error::{CmdResult, ErrorCode, RkError};

/// Dispatch `rackabel dev …`. A verb routes to its (stubbed) handler; the bare form
/// (no verb) routes to the watch loop via [`watch_cmd::run_bare`].
pub fn run(args: DevArgs, ctx: &Ctx) -> CmdResult<()> {
    match args.command {
        Some(DevCommand::Start(a)) => start::run(&a, ctx),
        Some(DevCommand::Stop) => stop::run(ctx),
        Some(DevCommand::Status) => status::run(ctx),
        Some(DevCommand::Register(a)) => register::run(&a, ctx),
        Some(DevCommand::Unregister(a)) => unregister::run(&a, ctx),
        Some(DevCommand::Enable(a)) => enable::run(&a, ctx),
        Some(DevCommand::Disable(a)) => disable::run(&a, ctx),
        Some(DevCommand::List) => list::run(ctx),
        Some(DevCommand::Watch(a)) => watch_cmd::run(&a, ctx),
        Some(DevCommand::Reload(a)) => reload::run(&a, ctx),
        Some(DevCommand::Logs(a)) => logs_cmd::run(&a, ctx),
        Some(DevCommand::Test(a)) => test_cmd::run(&a, ctx),
        // No verb → the flagship bare loop (start-if-needed + watch + tail).
        None => watch_cmd::run_bare(&args, ctx),
    }
}

/// Connect to a running dev-host daemon's control socket, or `RK0309` if none is up.
///
/// Registry-side verbs (`reload`) must work without first resolving Live (so the
/// no-daemon case is a clean `exit 3` even on a machine with no Live install — the
/// §6.2 / §7 contract). We therefore locate the daemon by scanning `~/.rackabel/daemon`
/// for `*.sock` files and connecting to the first one that answers, rather than going
/// through full Live resolution (which could fail with an unrelated `RK0303`). With
/// one daemon per Live install this is unambiguous in the common case; a future
/// multi-Live disambiguation can key off the resolved socket hash.
pub(crate) fn connect_daemon(ctx: &Ctx) -> CmdResult<Client> {
    let dir = crate::dev::daemon_dir(ctx);
    let mut last_err: Option<RkError> = None;
    if let Ok(read) = std::fs::read_dir(&dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("sock") {
                continue;
            }
            match Client::connect(&path) {
                Ok(client) => return Ok(client),
                Err(e) => last_err = Some(e),
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        RkError::of(
            ErrorCode::NoDaemon,
            "no dev host is running",
            "start it with `rackabel dev` (or `rackabel dev start`), then retry",
        )
        .at(dir.display().to_string())
    }))
}

/// The hidden `__daemon` re-exec target (DESIGN §3.1). Bridges the clap args to the
/// daemon-core entry. STUB until daemon-core lands.
pub fn run_daemon(args: DaemonArgs, ctx: &Ctx) -> CmdResult<()> {
    let params = crate::dev::daemon::DaemonParams {
        live_app: args.live,
        sock: args.sock,
        state_home: args.state,
    };
    crate::dev::daemon::run_daemon(params, ctx)
}
