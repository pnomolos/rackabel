//! `rackabel dev status` — daemon + per-extension state (DESIGN §2, §3).
//!
//! OWNED BY THE DAEMON-CORE AGENT. A thin socket client: resolves the per-Live daemon,
//! asks it for a `Status` snapshot, and renders the host state, the resolved Live/host
//! paths, the inspector state/port, the last-reload + rolling-p50 reload metrics, and a
//! per-extension table (incl. `Skipped:` pre-filter rows). `--json` emits the raw
//! snapshot for scripting (§7). With no daemon up it is a clean `RK0309` (exit 3).

use std::path::Path;

use crate::context::Ctx;
use crate::dev::ipc::{self, InspectorState, Request, Response};
use crate::dev::{Inspect, resolve, sock_path};
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::ui;

pub fn run(ctx: &Ctx) -> CmdResult<()> {
    let target = resolve::resolve(ctx)?;
    let sock = sock_path(ctx, target.app());
    let mut client = ipc::Client::connect(&sock)?;
    let resp = client.call(Request::Status)?;
    match resp {
        Response::Status {
            host,
            extensions,
            live_app,
            host_module,
            eh_node,
            dev_mode,
            inspector,
            reload_ms_last,
            reload_ms_p50,
        } => {
            if ctx.json {
                let snapshot = serde_json::json!({
                    "host": host,
                    "extensions": extensions,
                    "live_app": live_app,
                    "host_module": host_module,
                    "eh_node": eh_node,
                    "dev_mode": dev_mode,
                    "inspector": inspector,
                    "reload_ms_last": reload_ms_last,
                    "reload_ms_p50": reload_ms_p50,
                });
                println!("{}", serde_json::to_string_pretty(&snapshot).unwrap());
            } else {
                render(
                    &host,
                    &extensions,
                    &live_app,
                    &host_module,
                    &eh_node,
                    dev_mode,
                    inspector.as_ref(),
                    reload_ms_last,
                    reload_ms_p50,
                    ctx,
                );
            }
            Ok(())
        }
        Response::Error { code, msg } => Err(status_error(code, msg)),
        other => Err(RkError::of(
            ErrorCode::ProtocolMismatch,
            format!("unexpected dev host reply to status: {other:?}"),
            "restart the dev host: `rackabel dev stop && rackabel dev`",
        )),
    }
}

/// Ask the running daemon to (re)apply an inspector setting, restarting the host with
/// `--inspect` when toggling on a running host (§7). Used by `dev start --inspect`.
pub(crate) fn apply_inspect(ctx: &Ctx, app: &Path, ins: &Inspect) -> CmdResult<()> {
    let sock = sock_path(ctx, app);
    let mut client = ipc::Client::connect(&sock)?;
    let resp = client.call(Request::SetInspect {
        enable: true,
        host: ins.host.clone(),
        port: ins.port,
    })?;
    if ctx.echo_on()
        && let Response::Ack { restarted, .. } = &resp
        && *restarted == Some(true)
    {
        println!(
            "  restarting host with --inspect on {}:{}",
            ins.host, ins.port
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn render(
    host: &crate::dev::HostState,
    extensions: &[crate::dev::ExtStatus],
    live_app: &str,
    host_module: &str,
    eh_node: &str,
    dev_mode: bool,
    inspector: Option<&InspectorState>,
    reload_ms_last: Option<u64>,
    reload_ms_p50: Option<u64>,
    ctx: &Ctx,
) {
    use crate::dev::HostState;
    let (sym, line) = match host {
        HostState::Running {
            pid, api_version, ..
        } => (
            ui::Symbol::Good,
            format!("dev host running (pid {pid}, host API {api_version})"),
        ),
        HostState::Starting => (ui::Symbol::Warn, "dev host starting…".to_string()),
        HostState::Reloading => (ui::Symbol::Warn, "dev host reloading…".to_string()),
        HostState::Crashed { code, .. } => {
            (ui::Symbol::Bad, format!("dev host crashed (code {code:?})"))
        }
        HostState::CrashLooping { attempts } => (
            ui::Symbol::Bad,
            format!("dev host crash-looping after {attempts} attempts — run `rackabel dev reload`"),
        ),
        HostState::Stopped => (ui::Symbol::Warn, "dev host stopped".to_string()),
    };
    ui::frame::emit(sym, &line, ctx);

    println!("  Live: {live_app}");
    println!("  host module: {host_module}");
    println!("  node: {eh_node}");
    println!(
        "  Developer Mode: {}",
        if dev_mode { "on" } else { "off (inferred)" }
    );
    match inspector {
        Some(i) if i.active => println!("  inspector: on {}:{}", i.host, i.port),
        Some(i) => println!(
            "  inspector: requested {}:{} (applies next start)",
            i.host, i.port
        ),
        None => println!("  inspector: off"),
    }
    match (reload_ms_last, reload_ms_p50) {
        (Some(last), Some(p50)) => println!("  last reload: {last}ms (p50 {p50}ms)"),
        (Some(last), None) => println!("  last reload: {last}ms"),
        _ => {}
    }

    if extensions.is_empty() {
        println!("  (no enabled extensions — `rackabel dev register <path>` to add one)");
        return;
    }
    println!("  extensions:");
    for e in extensions {
        let state = match e.lifecycle {
            crate::dev::Lifecycle::Loaded => "loaded".to_string(),
            crate::dev::Lifecycle::Skipped => format!(
                "skipped ({})",
                e.skip_reason.as_deref().unwrap_or("incompatible")
            ),
            crate::dev::Lifecycle::Failed => {
                format!(
                    "failed ({})",
                    e.error.as_deref().unwrap_or("activate threw")
                )
            }
            crate::dev::Lifecycle::Deployed => "deployed".to_string(),
            crate::dev::Lifecycle::Registered => "registered".to_string(),
        };
        let enabled = if e.enabled { "" } else { " (disabled)" };
        println!("    - {} — {state}{enabled}", e.name);
    }
}

/// Map a daemon `Error` response into a framed error (preserving its code where known).
fn status_error(code: String, msg: String) -> RkError {
    let ec = ErrorCode::from_str(&code).unwrap_or(ErrorCode::NoDaemon);
    RkError::of(
        ec,
        msg,
        "run `rackabel dev status` again, or restart the dev host",
    )
}
