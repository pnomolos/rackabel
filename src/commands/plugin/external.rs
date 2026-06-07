//! The `rackabel <foo>` PATH-subcommand dispatch (DESIGN §5.1) — FOUNDATION-OWNED.
//!
//! clap routes a leading token that matches NO built-in into `Command::External(argv)`.
//! This module resolves `argv[0]` to a `rackabel-<foo>` executable (managed-bin-first,
//! then `$PATH`), sets the §5.2 env contract, forwards the trailing args verbatim, and
//! runs it — its exit code passes through (tier 2, §7). A miss is `RK0401`.

use std::ffi::OsString;
use std::process::Command;

use crate::cli::RESERVED_NAMESPACE;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::plugin::lock::LockFile;
use crate::plugin::resolve::{self, Resolution};
use crate::plugin::{env_contract, warn_state};
use crate::ui;

/// Run an external `rackabel-<foo>` subcommand. `argv[0]` is the `<foo>` token; the rest
/// are forwarded verbatim. The process exit code is propagated by exiting with it (so a
/// plugin's non-zero status is the user's status — tier-2 passthrough).
pub fn run(argv: &[OsString], ctx: &Ctx) -> CmdResult<()> {
    let Some((name_os, rest)) = argv.split_first() else {
        return Err(unknown_command("", ctx));
    };
    let name = name_os.to_string_lossy().into_owned();

    let resolution = resolve::resolve_real(ctx, &name);
    let exe = match &resolution {
        // A built-in shadows the name. By construction clap would have routed a built-in
        // to its own handler, so reaching here for a reserved token is unusual — but we
        // surface it honestly rather than running a shadowed plugin behind the user's
        // back. (`plugin run` is the explicit escape hatch.)
        Resolution::Builtin { .. } => {
            return Err(RkError::of(
                ErrorCode::PluginShadowedByBuiltin,
                format!("`{name}` is a built-in subcommand"),
                format!("run the plugin explicitly with `rackabel plugin run {name}`"),
            ));
        }
        Resolution::Managed { path, also_on_path } => {
            // Dispatch gating (§5.4): a DISABLED managed plugin is skipped in the bin
            // search with a one-line note. If the same name is ALSO on $PATH, fall back to
            // that unmanaged copy; otherwise treat it as not-installed for the bare name
            // (the escape hatch `plugin run` still reaches it). PLUGIN-MGMT-owned hook.
            if crate::plugin::store::is_managed_disabled(ctx, &name) {
                note_managed_disabled(&name, ctx);
                if let Some(p) = path_lookup_path(also_on_path, &name, ctx) {
                    p
                } else {
                    return Err(disabled_managed(&name));
                }
            } else {
                if *also_on_path {
                    // The one-time both-locations nudge (§5.1) — persisted per name per
                    // RACKABEL_HOME, suppressed (and not recorded) under --json/quiet.
                    warn_state::warn_both_locations_once(ctx, &name);
                }
                path.clone()
            }
        }
        Resolution::Path { path } => path.clone(),
        Resolution::NotFound => return Err(unknown_command(&name, ctx)),
    };

    // Tamper check (§5.7): before running a MANAGED (lock-recorded) plugin, verify its
    // on-disk bytes still match the `plugins.lock` sha256 pin. A modified installed file is
    // `RK4007` (validation, exit 4) — pins protect against tampering and silent updates.
    // An unmanaged `$PATH` plugin has no pin and is run as-is (the user owns it).
    // PLUGIN-MGMT-owned hook.
    crate::plugin::store::verify_managed(ctx, &name)?;

    // Build the §5.2 env contract and overlay it on the inherited environment.
    let project = env_contract::resolve_project_root(ctx);
    let env = env_contract::build(ctx, project.as_deref());

    let mut cmd = Command::new(&exe);
    cmd.args(rest);
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

    // Tier-2 passthrough: the plugin's exit code is rackabel's. A success is Ok(());
    // any non-zero (or signal) terminates the process with that code directly, since the
    // RkError taxonomy is for rackabel's OWN failures, not a plugin's.
    if status.success() {
        Ok(())
    } else {
        let code = status.code().unwrap_or(1);
        std::process::exit(code);
    }
}

/// The `RK0401` "no such plugin/command" frame for a missing external subcommand.
///
/// The `help:` line carries an optional did-you-mean BLOCK listing the closest built-in
/// and/or installed-plugin candidates (§5.1). It is a help line LISTING candidates, never
/// an auto-correct: rackabel never silently runs a different command than the user typed.
fn unknown_command(name: &str, ctx: &Ctx) -> RkError {
    let base = "run `rackabel --help` for built-ins, `rackabel plugin list` for installed \
                plugins, or `rackabel plugin install OWNER/REPO` to add one";
    let help = match did_you_mean(name, ctx) {
        Some(candidates) => format!("did you mean: {candidates}?\n{base}"),
        None => base.to_string(),
    };
    RkError::of(
        ErrorCode::PluginNotFound,
        if name.is_empty() {
            "no subcommand given".to_string()
        } else {
            format!("unknown command `{name}` (no built-in and no plugin by that name)")
        },
        help,
    )
}

/// A comma-joined list of the closest built-in + installed-plugin candidates for `name`,
/// or `None` when nothing is close enough. Candidates are within edit distance 2 (and at
/// most half the typed token's length), so an obvious typo (`biuld` → `build`) surfaces a
/// suggestion while a genuinely novel token gets none. Never auto-corrects.
fn did_you_mean(name: &str, ctx: &Ctx) -> Option<String> {
    if name.is_empty() {
        return None;
    }
    // Candidate pool: every reserved built-in + every installed plugin name. A lockfile
    // read failure is non-fatal here — a missing suggestion never blocks the real error.
    let mut pool: Vec<String> = RESERVED_NAMESPACE.iter().map(|s| s.to_string()).collect();
    if let Ok(lock) = LockFile::load(ctx) {
        pool.extend(lock.plugins.iter().map(|p| p.name.clone()));
    }

    let threshold = (name.chars().count() / 2).clamp(1, 2);
    let mut hits: Vec<(usize, String)> = pool
        .into_iter()
        .filter(|c| c != name)
        .filter_map(|c| {
            let d = edit_distance(name, &c);
            (d <= threshold).then_some((d, c))
        })
        .collect();
    if hits.is_empty() {
        return None;
    }
    // Closest first, then alphabetical for determinism; dedup; cap at three.
    hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    hits.dedup_by(|a, b| a.1 == b.1);
    let list: Vec<String> = hits
        .into_iter()
        .take(3)
        .map(|(_, c)| format!("`{c}`"))
        .collect();
    Some(list.join(", "))
}

/// Levenshtein edit distance between two tokens (small, allocation-light DP over chars).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// When a managed copy is disabled but the same name is ALSO on `$PATH`, re-resolve the
/// PATH copy (the managed one is skipped). Returns `None` when there is no PATH fallback.
fn path_lookup_path(also_on_path: &bool, name: &str, ctx: &Ctx) -> Option<std::path::PathBuf> {
    if !*also_on_path {
        return None;
    }
    let found = resolve::path_lookup_real(&crate::plugin::exe_basename(name));
    if found.is_some() && ctx.echo_on() {
        ui::frame::ewarn(
            &format!("using the $PATH `rackabel-{name}` (the managed copy is disabled)"),
            ctx,
        );
    }
    found
}

/// The one-line note that a managed copy is skipped because it is disabled (§5.4).
fn note_managed_disabled(name: &str, ctx: &Ctx) {
    if ctx.echo_on() {
        ui::frame::ewarn(
            &format!(
                "managed plugin `{name}` is disabled (skipped); enable it with `rackabel \
                 plugin enable {name}`, or run it anyway with `rackabel plugin run {name}`"
            ),
            ctx,
        );
    }
}

/// The `RK0401` frame for a bare-name dispatch to a DISABLED managed plugin with no $PATH
/// fallback: the plugin exists but is intentionally skipped (§5.4).
fn disabled_managed(name: &str) -> RkError {
    RkError::of(
        ErrorCode::PluginNotFound,
        format!("managed plugin `{name}` is installed but disabled"),
        format!(
            "enable it with `rackabel plugin enable {name}`, or invoke it anyway with \
             `rackabel plugin run {name}`"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("build", "build"), 0);
        assert_eq!(edit_distance("biuld", "build"), 2); // transposition = 2 subs
        assert_eq!(edit_distance("buld", "build"), 1); // one insertion
        assert_eq!(edit_distance("", "abc"), 3);
    }
}
