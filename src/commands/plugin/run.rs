//! `rackabel plugin run <name> [args…]` (DESIGN §5.6) — FOUNDATION-OWNED.
//!
//! The escape hatch: invoke a plugin executable EVEN IF a built-in shadows the name. It
//! uses the same external-candidate order as §5.1 (managed-bin first, then `$PATH`) and
//! emits the same one-time both-locations warning, so the escape hatch is itself
//! deterministic. The plugin gets the full §5.2 env contract and its exit code passes
//! through.

use std::process::Command;

use crate::cli::PluginRunArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::plugin::resolve::{self};
use crate::plugin::{env_contract, warn_state};

pub fn run(args: &PluginRunArgs, ctx: &Ctx) -> CmdResult<()> {
    let name = &args.name;
    let r = resolve::resolve_real(ctx, name);

    // Unlike the bare dispatch, `plugin run` ignores built-in shadowing and runs the
    // plugin if one exists at all (managed first, then PATH — Resolution::plugin_path
    // encodes that order). The both-locations warning still fires for a managed copy
    // that is also on PATH.
    let exe = match r.plugin_path() {
        Some(p) => {
            if r.both_locations() {
                warn_state::warn_both_locations_once(ctx, name);
            }
            // For a built-in-shadowed plugin, plugin_path returns the shadowed plugin —
            // exactly what the escape hatch is for.
            p.to_path_buf()
        }
        None => {
            return Err(RkError::of(
                ErrorCode::PluginNotFound,
                format!("no plugin named `{name}` is installed or on PATH"),
                "run `rackabel plugin list`, or `rackabel plugin install OWNER/REPO`",
            ));
        }
    };

    // Tamper check (§5.7): the escape hatch runs the MANAGED (lock-recorded) executable
    // when one exists (`plugin_path` returns managed-bin first), so it must enforce the
    // SAME `plugins.lock` pin the bare dispatch does (external.rs) — a modified store file
    // is `RK4007` (exit 4) BEFORE any code runs. `plugin run` deliberately ignores the
    // disabled flag (it always reaches the plugin), but it must NOT bypass the pin: the
    // sha256/commit guarantee that installed code hasn't been swapped applies on every path
    // that executes a managed plugin. An unmanaged `$PATH` copy has no lock entry and is
    // run as-is (the user owns it). PLUGIN-MGMT-owned hook.
    crate::plugin::store::verify_managed(ctx, name)?;

    let project = env_contract::resolve_project_root(ctx);
    let env = env_contract::build(ctx, project.as_deref());

    let mut cmd = Command::new(&exe);
    cmd.args(&args.args);
    for (k, v) in &env {
        cmd.env(k, v);
    }
    cmd.current_dir(&ctx.cwd);

    let status = cmd.status().map_err(|e| {
        RkError::of(
            ErrorCode::PluginNotFound,
            format!("could not run the plugin `rackabel-{name}`"),
            "the file may not be executable — `chmod +x` it, or reinstall the plugin",
        )
        .at(exe.display().to_string())
        .raw(e.into())
    })?;

    if status.success() {
        Ok(())
    } else {
        std::process::exit(status.code().unwrap_or(1));
    }
}
