//! `rackabel plugin enable <name>` (DESIGN §5.4/§5.7).
//!
//! OWNED BY THE PLUGIN-MGMT AGENT. Flips the `enabled` flag on the
//! [`crate::plugin::lock`] entry. In 0.4 this gates DISPATCH of the MANAGED copy: a
//! disabled managed plugin is skipped in the bin search with a one-line note (see
//! [`crate::plugin::resolve`]). It is also the consent gate for 0.5 hooks — enabling IS the
//! consent (§5.7); no hook is executed in 0.4.

use crate::cli::PluginNameArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::plugin::lock::LockFile;
use crate::ui;

pub fn run(args: &PluginNameArgs, ctx: &Ctx) -> CmdResult<()> {
    set_enabled(&args.name, true, ctx)
}

/// Shared enable/disable: load the lock, flip the flag (idempotent), persist, report.
pub fn set_enabled(name: &str, enabled: bool, ctx: &Ctx) -> CmdResult<()> {
    // Announce any upgrade-time collision loudly once (§5.6) on every plugin command.
    crate::plugin::collision::check_and_warn(ctx, crate::cli::is_reserved);

    let mut lock = LockFile::load(ctx)?;
    let entry = lock.find_mut(name).ok_or_else(|| not_installed(name))?;

    let was = entry.enabled;
    entry.enabled = enabled;
    let has_hooks = entry.has_plugin_manifest && !entry.hooks.is_empty();
    lock.save(ctx)?;

    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": name,
                "enabled": enabled,
                "changed": was != enabled,
            }))
            .unwrap()
        );
        return Ok(());
    }
    if ctx.echo_on() {
        let state = if enabled { "enabled" } else { "disabled" };
        if was == enabled {
            ui::frame::emit(
                ui::frame::Symbol::Good,
                &format!("`{name}` is already {state}"),
                ctx,
            );
        } else {
            ui::frame::emit(ui::frame::Symbol::Good, &format!("{state} `{name}`"), ctx);
            if enabled && has_hooks {
                ui::frame::note(
                    "its lifecycle hooks are recorded but do not run yet (a later release)",
                    ctx,
                );
            }
            if !enabled {
                ui::frame::note(
                    "its managed copy is now skipped in the bin search; `rackabel plugin run \
                     <name>` still invokes it explicitly",
                    ctx,
                );
            }
        }
    }
    Ok(())
}

fn not_installed(name: &str) -> RkError {
    RkError::of(
        ErrorCode::PluginNotFound,
        format!("no plugin named `{name}` is installed"),
        "run `rackabel plugin list`, or `rackabel plugin install OWNER/REPO`",
    )
}
