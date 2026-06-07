//! Inference helpers for the `[extension]` manifest (DESIGN §4.2, UX rule 1).
//!
//! Every field is optional; these supply the documented defaults. They do NOT echo
//! — echoing is the caller's job (it knows whether a value was actually inferred vs.
//! supplied), via [`crate::ui::echo_inferred`].

use std::path::{Path, PathBuf};
use std::process::Command;

/// Display name inferred from the project root directory basename.
pub fn infer_name_from_dir(root: &Path) -> String {
    root.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("extension")
        .to_string()
}

/// Author from `git config user.name` (best-effort; `None` if git/config absent).
pub fn infer_author_from_git() -> Option<String> {
    let out = Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

/// The default starting version (DESIGN §4.2: inferred 0.1.0).
pub fn default_version() -> semver::Version {
    semver::Version::new(0, 1, 0)
}

/// The conventional source entry point (`src/extension.ts`).
pub fn infer_entry(root: &Path) -> PathBuf {
    // The convention is fixed; `root` is accepted so a future heuristic (e.g.
    // detecting a differently-named single source file) can use it without an API
    // change. For 0.2 the conventional path is canonical.
    let _ = root;
    PathBuf::from("src/extension.ts")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_from_dir_basename() {
        assert_eq!(
            infer_name_from_dir(Path::new("/tmp/clip-renamer")),
            "clip-renamer"
        );
        assert_eq!(infer_name_from_dir(Path::new("/")), "extension");
    }

    #[test]
    fn default_version_is_010() {
        assert_eq!(default_version(), semver::Version::new(0, 1, 0));
    }

    #[test]
    fn entry_is_conventional() {
        assert_eq!(
            infer_entry(Path::new("/x")),
            PathBuf::from("src/extension.ts")
        );
    }
}
