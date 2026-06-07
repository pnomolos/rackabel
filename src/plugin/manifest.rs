//! `rackabel-plugin.toml` — the lifecycle-hook manifest a tier-3 plugin carries
//! (DESIGN §5.3). A REAL PARSED structure in 0.5 (was inert in 0.4).
//!
//! FOUNDATION-OWNED. A tier-3 plugin ships a `rackabel-plugin.toml` declaring named
//! lifecycle hooks (`post_build`, `pre_deploy`, `on_reload`, `doctor_check`,
//! `new_template`) bound to scripts it carries, plus the tier-3 contract version
//! (`hook_api`) it targets and an optional `[hooks.timeouts]` per-hook override table.
//!
//! 0.4 parsed this INERT — only the hook NAMES, recorded in `plugins.lock` so the
//! install/list surface was ready. 0.5 turns it into a real model the discovery resolver
//! and engine read: the `[hooks]`/`[hooks.timeouts]` tables are now a typed
//! [`crate::hooks::manifest::HooksTable`], and `hook_api` is read so `plugin migrate` can
//! detect a declared version this build doesn't support. We still RECORD the inert
//! presence + hook-name list at install time (the lock keeps that for `plugin list`), and
//! hooks remain `enabled = false` by default — enabling is the 0.5 consent gate (§5.7).
//!
//! The parser stays lenient about FORWARD-COMPATIBILITY — it does NOT
//! `deny_unknown_fields`, so a 0.6+ manifest that adds tables/keys a 0.5 binary doesn't
//! know still parses and installs (we read only the fields we understand). It is strict
//! only about being valid TOML.

use std::path::Path;

use serde::Deserialize;

use crate::error::{CmdResult, ErrorCode, RkError};
use crate::hooks::HookKind;
use crate::hooks::manifest::HooksTable;

/// The conventional manifest filename a plugin may carry at its root (§5.3).
pub const PLUGIN_MANIFEST_NAME: &str = "rackabel-plugin.toml";

/// A parsed `rackabel-plugin.toml`. Forward-compatible (no `deny_unknown_fields`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginManifest {
    /// The tier-3 hook-contract version the plugin targets (§5.2/§5.3). Absent ⇒ treated
    /// as the floor (`1`) — an older plugin written before the key was conventional still
    /// installs and runs against the v1 contract.
    pub hook_api: Option<u32>,
    /// The `[hooks]` + `[hooks.timeouts]` tables (§5.3), a typed [`HooksTable`].
    #[serde(default)]
    pub hooks: HooksTable,
}

impl PluginManifest {
    /// Parse a `rackabel-plugin.toml` from text. A parse error is framed (the install
    /// aborts rather than silently recording a half-read manifest).
    pub fn parse(text: &str, at: &Path) -> CmdResult<Self> {
        toml::from_str(text).map_err(|e| {
            RkError::of(
                ErrorCode::ManifestParse,
                "the plugin's rackabel-plugin.toml could not be parsed",
                "fix the TOML, or report it to the plugin's author",
            )
            .at(at.display().to_string())
            .raw(e.into())
        })
    }

    /// Load + parse the manifest at `<dir>/rackabel-plugin.toml` if present. Returns
    /// `Ok(None)` when there is no manifest (the common tier-2 plugin case — a plain
    /// `rackabel-<foo>` executable with no hooks).
    pub fn load_from_dir(dir: &Path) -> CmdResult<Option<Self>> {
        let path = dir.join(PLUGIN_MANIFEST_NAME);
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).map_err(|e| {
            RkError::of(
                ErrorCode::ManifestParse,
                "could not read the plugin's rackabel-plugin.toml",
                "check the file's permissions and retry",
            )
            .at(path.display().to_string())
            .raw(e.into())
        })?;
        Ok(Some(Self::parse(&text, &path)?))
    }

    /// The declared hook names, sorted (deterministic for the lockfile + transcripts).
    /// This is the inert list `plugin install` records in `plugins.lock` (and `plugin
    /// list` shows); the discovery resolver re-reads the full [`HooksTable`] for commands.
    pub fn hook_names(&self) -> Vec<String> {
        self.hooks
            .declared_kinds()
            .into_iter()
            .map(|k| k.as_str().to_string())
            .collect()
    }

    /// The hook kinds declared, in [`HookKind::ALL`] order.
    pub fn declared_kinds(&self) -> Vec<HookKind> {
        self.hooks.declared_kinds()
    }

    /// The `hook_api` the plugin targets, defaulting to the v1 floor when unset.
    pub fn declared_hook_api(&self) -> u32 {
        self.hook_api.unwrap_or(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_hooks_table_names_only() {
        let src = r#"
            hook_api = 1
            [hooks]
            post_build = ".rackabel/hooks/post-build"
            pre_deploy = "bin/pre-deploy"
        "#;
        let m = PluginManifest::parse(src, Path::new("rackabel-plugin.toml")).unwrap();
        assert_eq!(m.hook_names(), vec!["post_build", "pre_deploy"]);
        assert_eq!(m.declared_hook_api(), 1);
        // The typed table is available for the engine/discovery.
        assert_eq!(
            m.hooks.command(HookKind::PostBuild),
            Some(".rackabel/hooks/post-build")
        );
    }

    #[test]
    fn forward_table_form_entry_is_not_recorded_as_a_name() {
        // A future `pre_deploy = { command = "x" }` parses but is not a v0.5 string
        // command, so it is neither recorded nor runnable on this build.
        let src = r#"
            [hooks]
            post_build = ".rackabel/hooks/post-build"
            pre_deploy = { command = "bin/pre-deploy", timeout = 30 }
        "#;
        let m = PluginManifest::parse(src, Path::new("rackabel-plugin.toml")).unwrap();
        assert_eq!(m.hook_names(), vec!["post_build"]);
    }

    #[test]
    fn timeouts_table_parses() {
        let src = r#"
            [hooks]
            post_build = "bin/pb"
            [hooks.timeouts]
            post_build = 120000
        "#;
        let m = PluginManifest::parse(src, Path::new("p")).unwrap();
        assert_eq!(m.hooks.timeout_ms(HookKind::PostBuild), 120_000);
        // The timeouts sub-table is not a hook name.
        assert_eq!(m.hook_names(), vec!["post_build"]);
    }

    #[test]
    fn forward_compatible_unknown_top_level_tables() {
        let src = r#"
            hook_api = 1
            name = "rackabel-plugin-notarize"
            [hooks]
            on_reload = "x"
            [some_future_table]
            whatever = true
        "#;
        let m = PluginManifest::parse(src, Path::new("p")).unwrap();
        assert_eq!(m.hook_names(), vec!["on_reload"]);
    }

    #[test]
    fn no_hooks_is_empty() {
        let m = PluginManifest::parse("", Path::new("p")).unwrap();
        assert!(m.hook_names().is_empty());
        // Absent hook_api defaults to the v1 floor.
        assert_eq!(m.declared_hook_api(), 1);
    }

    #[test]
    fn higher_hook_api_is_read_verbatim() {
        let m = PluginManifest::parse("hook_api = 2\n", Path::new("p")).unwrap();
        assert_eq!(m.declared_hook_api(), 2);
    }

    #[test]
    fn malformed_toml_is_framed() {
        let err = PluginManifest::parse("[hooks", Path::new("p")).unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestParse);
    }

    #[test]
    fn load_from_dir_present_and_absent() {
        let tmp = tempdir().unwrap();
        assert!(PluginManifest::load_from_dir(tmp.path()).unwrap().is_none());
        std::fs::write(
            tmp.path().join(PLUGIN_MANIFEST_NAME),
            "[hooks]\npost_build = \"x\"\n",
        )
        .unwrap();
        let m = PluginManifest::load_from_dir(tmp.path()).unwrap().unwrap();
        assert_eq!(m.hook_names(), vec!["post_build"]);
    }
}
