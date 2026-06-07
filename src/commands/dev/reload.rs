//! `rackabel dev reload` — force a whole-host reload now (DESIGN §2, §3.3).
//!
//! A thin IPC client over the daemon's control socket (SPEC D §2). It sends a
//! `Reload { only, strict }` and turns the `ReloadResult` into the §7 exit contract:
//!   - `0` — the host re-initialized and every targeted extension `Loaded`.
//!   - `1` (`RK1306`) — any targeted extension threw in `activate()`.
//!   - `3` (`RK0309`) — no daemon is up to reload (or the connection dropped).
//!   - `1` (`RK4006`) — `--strict` and an extension was pre-filtered (host-
//!     incompatible `minimumApiVersion`); without `--strict` a skip is reported but the
//!     exit stays `0`.
//!
//! Pre-filtered (skipped) extensions are always reported on stderr (and in `--json`);
//! the minimumApiVersion pre-filter ITSELF lives in `crate::dev::registry::prefilter`
//! (the daemon applies it before building the `initialize()` array, SPEC H §6) — this
//! command only surfaces the daemon's reported skips.

use serde_json::json;

use crate::cli::DevReloadArgs;
use crate::context::Ctx;
use crate::dev::ipc::{Request, Response};
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::ui;

pub fn run(args: &DevReloadArgs, ctx: &Ctx) -> CmdResult<()> {
    let only = if args.names.is_empty() {
        None
    } else {
        Some(args.names.clone())
    };

    let mut client = super::connect_daemon(ctx)?;
    let resp = client.call(Request::Reload {
        only,
        strict: args.strict,
    })?;

    let (ok, reloaded, failed, skipped, reload_ms) = match resp {
        Response::ReloadResult {
            ok,
            reloaded,
            failed,
            skipped,
            reload_ms,
            ..
        } => (ok, reloaded, failed, skipped, reload_ms),
        Response::Error { code, msg } => {
            return Err(daemon_error(&code, &msg));
        }
        other => {
            return Err(RkError::of(
                ErrorCode::ProtocolMismatch,
                "the dev host returned an unexpected reply to reload",
                "restart the dev host: `rackabel dev stop && rackabel dev`",
            )
            .at(format!("{other:?}")));
        }
    };

    if ctx.json {
        let out = json!({
            "ok": ok,
            "reloaded": reloaded,
            "failed": failed.iter().map(|f| json!({"name": f.name, "error": f.error})).collect::<Vec<_>>(),
            "skipped": skipped.iter().map(|s| json!({"name": s.name, "reason": s.reason})).collect::<Vec<_>>(),
            "reload_ms": reload_ms,
        });
        println!("{}", serde_json::to_string_pretty(&out).expect("json"));
    } else {
        // Will this run exit non-zero? An activate() failure, or a strict skip, is
        // fatal — do NOT print a green `[✓] host reloaded` success line above the
        // failure frame (findings #5/#8). In the strict-skip short-circuit the daemon
        // returns `reloaded: []` and no reload actually happened, so a success symbol
        // would be doubly wrong.
        let will_fail = !failed.is_empty() || (args.strict && !skipped.is_empty());
        if !will_fail {
            if !reloaded.is_empty() {
                ui::frame::emit(
                    ui::frame::Symbol::Good,
                    &format!("reloaded {} ({reload_ms} ms)", reloaded.join(", ")),
                    ctx,
                );
            } else {
                ui::frame::emit(
                    ui::frame::Symbol::Good,
                    &format!("host reloaded ({reload_ms} ms)"),
                    ctx,
                );
            }
        }
        // Skips and failures go to stderr so a `--json`-less pipe of stdout stays clean.
        for s in &skipped {
            eprintln!("Skipped: {} ({})", s.name, s.reason);
        }
        for f in &failed {
            eprintln!("Failed: {} — {}", f.name, f.error);
        }
    }

    // Exit-code contract (§7). Failures (activate throws) dominate; then strict skips;
    // then success.
    // Under `--json` the reload-result object (printed above) is the authoritative
    // machine output — its `ok`/`failed`/`skipped` arrays carry the cause — so the
    // exit-coded errors below mark `json_handled` to suppress a second object in `main`.
    let handled = |e: RkError| if ctx.json { e.json_handled() } else { e };
    if !failed.is_empty() {
        let detail = failed
            .iter()
            .map(|f| format!("{}: {}", f.name, f.error))
            .collect::<Vec<_>>()
            .join("; ");
        return Err(handled(
            RkError::of(
                ErrorCode::ReloadActivateFailed,
                "an extension threw in activate() on reload",
                "fix the error above (see `rackabel dev logs <name>`), then reload again",
            )
            .at(detail),
        ));
    }
    if args.strict && !skipped.is_empty() {
        let detail = skipped
            .iter()
            .map(|s| format!("{}: {}", s.name, s.reason))
            .collect::<Vec<_>>()
            .join("; ");
        // §7: `--strict` treats a skip as a *strict failure* — exit 1, not the bare
        // RK4006 validation class (4). We keep the RK4006 code for the explain entry
        // but force the build/runtime exit class so `--strict` is the CI-fatal toggle
        // it is documented to be.
        return Err(handled(
            RkError::new(
                ErrorCode::SkippedIncompatible,
                crate::error::ExitClass::BuildRuntime,
                "an extension was skipped as host-incompatible (--strict)",
                "lower the extension's minimumApiVersion or drop --strict to reload the rest",
            )
            .at(detail),
        ));
    }
    Ok(())
}

/// Re-frame a daemon-side `Error` response into the matching local exit class. An
/// unknown code degrades to `RK0309` (the host is in a bad state — restart it).
fn daemon_error(code: &str, msg: &str) -> RkError {
    let ec = ErrorCode::from_str(code).unwrap_or(ErrorCode::NoDaemon);
    RkError::of(
        ec,
        msg.to_string(),
        "see `rackabel dev status`; restart with `rackabel dev stop && rackabel dev` if needed",
    )
}
