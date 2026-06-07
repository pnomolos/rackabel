//! The `.rackabel/state.toml` sidecar (DESIGN §4.3).
//!
//! Tool-computed values (last-packed version for drift detection, build hash,
//! deploy timestamp) live here, never injected back into `rackabel.toml`, so
//! hand-editing the manifest never fights the tool. All fields optional; a missing
//! file means "fresh project".

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{CmdResult, ErrorCode, RkError};

const STATE_DIR: &str = ".rackabel";
const STATE_FILE: &str = "state.toml";

/// The sidecar state. Versions are stored as strings to keep the file
/// human-legible and tolerant of a hand-edit; callers parse to semver as needed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    /// The version of the last `pack` (drives version-bump validation, RK4003).
    pub last_packed_version: Option<String>,
    /// Short content hash of the last build (so "did it rebuild?" is answerable).
    pub build_hash: Option<String>,
    /// RFC3339 timestamp of the last deploy.
    pub deployed_at: Option<String>,
    /// The Live `.app` chosen for this project's dev loop (DESIGN §3.6 persisted
    /// multi-Live choice). Recorded so `rackabel dev` recalls the choice instead of
    /// re-prompting; the per-Live daemon socket/pidfile is keyed off this path's hash.
    pub dev_live: Option<String>,
    /// A snapshot of the SDK `manifest.json` as it was at the last `pack` (DESIGN §2
    /// stable-identifier drift). Persisted so `validate` can diff the CURRENTLY
    /// generated manifest's stable identifiers against the last shipped ones and warn
    /// when one disappeared/changed — "this breaks existing users' saved state". The
    /// SDK manifest carries only the five fields it reads (commands are registered in
    /// *code*, never on disk — DEVIATIONS D-12/D-102), so the checkable on-disk
    /// identifier is the extension `name` (the key Live uses for an extension's saved
    /// state); `entry` is recorded too for completeness. A missing snapshot means "no
    /// prior pack to diff against".
    pub last_packed_manifest: Option<PackedManifestSnapshot>,
}

/// The subset of the SDK `manifest.json` that `pack` snapshots for drift detection
/// (DESIGN §2). Mirrors the five fields the toolchain reads (SPEC A §2). Stored under
/// `[last_packed_manifest]` in `.rackabel/state.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackedManifestSnapshot {
    /// The extension `name` — the stable identifier Live keys saved state on. A change
    /// here is the on-disk "stable-identifier drift" the validate rule warns about.
    pub name: String,
    /// The manifest `author` (recorded for completeness; not drift-checked).
    pub author: String,
    /// The built `entry` bundle path (recorded for completeness).
    pub entry: String,
    /// The version this manifest shipped at — used in the §2 warning text
    /// ("present in 1.1.0").
    pub version: String,
    /// The `minimumApiVersion` this manifest declared (recorded for completeness).
    pub minimum_api_version: String,
}

/// Load the state for a project root. A missing file yields `State::default()`.
pub fn load(root: &Path) -> CmdResult<State> {
    let path = root.join(STATE_DIR).join(STATE_FILE);
    if !path.is_file() {
        return Ok(State::default());
    }
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        RkError::of(
            ErrorCode::ManifestParse,
            "could not read project state",
            "check permissions on the .rackabel directory",
        )
        .at(path.display().to_string())
        .raw(e.into())
    })?;
    toml::from_str(&raw).map_err(|e| {
        RkError::of(
            ErrorCode::ManifestParse,
            ".rackabel/state.toml is malformed",
            "delete .rackabel/state.toml to reset it (it is regenerated)",
        )
        .at(path.display().to_string())
        .raw(e.into())
    })
}

/// Save the state for a project root, creating `.rackabel/` if needed.
pub fn save(root: &Path, state: &State) -> CmdResult<()> {
    let dir = root.join(STATE_DIR);
    std::fs::create_dir_all(&dir).map_err(|e| {
        RkError::of(
            ErrorCode::DeployCopyFailed,
            "could not create the .rackabel state directory",
            "check write permissions for the project directory",
        )
        .at(dir.display().to_string())
        .raw(e.into())
    })?;
    let body = toml::to_string_pretty(state).expect("State serializes");
    let path = dir.join(STATE_FILE);
    std::fs::write(&path, body).map_err(|e| {
        RkError::of(
            ErrorCode::DeployCopyFailed,
            "could not write project state",
            "check write permissions for the .rackabel directory",
        )
        .at(path.display().to_string())
        .raw(e.into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_is_default() {
        let tmp = tempfile::tempdir().unwrap();
        let s = load(tmp.path()).unwrap();
        assert!(s.last_packed_version.is_none());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let s = State {
            last_packed_version: Some("1.2.3".into()),
            build_hash: Some("deadbeef".into()),
            deployed_at: Some("2026-06-06T00:00:00Z".into()),
            dev_live: None,
            last_packed_manifest: Some(PackedManifestSnapshot {
                name: "Clip Renamer".into(),
                author: "Jane".into(),
                entry: "dist/extension.js".into(),
                version: "1.2.3".into(),
                minimum_api_version: "1.0.0".into(),
            }),
        };
        save(tmp.path(), &s).unwrap();
        let back = load(tmp.path()).unwrap();
        assert_eq!(back.last_packed_version.as_deref(), Some("1.2.3"));
        assert_eq!(back.build_hash.as_deref(), Some("deadbeef"));
        let snap = back.last_packed_manifest.expect("snapshot persisted");
        assert_eq!(snap.name, "Clip Renamer");
        assert_eq!(snap.entry, "dist/extension.js");
        assert_eq!(snap.version, "1.2.3");
    }

    /// A state.toml written by an older rackabel (no `[last_packed_manifest]`) still
    /// loads — the field defaults to `None` (additive, backward-compatible).
    #[test]
    fn legacy_state_without_snapshot_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(STATE_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(STATE_FILE), "last_packed_version = \"1.0.0\"\n").unwrap();
        let s = load(tmp.path()).unwrap();
        assert_eq!(s.last_packed_version.as_deref(), Some("1.0.0"));
        assert!(s.last_packed_manifest.is_none());
    }
}
