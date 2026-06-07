//! `rackabel dev list` (alias `ls`) — show the registry with status (DESIGN §2, §3.2).
//!
//! Reads `~/.rackabel/registry.toml` and prints one row per entry with an enabled/
//! disabled status column and the project path. Works with a DEAD daemon (§3.2): it
//! never touches the host or the socket, so the listing is always available. `--json`
//! emits the same rows as a machine-readable array (the stable shape downstream tools
//! key off).

use serde_json::json;

use crate::context::Ctx;
use crate::dev::registry::Registry;
use crate::error::CmdResult;
use crate::ui;

pub fn run(ctx: &Ctx) -> CmdResult<()> {
    let reg = Registry::load(ctx)?;
    let entries = reg.entries();

    if ctx.json {
        let rows: Vec<_> = entries
            .iter()
            .map(|e| {
                json!({
                    "name": e.name,
                    "path": e.path.display().to_string(),
                    "enabled": e.enabled,
                    "status": if e.enabled { "enabled" } else { "disabled" },
                })
            })
            .collect();
        let out = json!({ "extensions": rows });
        println!("{}", serde_json::to_string_pretty(&out).expect("json"));
        return Ok(());
    }

    if entries.is_empty() {
        ui::frame::note(
            "no extensions registered — `rackabel dev register [PATH]` to add one",
            ctx,
        );
        return Ok(());
    }

    // Plain aligned columns: STATUS  NAME  PATH.
    let name_w = entries
        .iter()
        .map(|e| e.name.len())
        .max()
        .unwrap_or(4)
        .max(4);
    for e in entries {
        let (sym, status) = if e.enabled {
            (ui::frame::Symbol::Good, "enabled")
        } else {
            (ui::frame::Symbol::Warn, "disabled")
        };
        ui::frame::emit(
            sym,
            &format!(
                "{:<name_w$}  {:<8}  {}",
                e.name,
                status,
                e.path.display(),
                name_w = name_w
            ),
            ctx,
        );
    }
    Ok(())
}
