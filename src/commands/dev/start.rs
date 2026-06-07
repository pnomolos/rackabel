//! `rackabel dev start` — launch the managed Extension Host (DESIGN §2, §3.1).
//!
//! OWNED BY THE DAEMON-CORE AGENT. Daemonized by default (re-exec the hidden `__daemon`
//! subcommand, which `setsid`s + supervises the host child); `--foreground` keeps the
//! host tied to this shell (the CI escape hatch, §3.1); `--inspect` attaches the Node
//! debugger with the §7 restart-with-announcement semantics; `--emit-launch-config`
//! writes a VS Code `launch.json` for attaching the debugger.

use crate::cli::DevStartArgs;
use crate::context::Ctx;
use crate::dev::{Inspect, daemon, launch_config, preflight};
use crate::error::CmdResult;
use crate::ui;

pub fn run(args: &DevStartArgs, ctx: &Ctx) -> CmdResult<()> {
    let inspect = parse_inspect(args.inspect.as_deref())?;

    // --emit-launch-config writes the debugger config and continues (DESIGN §7).
    if args.emit_launch_config {
        let endpoint = inspect.clone().unwrap_or_else(Inspect::default_endpoint);
        launch_config::emit(&endpoint, ctx)?;
    }

    if args.foreground {
        // Foreground supervises in-process (preflight runs inside).
        return daemon::run_foreground(ctx, inspect);
    }

    // Daemonized path: preflight first (block-and-wait / RK0306 under --no-input), then
    // re-exec the detached daemon and wait for it to come up.
    preflight::ensure_ready(ctx)?;
    let target = daemon::start(ctx)?;

    // If --inspect was requested, ask the (now-running) daemon to restart with it
    // enabled, announcing what it did (§7 restart-with-announcement).
    if let Some(ins) = inspect {
        super::status::apply_inspect(ctx, target.app(), &ins)?;
    }

    if ctx.echo_on() {
        ui::frame::emit(
            ui::Symbol::Good,
            &format!("dev host running ({})", target.app().display()),
            ctx,
        );
    }
    Ok(())
}

/// Parse the `--inspect[=host:port]` flag: `None` when absent, the default endpoint when
/// given with no value, or the parsed `host:port`.
pub(crate) fn parse_inspect(raw: Option<&str>) -> CmdResult<Option<Inspect>> {
    match raw {
        None => Ok(None),
        Some(s) => Inspect::parse(s).map(Some).map_err(|msg| {
            crate::error::RkError::new(
                crate::error::ErrorCode::UsageError,
                crate::error::ExitClass::Usage,
                msg,
                "use --inspect, --inspect=PORT, or --inspect=HOST:PORT",
            )
        }),
    }
}
