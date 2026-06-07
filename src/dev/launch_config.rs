//! Emitting a VS Code `launch.json` for attaching the Node debugger to the dev host
//! (DESIGN §7 `--emit-launch-config`, adapting the create-extension shape, §4.7).
//!
//! OWNED BY THE DAEMON-CORE AGENT (the `--inspect`/launch-config owner). `dev start
//! --emit-launch-config` / `dev --emit-launch-config` write `.vscode/launch.json` in the
//! project root with a single Node *attach* configuration pointed at the inspector
//! endpoint (default `127.0.0.1:9229`), then continue. The shape matches what
//! create-extension produces so an existing config is not surprised; rackabel never
//! requires hand-editing it.

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::ui;

use super::Inspect;

/// Write `.vscode/launch.json` in the cwd project (or the cwd if no project) for
/// attaching the debugger to `endpoint`. Best-effort: a missing project just uses cwd.
pub fn emit(endpoint: &Inspect, ctx: &Ctx) -> CmdResult<()> {
    let root = crate::manifest::Project::discover_cwd(ctx)
        .map(|p| p.root)
        .unwrap_or_else(|_| ctx.cwd.clone());
    let dir = root.join(".vscode");
    std::fs::create_dir_all(&dir).map_err(|e| {
        RkError::of(
            ErrorCode::UsageError,
            "could not create the .vscode directory",
            "check write permissions on the project, then retry",
        )
        .at(dir.display().to_string())
        .raw(e.into())
    })?;
    let path = dir.join("launch.json");
    let body = render(endpoint);
    std::fs::write(&path, body).map_err(|e| {
        RkError::of(
            ErrorCode::UsageError,
            "could not write launch.json",
            "check write permissions on the project, then retry",
        )
        .at(path.display().to_string())
        .raw(e.into())
    })?;
    if ctx.echo_on() {
        ui::frame::emit(
            ui::Symbol::Good,
            &format!("wrote {} (attach the debugger)", path.display()),
            ctx,
        );
    }
    Ok(())
}

/// Render the launch.json body for a Node attach config at `endpoint`.
fn render(endpoint: &Inspect) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "version": "0.2.0",
        "configurations": [
            {
                "type": "node",
                "request": "attach",
                "name": "Attach to rackabel dev host",
                "address": endpoint.host,
                "port": endpoint.port,
                "skipFiles": ["<node_internals>/**"],
                "continueOnAttach": true
            }
        ]
    }))
    .expect("launch.json serializes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_carries_endpoint() {
        let body = render(&Inspect {
            host: "127.0.0.1".into(),
            port: 9229,
        });
        assert!(body.contains("\"attach\""));
        assert!(body.contains("\"port\": 9229"));
        assert!(body.contains("127.0.0.1"));
    }
}
