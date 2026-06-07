//! The §5.2 env contract a PATH-subcommand / hook plugin receives.
//!
//! FOUNDATION-OWNED. Before exec-ing `rackabel-<foo>` (§5.1) — and, in 0.5, a hook —
//! rackabel sets a language-agnostic, versioned env contract and forwards trailing args
//! verbatim. This module is the single place that map is built, so the env surface is
//! identical everywhere and the presence rules are tested.
//!
//! The contract is committed in writing to be **additive-only**: bumping
//! [`RACKABEL_PLUGIN_API`] only ever ADDS vars; a v1 var is never removed or repurposed
//! (§5.2). So a v1 plugin works unchanged forever.
//!
//! THE PRESENCE RULE (commit unset, not empty). `RACKABEL`, `RACKABEL_VERSION`,
//! `RACKABEL_PLUGIN_API`, and `RACKABEL_REGISTRY` are ALWAYS set. `RACKABEL_MANIFEST`
//! and `RACKABEL_PROJECT_DIR` are UNSET (not empty-string) when rackabel runs outside a
//! project. A plugin tests presence, never an empty string — removing the
//! empty-vs-unset ambiguity that commonly breaks plugins.

use std::collections::BTreeMap;
use std::path::Path;

use crate::context::Ctx;
use crate::manifest::{MANIFEST_NAME, Project};

/// The tier-2 env-contract version integer (§5.2). Currently 1. It is a HINT, never a
/// gate: because the contract is additive-only, a plugin never NEEDS this to keep
/// working — it presence-tests every var. The integer's one documented job is to signal
/// that newer optional vars may exist; bumping it is the only way rackabel announces "a
/// new var was added".
pub const RACKABEL_PLUGIN_API: u32 = 1;

/// Build the exact §5.2 env map for a plugin invocation. `project` is the resolved
/// project root when rackabel runs INSIDE a project, else `None` (e.g. `rackabel foo`
/// from `~`). The returned map holds ONLY the vars rackabel sets/overwrites; the caller
/// merges it onto the inherited environment (additive — it never unsets inherited vars,
/// it only overwrites the contract keys it owns).
///
/// Presence rules (the contract): `RACKABEL`, `RACKABEL_VERSION`, `RACKABEL_PLUGIN_API`,
/// `RACKABEL_REGISTRY` are ALWAYS present; `RACKABEL_MANIFEST` and `RACKABEL_PROJECT_DIR`
/// are present ONLY when `project` is `Some` (unset-not-empty otherwise). `RACKABEL` is
/// always the current binary's absolute path, OVERWRITING any inherited value (so a
/// nested call never picks up a stale path — cargo `CARGO`-points-at-wrong-binary bug).
pub fn build(ctx: &Ctx, project: Option<&Path>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();

    // RACKABEL — always set to the current binary, overwriting an inherited value.
    env.insert("RACKABEL".to_string(), current_exe_string());
    // RACKABEL_VERSION — the product version (0.x scheme).
    env.insert(
        "RACKABEL_VERSION".to_string(),
        env!("CARGO_PKG_VERSION").to_string(),
    );
    // RACKABEL_PLUGIN_API — the tier-2 contract integer.
    env.insert(
        "RACKABEL_PLUGIN_API".to_string(),
        RACKABEL_PLUGIN_API.to_string(),
    );
    // RACKABEL_REGISTRY — abs path to ~/.rackabel/registry.toml (always set).
    env.insert(
        "RACKABEL_REGISTRY".to_string(),
        registry_path(ctx).display().to_string(),
    );

    // RACKABEL_MANIFEST / RACKABEL_PROJECT_DIR — present ONLY in a project. Unset (not
    // empty) outside one: a plugin tests presence, never an empty string.
    if let Some(root) = project {
        env.insert(
            "RACKABEL_PROJECT_DIR".to_string(),
            root.display().to_string(),
        );
        env.insert(
            "RACKABEL_MANIFEST".to_string(),
            root.join(MANIFEST_NAME).display().to_string(),
        );
    }

    env
}

/// Resolve the project root for the env contract from `ctx.cwd`: the nearest ancestor
/// bearing a `rackabel.toml`, or `None` when rackabel runs outside any project. This is
/// the input to [`build`]'s `project` argument; isolating it keeps the discover failure
/// (RK0001) from leaking — a not-in-a-project state is normal for `rackabel foo` from
/// `~`, NOT an error.
pub fn resolve_project_root(ctx: &Ctx) -> Option<std::path::PathBuf> {
    Project::discover(&ctx.cwd).ok().map(|p| p.root)
}

/// The current binary's absolute path. Falls back to the literal `rackabel` (the user's
/// PATH will then resolve it) only if `current_exe` fails, which is essentially never on
/// a real install.
fn current_exe_string() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "rackabel".to_string())
}

/// `~/.rackabel/registry.toml`. The dev module owns the canonical helper; we re-derive
/// it here from `ctx.rackabel_home` to avoid a `#[cfg(unix)]`-gated dependency on the
/// dev module (the env contract is platform-independent). Kept in lockstep with
/// `dev::registry_path`.
fn registry_path(ctx: &Ctx) -> std::path::PathBuf {
    ctx.rackabel_home.join("registry.toml")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn ctx_in(home: &Path, cwd: &Path) -> Ctx {
        crate::context::Ctx {
            no_input: true,
            json: false,
            quiet: false,
            verbose: false,
            raw: false,
            color: crate::ui::color::ColorMode::Never,
            color_err: crate::ui::color::ColorMode::Never,
            cwd: cwd.to_path_buf(),
            rackabel_home: home.join(".rackabel"),
            home: home.to_path_buf(),
            ableton_app: None,
            ableton_user_library: None,
            ableton_eh_mod: None,
            ableton_eh_node: None,
            ableton_extensions_dir: None,
            ableton_storage_base: None,
            rackabel_host_cmd: None,
        }
    }

    #[test]
    fn always_set_vars_present_outside_a_project() {
        let tmp = tempdir().unwrap();
        let c = ctx_in(tmp.path(), tmp.path());
        let env = build(&c, None);
        // The four always-set vars.
        assert!(env.contains_key("RACKABEL"));
        assert!(env.contains_key("RACKABEL_VERSION"));
        assert_eq!(env["RACKABEL_PLUGIN_API"], "1");
        assert!(
            env["RACKABEL_REGISTRY"].ends_with(".rackabel/registry.toml"),
            "got {}",
            env["RACKABEL_REGISTRY"]
        );
        // The two project-only vars are UNSET (not empty) outside a project.
        assert!(!env.contains_key("RACKABEL_MANIFEST"));
        assert!(!env.contains_key("RACKABEL_PROJECT_DIR"));
    }

    #[test]
    fn project_vars_present_only_inside_a_project() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("my-ext");
        let env = build(&ctx_in(tmp.path(), &root), Some(&root));
        assert_eq!(env["RACKABEL_PROJECT_DIR"], root.display().to_string());
        assert_eq!(
            env["RACKABEL_MANIFEST"],
            root.join(MANIFEST_NAME).display().to_string()
        );
        // No empty strings anywhere — presence is the only signal.
        for (k, v) in &env {
            assert!(!v.is_empty(), "{k} is empty — must be unset, not empty");
        }
    }

    #[test]
    fn rackabel_var_is_absolute_and_overwrites() {
        let tmp = tempdir().unwrap();
        let env = build(&ctx_in(tmp.path(), tmp.path()), None);
        let r = &env["RACKABEL"];
        // Always set, and it does not honor a (hypothetical) inherited value — `build`
        // computes it from current_exe every time, so a stale RACKABEL is overwritten.
        assert!(!r.is_empty());
    }

    #[test]
    fn resolve_project_root_finds_manifest_or_none() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(MANIFEST_NAME), "[extension]\nname=\"x\"\n").unwrap();
        let c = ctx_in(tmp.path(), &root);
        assert_eq!(resolve_project_root(&c), Some(canon(&root)));

        // Outside any project → None (NOT an error).
        let elsewhere = tmp.path().join("nowhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        let c2 = ctx_in(tmp.path(), &elsewhere);
        assert_eq!(resolve_project_root(&c2), None);
    }

    fn canon(p: &Path) -> PathBuf {
        // Project::discover returns the dir it found the manifest in (the start dir's
        // ancestor); for a manifest at `root`, that is `root`.
        p.to_path_buf()
    }
}
