//! Generate and read the SDK's `manifest.json` (DESIGN §4.5).
//!
//! The SDK manifest is *generated* from `rackabel.toml` on every build with a "do
//! not edit" marker — the user never hand-edits it. The fields are exactly the five
//! the toolchain reads/writes (SPEC A §2): `name`, `author`, `entry`, `version`,
//! `minimumApiVersion`. `entry` is the *built* bundle path (`dist/extension.js`),
//! not the source entry.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{CmdResult, ErrorCode, RkError};
use crate::manifest::ResolvedExtension;

/// The five fields the SDK toolchain reads/writes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SdkManifest {
    pub name: String,
    pub author: String,
    pub entry: String,
    pub version: String,
    #[serde(rename = "minimumApiVersion")]
    pub minimum_api_version: String,
}

/// Build the JSON object for the generated `manifest.json`. `dist_entry` is the
/// built bundle path written into `entry` (e.g. `"dist/extension.js"`).
///
/// We include a `_generated` marker key so a human who opens the file sees it is
/// tool-owned. The SDK ignores unknown keys (it only reads the five fields).
pub fn generate(ext: &ResolvedExtension, dist_entry: &str) -> Value {
    json!({
        "_generated": "by rackabel from rackabel.toml — do not edit; edit rackabel.toml instead",
        "name": ext.name,
        "author": ext.author,
        "entry": dist_entry,
        "version": ext.version.to_string(),
        "minimumApiVersion": ext.minimum_api_version.to_string(),
    })
}

/// Read a `manifest.json` from `dir`.
pub fn read(dir: &Path) -> CmdResult<SdkManifest> {
    let path = dir.join("manifest.json");
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        RkError::of(
            ErrorCode::ManifestIncomplete,
            "manifest.json not found",
            "run `rackabel build` to generate it",
        )
        .at(path.display().to_string())
        .raw(e.into())
    })?;
    serde_json::from_str(&raw).map_err(|e| {
        RkError::of(
            ErrorCode::ManifestIncomplete,
            "manifest.json is missing required fields or is malformed",
            "rerun `rackabel build` to regenerate it from rackabel.toml",
        )
        .at(path.display().to_string())
        .raw(e.into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ext() -> ResolvedExtension {
        ResolvedExtension {
            name: "Clip Renamer".into(),
            author: "Jane".into(),
            version: semver::Version::new(0, 1, 0),
            entry: PathBuf::from("src/extension.ts"),
            minimum_api_version: semver::Version::new(1, 0, 0),
            extra_dist_files: vec![],
            native_deps: vec![],
            pack_targets: vec![],
            inferred: vec![],
        }
    }

    #[test]
    fn generate_has_five_fields_and_marker() {
        let v = generate(&ext(), "dist/extension.js");
        assert_eq!(v["name"], "Clip Renamer");
        assert_eq!(v["author"], "Jane");
        assert_eq!(v["entry"], "dist/extension.js");
        assert_eq!(v["version"], "0.1.0");
        assert_eq!(v["minimumApiVersion"], "1.0.0");
        assert!(v["_generated"].as_str().unwrap().contains("do not edit"));
    }

    #[test]
    fn read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let v = generate(&ext(), "dist/extension.js");
        std::fs::write(
            tmp.path().join("manifest.json"),
            serde_json::to_string_pretty(&v).unwrap(),
        )
        .unwrap();
        let m = read(tmp.path()).unwrap();
        assert_eq!(m.name, "Clip Renamer");
        assert_eq!(m.entry, "dist/extension.js");
        assert_eq!(m.minimum_api_version, "1.0.0");
    }
}
