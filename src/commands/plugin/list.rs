//! `rackabel plugin list` (alias `ls`) (DESIGN §5.4) — FOUNDATION-OWNED read.
//!
//! Shows installed plugins + enabled state + pinned ref, from `~/.rackabel/plugins.lock`.
//! A pure read over the frozen [`crate::plugin::lock`] model, so it works on day one even
//! before the install agent lands the write side. `--json` is the machine-readable state
//! surface (§7).

use crate::context::Ctx;
use crate::error::CmdResult;
use crate::plugin::lock::LockFile;

pub fn run(ctx: &Ctx) -> CmdResult<()> {
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
                    "enabled": p.enabled,
                    "has_plugin_manifest": p.has_plugin_manifest,
                    "hooks": p.hooks,
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
        let hooks = if p.has_plugin_manifest {
            format!(" [{} hook(s)]", p.hooks.len())
        } else {
            String::new()
        };
        println!("{}  {}  {}{}", p.name, state, p.pin_display(), hooks);
    }
    Ok(())
}
