//! Hook discovery resolution (DESIGN §5.3, §5.5, §5.7) — FROZEN ordering.
//!
//! FOUNDATION-OWNED. For a given [`HookKind`], the ordered list of hooks to run is:
//!   1. the PROJECT-local `[hooks]` entry from the project's own `rackabel.toml`
//!      (implicit trust — the user's own code, no enable step, §5.5); then
//!   2. EVERY ENABLED installed plugin that declares the kind, in `plugins.lock` order
//!      (the lock `enabled` flag is the consent gate — note 0.4 enable/disable gates
//!      DISPATCH; 0.5 makes the SAME flag also gate hooks, §5.7).
//!
//! A NOT-yet-enabled plugin contributes NOTHING — "enabling is the consent" is uniform
//! across all hook kinds, including the pre-wizard `new_template` (§5.3). A pin-change
//! that DISABLES a hook plugin (§5.7) therefore drops it from this list until re-enabled,
//! because it clears the `enabled` flag this resolver reads.
//!
//! This module resolves the ORDERED SOURCE LIST + each source's command/timeout; the
//! engine ([`super::engine`]) executes them. Resolution does no IO beyond reading the
//! lockfile and the per-plugin `rackabel-plugin.toml` already on disk.

use std::path::Path;

use crate::context::Ctx;
use crate::error::CmdResult;
use crate::plugin::lock::LockFile;
use crate::plugin::{plugin_store_dir, store};

use super::manifest::HooksTable;
use super::{HookKind, HookSource};

/// One resolved hook ready to run: its source (for trust + framing), the resolved
/// command path, and the wall-clock timeout (ms) to run it under.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedHook {
    /// Where the hook came from (project-local vs an enabled plugin).
    pub source: HookSource,
    /// The hook kind.
    pub kind: HookKind,
    /// The command path AS DECLARED (relative to `source.base_dir()`). The engine joins it
    /// onto the base dir; kept relative here so the source's root stays the single anchor.
    pub command: String,
    /// The resolved per-hook wall-clock timeout in milliseconds (§5.3).
    pub timeout_ms: u64,
}

impl ResolvedHook {
    /// The absolute command path: `source.base_dir()` joined with the declared command
    /// (an already-absolute command is used as-is). Paths are relative to the owning root
    /// (project root for a project hook, plugin store dir for a plugin hook).
    pub fn command_path(&self) -> std::path::PathBuf {
        let p = Path::new(&self.command);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.source.base_dir().join(p)
        }
    }
}

/// Resolve the ORDERED list of hooks for `kind` (the frozen §5.3/§5.5/§5.7 ordering).
///
/// `project` is the project's parsed `[hooks]` table when rackabel runs inside a project
/// (with its root), else `None` (e.g. `doctor`/`new` outside a project). The plugin
/// sources come from `plugins.lock`: every entry with `enabled = true` whose recorded
/// `hooks` list includes `kind`, re-reading its `rackabel-plugin.toml` for the command +
/// timeout. Project-local sources come FIRST, then enabled plugins in lock order.
pub fn resolve(
    ctx: &Ctx,
    kind: HookKind,
    project: Option<(&Path, &HooksTable)>,
) -> CmdResult<Vec<ResolvedHook>> {
    let mut out = Vec::new();

    // 1. Project-local [hooks] (implicit trust, no enable step) — always first.
    if let Some((root, table)) = project
        && let Some(cmd) = table.command(kind)
    {
        out.push(ResolvedHook {
            source: HookSource::Project {
                project_root: root.to_path_buf(),
            },
            kind,
            command: cmd.to_string(),
            timeout_ms: table.timeout_ms(kind),
        });
    }

    // 2. Enabled installed plugins declaring the kind, in plugins.lock order.
    let lock = LockFile::load(ctx)?;
    for entry in &lock.plugins {
        // The enable flag is the consent gate (§5.7); a disabled plugin contributes
        // nothing, including its new_template choice.
        if !entry.enabled {
            continue;
        }
        // The lock records the inert hook-name list at install time; skip a plugin that
        // does not declare this kind without touching its store dir.
        if !entry.hooks.iter().any(|h| h == kind.as_str()) {
            continue;
        }
        let store_dir = plugin_store_dir(ctx, &entry.name);
        // Re-read the manifest for the authoritative command + timeout (the lock holds
        // only names). A plugin whose manifest no longer parses / no longer declares the
        // kind is skipped here rather than failing the whole resolution.
        let Some(table) = load_plugin_hooks(&store_dir)? else {
            continue;
        };
        let Some(cmd) = table.command(kind) else {
            continue;
        };
        out.push(ResolvedHook {
            source: HookSource::Plugin {
                name: entry.name.clone(),
                store_dir: store_dir.clone(),
            },
            kind,
            command: cmd.to_string(),
            timeout_ms: table.timeout_ms(kind),
        });
    }

    Ok(out)
}

/// Load the `[hooks]` table from an installed plugin's `rackabel-plugin.toml` in its store
/// dir, returning `Ok(None)` when there is no manifest (or it declares no hooks). Uses the
/// foundation [`store`] resolver to find the manifest within the store dir.
fn load_plugin_hooks(store_dir: &Path) -> CmdResult<Option<HooksTable>> {
    match store::load_plugin_manifest(store_dir)? {
        Some(m) => Ok(Some(m.hooks)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::lock::{PluginLockEntry, SourceKind};
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn ctx(home: &Path) -> Ctx {
        Ctx {
            no_input: true,
            json: false,
            quiet: false,
            verbose: false,
            raw: false,
            color: crate::ui::color::ColorMode::Never,
            color_err: crate::ui::color::ColorMode::Never,
            cwd: home.to_path_buf(),
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

    fn table(src: &str) -> HooksTable {
        #[derive(serde::Deserialize)]
        struct W {
            hooks: HooksTable,
        }
        toml::from_str::<W>(src).unwrap().hooks
    }

    /// Install a fake plugin: write its store-dir manifest + a lock entry.
    fn install_plugin(ctx: &Ctx, name: &str, enabled: bool, manifest: &str, hooks: Vec<&str>) {
        let store = plugin_store_dir(ctx, name);
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("rackabel-plugin.toml"), manifest).unwrap();
        let mut lock = LockFile::load(ctx).unwrap();
        lock.upsert(PluginLockEntry {
            name: name.to_string(),
            source: SourceKind::Gh,
            origin: "o/r".to_string(),
            commit: Some("c".to_string()),
            sha256: None,
            installed_at: "2026-06-07T00:00:00Z".to_string(),
            executable: PathBuf::from(format!(".rackabel/plugins/bin/rackabel-{name}")),
            has_plugin_manifest: true,
            hooks: hooks.into_iter().map(str::to_string).collect(),
            enabled,
        });
        lock.save(ctx).unwrap();
    }

    #[test]
    fn project_local_comes_first_then_enabled_plugins_in_lock_order() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let proj_table = table("[hooks]\npost_build = \".rackabel/hooks/pb\"\n");

        install_plugin(
            &c,
            "alpha",
            true,
            "[hooks]\npost_build = \"bin/pb\"\n",
            vec!["post_build"],
        );
        install_plugin(
            &c,
            "beta",
            true,
            "[hooks]\npost_build = \"bin/pb\"\n",
            vec!["post_build"],
        );

        let resolved = resolve(&c, HookKind::PostBuild, Some((&proj, &proj_table))).unwrap();
        assert_eq!(resolved.len(), 3);
        // Project FIRST.
        assert!(matches!(resolved[0].source, HookSource::Project { .. }));
        // Then enabled plugins in lock order (alpha installed before beta).
        assert_eq!(resolved[1].source.label(), "plugin alpha");
        assert_eq!(resolved[2].source.label(), "plugin beta");
    }

    #[test]
    fn disabled_plugin_contributes_nothing() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        install_plugin(
            &c,
            "notarize",
            false, // NOT enabled — the consent gate (§5.7).
            "[hooks]\npre_deploy = \"bin/pd\"\n",
            vec!["pre_deploy"],
        );
        let resolved = resolve(&c, HookKind::PreDeploy, None).unwrap();
        assert!(resolved.is_empty(), "a disabled plugin runs no hooks");
    }

    #[test]
    fn plugin_not_declaring_the_kind_is_skipped() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        install_plugin(
            &c,
            "x",
            true,
            "[hooks]\npost_build = \"bin/pb\"\n",
            vec!["post_build"],
        );
        // Asking for a different kind ⇒ nothing.
        let resolved = resolve(&c, HookKind::OnReload, None).unwrap();
        assert!(resolved.is_empty());
    }

    #[test]
    fn timeout_override_flows_through() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        install_plugin(
            &c,
            "slow",
            true,
            "[hooks]\npost_build = \"bin/pb\"\n[hooks.timeouts]\npost_build = 120000\n",
            vec!["post_build"],
        );
        let resolved = resolve(&c, HookKind::PostBuild, None).unwrap();
        assert_eq!(resolved[0].timeout_ms, 120_000);
    }

    #[test]
    fn command_path_joins_relative_onto_base_dir() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().join("proj");
        let h = ResolvedHook {
            source: HookSource::Project {
                project_root: root.clone(),
            },
            kind: HookKind::PostBuild,
            command: ".rackabel/hooks/pb".to_string(),
            timeout_ms: 30_000,
        };
        assert_eq!(h.command_path(), root.join(".rackabel/hooks/pb"));
    }

    #[test]
    fn no_project_and_no_plugins_is_empty() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        assert!(resolve(&c, HookKind::DoctorCheck, None).unwrap().is_empty());
    }
}
