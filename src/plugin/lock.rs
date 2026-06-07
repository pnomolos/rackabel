//! `~/.rackabel/plugins.lock` — the authoritative, pinned record of installed plugins
//! (DESIGN §5.4).
//!
//! FOUNDATION-OWNED model + IO. Every `plugin install` writes an entry pinning the
//! resolved code by commit (a clone) or sha256 (a downloaded asset/tarball). The
//! lockfile is authoritative — a pin mismatch at install/verify time is `RK4007`
//! (validation, exit 4) so CI can gate on it. rackabel NEVER auto-updates an entry
//! silently; `--force` past a pin announces it (§5.4/§5.7).
//!
//! The model also RECORDS what 0.5 (lifecycle hooks) will need without executing
//! anything: whether the installed plugin carried a `rackabel-plugin.toml`, and the
//! inert list of hook names it declared. Hooks are `enabled = false` by default and no
//! hook is ever run in 0.4 — this is purely metadata so the install/list surface is
//! ready for 0.5 (per the milestone note: parse and store presence/hook list as inert).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};

use super::lock_path;

/// How a plugin's code was obtained (DESIGN §5.4). Drives which pin field is
/// authoritative: `Gh` pins by `commit`; `Path`/`Tarball` (sideload) pin by `sha256`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    /// `OWNER/REPO` — a GitHub release asset, or a clone built locally.
    Gh,
    /// A local path the user sideloaded (always works, no gatekeeper).
    Path,
    /// A local (or downloaded) `.tgz`/tarball the user sideloaded.
    Tarball,
}

/// One installed plugin, as recorded in `plugins.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginLockEntry {
    /// The plugin name (the `<foo>` of `rackabel-<foo>`; how `plugin which/run` address it).
    pub name: String,
    /// How the code was obtained.
    pub source: SourceKind,
    /// The original source string the user gave (e.g. `owner/repo`, `~/p/rackabel-foo`).
    pub origin: String,
    /// The pinned git commit (for a `gh` clone). Absent for an asset/sideload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The pinned sha256 of the resolved executable/asset (for an asset/tarball/path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// RFC3339 install timestamp (recorded as a string so no chrono dep is needed).
    pub installed_at: String,
    /// The managed executable path (the symlink under `~/.rackabel/plugins/bin`).
    pub executable: PathBuf,
    /// Whether the installed plugin carried a `rackabel-plugin.toml` (0.5 metadata).
    #[serde(default)]
    pub has_plugin_manifest: bool,
    /// The hook names the manifest declared, stored INERT for 0.5 — never executed in
    /// 0.4. Empty when there is no manifest (or it declares no `[hooks]`).
    #[serde(default)]
    pub hooks: Vec<String>,
    /// Whether the plugin (and, in 0.5, its hooks) is enabled. Hooks are disabled by
    /// default (§5.3); a PATH-subcommand-only plugin is usable regardless of this flag,
    /// which is the consent gate for the 0.5 hook surface.
    #[serde(default)]
    pub enabled: bool,
}

impl PluginLockEntry {
    /// The pin actually authoritative for this entry's source kind: the commit for a
    /// `gh` clone, else the sha256. `None` only for a malformed entry.
    pub fn pin(&self) -> Option<&str> {
        match self.source {
            SourceKind::Gh => self.commit.as_deref().or(self.sha256.as_deref()),
            SourceKind::Path | SourceKind::Tarball => self.sha256.as_deref(),
        }
    }

    /// A human-readable pin display for `plugin list` (e.g. `commit abc1234` /
    /// `sha256 deadbe…`).
    pub fn pin_display(&self) -> String {
        if let Some(c) = &self.commit {
            format!("commit {}", short(c))
        } else if let Some(s) = &self.sha256 {
            format!("sha256 {}", short(s))
        } else {
            "unpinned".to_string()
        }
    }
}

fn short(s: &str) -> String {
    s.chars().take(12).collect()
}

/// The on-disk shape of `plugins.lock`: a list of `[[plugin]]` tables (+ a small format
/// version so a future change can be detected).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LockFile {
    /// The lockfile format version. Bumped only on a breaking shape change.
    #[serde(default = "default_lock_version")]
    pub version: u32,
    #[serde(default, rename = "plugin")]
    pub plugins: Vec<PluginLockEntry>,
}

fn default_lock_version() -> u32 {
    1
}

/// The current lockfile format version.
pub const LOCK_VERSION: u32 = 1;

impl LockFile {
    /// Load `~/.rackabel/plugins.lock`. A missing file yields an empty lockfile (the
    /// "delete the file to reset" affordance). A parse error is surfaced framed.
    pub fn load(ctx: &Ctx) -> CmdResult<Self> {
        let path = lock_path(ctx);
        Self::load_path(&path)
    }

    /// Load from an explicit path (testable without a full `Ctx`).
    pub fn load_path(path: &Path) -> CmdResult<Self> {
        if !path.is_file() {
            return Ok(Self {
                version: LOCK_VERSION,
                plugins: Vec::new(),
            });
        }
        let text = std::fs::read_to_string(path).map_err(|e| {
            RkError::of(
                ErrorCode::ManifestParse,
                "could not read plugins.lock",
                "check the file's permissions and try again",
            )
            .at(path.display().to_string())
            .raw(e.into())
        })?;
        toml::from_str(&text).map_err(|e| {
            RkError::of(
                ErrorCode::ManifestParse,
                "plugins.lock could not be parsed",
                "fix the TOML shown above, or delete the file to reset installed plugins",
            )
            .at(path.display().to_string())
            .raw(e.into())
        })
    }

    /// Persist to `~/.rackabel/plugins.lock` (atomic write + rename).
    pub fn save(&self, ctx: &Ctx) -> CmdResult<()> {
        self.save_path(&lock_path(ctx))
    }

    /// Persist to an explicit path (atomic).
    pub fn save_path(&self, path: &Path) -> CmdResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        let body = toml::to_string_pretty(self).map_err(|e| {
            RkError::of(
                ErrorCode::ManifestParse,
                "could not serialize plugins.lock",
                "this is a bug; please report it",
            )
            .raw(e.into())
        })?;
        let header = "# ~/.rackabel/plugins.lock  —  managed by rackabel; pins installed plugins\n";
        let tmp = path.with_extension("lock.tmp");
        std::fs::write(&tmp, format!("{header}{body}")).map_err(|e| io_err(&tmp, e))?;
        std::fs::rename(&tmp, path).map_err(|e| io_err(path, e))?;
        Ok(())
    }

    /// The entry for `name`, if any.
    pub fn find(&self, name: &str) -> Option<&PluginLockEntry> {
        self.plugins.iter().find(|p| p.name == name)
    }

    /// A mutable handle to the entry for `name`, if any (for `enable`/`disable`).
    pub fn find_mut(&mut self, name: &str) -> Option<&mut PluginLockEntry> {
        self.plugins.iter_mut().find(|p| p.name == name)
    }

    /// Insert or replace the entry for `entry.name` (idempotent re-install).
    pub fn upsert(&mut self, entry: PluginLockEntry) {
        if let Some(slot) = self.plugins.iter_mut().find(|p| p.name == entry.name) {
            *slot = entry;
        } else {
            self.plugins.push(entry);
        }
    }

    /// Remove the entry for `name`, returning it if present.
    pub fn remove(&mut self, name: &str) -> Option<PluginLockEntry> {
        if let Some(idx) = self.plugins.iter().position(|p| p.name == name) {
            Some(self.plugins.remove(idx))
        } else {
            None
        }
    }
}

fn io_err(path: &Path, e: std::io::Error) -> RkError {
    RkError::of(
        ErrorCode::ManifestParse,
        "could not write plugins.lock",
        "check write permissions on ~/.rackabel and retry",
    )
    .at(path.display().to_string())
    .raw(e.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample(name: &str) -> PluginLockEntry {
        PluginLockEntry {
            name: name.to_string(),
            source: SourceKind::Gh,
            origin: "owner/repo".to_string(),
            commit: Some("abcdef0123456789".to_string()),
            sha256: None,
            installed_at: "2026-06-07T00:00:00Z".to_string(),
            executable: PathBuf::from(format!("/home/u/.rackabel/plugins/bin/rackabel-{name}")),
            has_plugin_manifest: false,
            hooks: Vec::new(),
            enabled: false,
        }
    }

    #[test]
    fn round_trips_through_toml() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("plugins.lock");
        let mut lf = LockFile {
            version: LOCK_VERSION,
            plugins: vec![sample("foo")],
        };
        // A second entry carrying inert 0.5 hook metadata.
        lf.upsert(PluginLockEntry {
            source: SourceKind::Tarball,
            origin: "/dl/rackabel-notarize-1.0.tgz".to_string(),
            commit: None,
            sha256: Some("deadbeefcafebabe".to_string()),
            has_plugin_manifest: true,
            hooks: vec!["post_build".to_string(), "pre_deploy".to_string()],
            ..sample("notarize")
        });
        lf.save_path(&path).unwrap();

        let back = LockFile::load_path(&path).unwrap();
        assert_eq!(back.version, LOCK_VERSION);
        assert_eq!(back.plugins.len(), 2);
        let n = back.find("notarize").unwrap();
        assert_eq!(n.source, SourceKind::Tarball);
        assert!(n.has_plugin_manifest);
        assert_eq!(n.hooks, vec!["post_build", "pre_deploy"]);
        // Hooks are inert + disabled by default (0.5 consent gate).
        assert!(!n.enabled);
    }

    #[test]
    fn load_missing_is_empty() {
        let tmp = tempdir().unwrap();
        let lf = LockFile::load_path(&tmp.path().join("nope.lock")).unwrap();
        assert!(lf.plugins.is_empty());
        assert_eq!(lf.version, LOCK_VERSION);
    }

    #[test]
    fn pin_is_source_appropriate() {
        let gh = sample("g");
        assert_eq!(gh.pin(), Some("abcdef0123456789"));
        let mut tb = sample("t");
        tb.source = SourceKind::Tarball;
        tb.commit = None;
        tb.sha256 = Some("ff00".to_string());
        assert_eq!(tb.pin(), Some("ff00"));
        assert!(tb.pin_display().starts_with("sha256 "));
    }

    #[test]
    fn upsert_replaces_and_remove_works() {
        let mut lf = LockFile::default();
        lf.upsert(sample("foo"));
        lf.upsert(PluginLockEntry {
            enabled: true,
            ..sample("foo")
        });
        assert_eq!(lf.plugins.len(), 1);
        assert!(lf.find("foo").unwrap().enabled);
        assert!(lf.remove("foo").is_some());
        assert!(lf.find("foo").is_none());
        assert!(lf.remove("foo").is_none());
    }

    #[test]
    fn minimal_entry_defaults() {
        // An entry written without the 0.5 metadata gets the documented defaults.
        let src = r#"version = 1
[[plugin]]
name = "bare"
source = "path"
origin = "/x/rackabel-bare"
sha256 = "ab"
installed_at = "2026-06-07T00:00:00Z"
executable = "/home/u/.rackabel/plugins/bin/rackabel-bare"
"#;
        let lf: LockFile = toml::from_str(src).unwrap();
        let e = lf.find("bare").unwrap();
        assert!(!e.has_plugin_manifest);
        assert!(e.hooks.is_empty());
        assert!(!e.enabled);
    }
}
