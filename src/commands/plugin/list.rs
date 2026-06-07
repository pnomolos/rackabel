//! `rackabel plugin list` (alias `ls`) (DESIGN §5.4).
//!
//! OWNED BY THE PLUGIN-MGMT AGENT. Shows installed plugins + enabled state + pinned ref +
//! source, with a "hooks pending (0.5)" marker for a plugin that carries a
//! `rackabel-plugin.toml` (inert metadata — no hook runs in 0.4). A pure read over the
//! frozen [`crate::plugin::lock`] model. `--json` is the machine-readable state surface
//! (§7). Running it also surfaces any upgrade-time collision (§5.6) loudly, once.

use crate::context::Ctx;
use crate::error::CmdResult;
use crate::plugin::lock::LockFile;

pub fn run(ctx: &Ctx) -> CmdResult<()> {
    // Surface a now-shadowed plugin loudly once before listing (§5.6).
    crate::plugin::collision::check_and_warn(ctx, crate::cli::is_reserved);

    let lock = LockFile::load(ctx)?;

    if ctx.json {
        let arr: Vec<_> = lock
            .plugins
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "source": format!("{:?}", p.source).to_lowercase(),
                    "origin": p.origin,
                    "commit": p.commit,
                    "sha256": p.sha256,
                    "pin": p.pin(),
                    "enabled": p.enabled,
                    "has_plugin_manifest": p.has_plugin_manifest,
                    "hooks": p.hooks,
                    "hooks_pending": p.has_plugin_manifest && !p.hooks.is_empty(),
                    "installed_at": p.installed_at,
                    "executable": p.executable.display().to_string(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "plugins": arr })).unwrap()
        );
        return Ok(());
    }

    if lock.plugins.is_empty() {
        println!("no plugins installed");
        println!("  install one with `rackabel plugin install OWNER/REPO`,");
        println!("  or find one with `rackabel plugin search <term>`");
        return Ok(());
    }

    for p in &lock.plugins {
        let state = if p.enabled { "enabled" } else { "disabled" };
        let source = format!("{:?}", p.source).to_lowercase();
        let hooks = if p.has_plugin_manifest && !p.hooks.is_empty() {
            format!("  [{} hook(s) pending (0.5)]", p.hooks.len())
        } else {
            String::new()
        };
        println!(
            "{}  {}  {}  ({}){}",
            p.name,
            state,
            p.pin_display(),
            source,
            hooks
        );
    }
    Ok(())
}
