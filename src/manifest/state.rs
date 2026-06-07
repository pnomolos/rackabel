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
        };
        save(tmp.path(), &s).unwrap();
        let back = load(tmp.path()).unwrap();
        assert_eq!(back.last_packed_version.as_deref(), Some("1.2.3"));
        assert_eq!(back.build_hash.as_deref(), Some("deadbeef"));
    }
}
