//! A small, forgiving `package.json` reader (DESIGN §4.1).
//!
//! `package.json` is a *fallback* anchor + inference source for manifestless
//! ("synthesized") projects — never authoritative when a `rackabel.toml` is
//! present. Reading is best-effort: a missing or unparseable file yields `None`
//! and never errors, so discovery degrades gracefully.

use std::path::Path;

use serde::Deserialize;

/// The subset of `package.json` we read. Every field is optional; unknown keys
/// are ignored (no `deny_unknown_fields`, since `package.json` carries dozens of
/// keys we do not care about).
#[derive(Debug, Deserialize)]
pub struct PkgJson {
    pub name: Option<String>,
    pub version: Option<String>,
    pub author: Option<AuthorField>,
    /// The `"rackabel"` namespace — opt-in overrides for synthesized projects.
    pub rackabel: Option<PkgRackabel>,
}

/// The `"rackabel"` object inside `package.json`.
#[derive(Debug, Deserialize)]
pub struct PkgRackabel {
    /// `"extension"` or `"device"`. Selects the default kind for a manifestless
    /// project (see [`crate::manifest::Project::kind`]).
    pub kind: Option<String>,
    pub entry: Option<String>,
    pub name: Option<String>,
}

/// `package.json`'s `"author"` may be either a bare string or an object with a
/// `name` (and optional email/url). We extract a best-effort display string.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum AuthorField {
    Str(String),
    Obj { name: Option<String> },
}

impl AuthorField {
    /// A best-effort display string for the author, or `None` if the object form
    /// carried no usable `name`.
    pub fn display(&self) -> Option<String> {
        match self {
            AuthorField::Str(s) => {
                let s = s.trim();
                if s.is_empty() {
                    None
                } else {
                    Some(s.to_string())
                }
            }
            AuthorField::Obj { name } => name
                .as_ref()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty()),
        }
    }
}

impl PkgJson {
    /// The author's display string, if any.
    pub fn author_display(&self) -> Option<String> {
        self.author.as_ref().and_then(AuthorField::display)
    }
}

/// Read `<root>/package.json`, returning `None` if it is missing or unparseable.
/// Never errors — a malformed `package.json` simply contributes no inference.
pub fn read(root: &Path) -> Option<PkgJson> {
    let path = root.join("package.json");
    let body = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn missing_is_none() {
        let tmp = tempdir().unwrap();
        assert!(read(tmp.path()).is_none());
    }

    #[test]
    fn unparseable_is_none() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("package.json"), "{ not json").unwrap();
        assert!(read(tmp.path()).is_none());
    }

    #[test]
    fn author_string_form() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"thing","version":"1.2.3","author":"Jane Doe"}"#,
        )
        .unwrap();
        let p = read(tmp.path()).unwrap();
        assert_eq!(p.name.as_deref(), Some("thing"));
        assert_eq!(p.version.as_deref(), Some("1.2.3"));
        assert_eq!(p.author_display().as_deref(), Some("Jane Doe"));
    }

    #[test]
    fn author_object_form() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"author":{"name":"Jane","email":"j@x.io"}}"#,
        )
        .unwrap();
        let p = read(tmp.path()).unwrap();
        assert_eq!(p.author_display().as_deref(), Some("Jane"));
    }

    #[test]
    fn rackabel_namespace() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"rackabel":{"kind":"device","entry":"src/main.ts","name":"Dev"}}"#,
        )
        .unwrap();
        let p = read(tmp.path()).unwrap();
        let rk = p.rackabel.unwrap();
        assert_eq!(rk.kind.as_deref(), Some("device"));
        assert_eq!(rk.entry.as_deref(), Some("src/main.ts"));
        assert_eq!(rk.name.as_deref(), Some("Dev"));
    }

    #[test]
    fn extra_keys_ignored() {
        let tmp = tempdir().unwrap();
        fs::write(
            tmp.path().join("package.json"),
            r#"{"name":"x","scripts":{"build":"tsc"},"dependencies":{"a":"1"}}"#,
        )
        .unwrap();
        assert!(read(tmp.path()).is_some());
    }
}
