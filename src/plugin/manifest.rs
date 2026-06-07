//! `rackabel-plugin.toml` — the OPTIONAL lifecycle-hook manifest a plugin may carry
//! (DESIGN §5.3). PARSED INERT in 0.4 (milestone note).
//!
//! OWNED BY THE PLUGIN-MGMT AGENT (foundation-fill). A tier-3 plugin ships a
//! `rackabel-plugin.toml` declaring named lifecycle hooks (`post_build`, `pre_deploy`,
//! `on_reload`, …) bound to scripts it carries. The 0.5 milestone runs those hooks; 0.4
//! does NOT execute anything. This module exists so `plugin install` can RECORD what 0.5
//! will need: whether the installed plugin carried a manifest and the inert list of hook
//! names it declared. Those land in the `plugins.lock` entry (`has_plugin_manifest` +
//! `hooks`), `enabled = false` by default (enabling is the 0.5 consent gate, §5.7).
//!
//! The parser is deliberately lenient about FORWARD-COMPATIBILITY — it does not
//! `deny_unknown_fields`, because a 0.5+ manifest may add tables/keys a 0.4 binary should
//! still be able to install (we read only the hook NAMES). It is strict only about being
//! valid TOML.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::error::{CmdResult, ErrorCode, RkError};

/// The conventional manifest filename a plugin may carry at its root (§5.3).
pub const PLUGIN_MANIFEST_NAME: &str = "rackabel-plugin.toml";

/// The inert, parsed `rackabel-plugin.toml`. Only the hook NAMES are read in 0.4; the
/// command/timeout/api fields a 0.5 binary will need are captured loosely so a forward
/// manifest still parses.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PluginManifest {
    /// The `[hooks]` table: `<hook_name> = <command-or-table>`. We keep the raw TOML value
    /// so a 0.5 manifest (where a hook may be a table with `command`/`timeout`) still
    /// parses; 0.4 only needs the KEYS (the hook names).
    #[serde(default)]
    pub hooks: BTreeMap<String, toml::Value>,
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
    /// Inert in 0.4 — no hook is executed; this is the list 0.5 will read.
    pub fn hook_names(&self) -> Vec<String> {
        self.hooks.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_hooks_table_names_only() {
        let src = r#"
            [hooks]
            post_build = ".rackabel/hooks/post-build"
            pre_deploy = { command = ".rackabel/hooks/pre-deploy", timeout = 30 }
        "#;
        let m = PluginManifest::parse(src, Path::new("rackabel-plugin.toml")).unwrap();
        // Both forms (string + table) parse; we capture only the names, sorted.
        assert_eq!(m.hook_names(), vec!["post_build", "pre_deploy"]);
    }

    #[test]
    fn forward_compatible_unknown_top_level_tables() {
        // A 0.5+ manifest may add tables a 0.4 binary doesn't know — it must still parse.
        let src = r#"
            [meta]
            hook_api = 1
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
    }

    #[test]
    fn malformed_toml_is_framed() {
        let err = PluginManifest::parse("[hooks", Path::new("p")).unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestParse);
    }

    #[test]
    fn load_from_dir_present_and_absent() {
        let tmp = tempdir().unwrap();
        // Absent → Ok(None).
        assert!(PluginManifest::load_from_dir(tmp.path()).unwrap().is_none());
        // Present → parsed.
        std::fs::write(
            tmp.path().join(PLUGIN_MANIFEST_NAME),
            "[hooks]\npost_build = \"x\"\n",
        )
        .unwrap();
        let m = PluginManifest::load_from_dir(tmp.path()).unwrap().unwrap();
        assert_eq!(m.hook_names(), vec!["post_build"]);
    }
}
