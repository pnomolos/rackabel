//! `rackabel plugin list` (alias `ls`) (DESIGN §5.4).
//!
//! OWNED BY THE PLUGIN-MGMT AGENT. Shows installed plugins + enabled state + pinned ref +
//! source, with an ENABLE-AWARE hooks marker for a plugin that carries a
//! `rackabel-plugin.toml`: `[N hook(s) active]` when ENABLED (its hooks run at their
//! lifecycle points — the 0.5 consent gate is satisfied) or `[N hook(s), disabled — `enable`
//! to run]` otherwise. `--json` mirrors this with `hooks_active`/`hooks_pending`. A pure read
//! over the frozen [`crate::plugin::lock`] model. `--json` is the machine-readable state
//! surface (§7). Running it also surfaces any upgrade-time collision (§5.6) loudly, once.

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
                    // Whether the plugin carries hooks that are NOT yet live: it has a
                    // manifest with hooks but is not enabled. An ENABLED hook plugin's hooks
                    // run at their lifecycle points, so it is NOT pending (see `hooks_active`).
                    "hooks_pending": p.has_plugin_manifest && !p.hooks.is_empty() && !p.enabled,
                    // Whether the plugin's hooks are LIVE: it has hooks AND is enabled (the
                    // 0.5 consent gate). `enable` makes a hook plugin's hooks active.
                    "hooks_active": p.has_plugin_manifest && !p.hooks.is_empty() && p.enabled,
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
            if p.enabled {
                // Enabled: the hooks run at their lifecycle points (the 0.5 consent gate is
                // satisfied) — they are LIVE, not "pending".
                format!("  [{} hook(s) active]", p.hooks.len())
            } else {
                // Carries hooks but not enabled — they do NOT run until `plugin enable`.
                format!("  [{} hook(s), disabled — `enable` to run]", p.hooks.len())
            }
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
