//! Remembered wizard answers (DESIGN §6.2: "Your answers above are remembered").
//!
//! The §6.2 SDK-not-found transcript promises the musician that their wizard answers
//! survive the failed run, so the re-run (`rackabel new <name> --sdk-dir …`) does not
//! re-ask name/author/license/template. We persist the answers to a small TOML file
//! under `$RACKABEL_HOME` keyed by the *project name* (the natural identity — the
//! re-run command names it). On the next `new` for the same name, the wizard seeds its
//! Enter-to-accept defaults from this file, and a successful scaffold clears it (the
//! answers are spent). A missing/garbage file is simply ignored — it is an optional
//! convenience, never load-bearing.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::context::Ctx;

/// The persisted answers for one in-progress `new`. All optional so a partial wizard
/// (the user only answered some prompts before the SDK-not-found stop) still round-trips.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberedAnswers {
    pub kind: Option<String>,
    pub name: Option<String>,
    pub author: Option<String>,
    pub license: Option<String>,
    pub template: Option<String>,
    pub minimal: Option<bool>,
}

impl RememberedAnswers {
    /// True if nothing was captured (so we needn't write a file).
    pub fn is_empty(&self) -> bool {
        *self == RememberedAnswers::default()
    }
}

const DIR: &str = "new-answers";

/// The file that remembers answers for `name`. We sanitize the name into a safe
/// filename so an odd project name can't escape the answers directory.
fn answers_path(ctx: &Ctx, name: &str) -> PathBuf {
    ctx.rackabel_home
        .join(DIR)
        .join(format!("{}.toml", safe_key(name)))
}

/// A filename-safe key derived from a project name (alnum + dash/underscore kept,
/// everything else collapsed to `-`). Never empty.
fn safe_key(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "unnamed".to_string()
    } else {
        trimmed
    }
}

/// Load remembered answers for `name`, or `None` if there are none (or the file is
/// unreadable/garbage — remembering is best-effort, never an error).
pub fn load(ctx: &Ctx, name: &str) -> Option<RememberedAnswers> {
    let path = answers_path(ctx, name);
    let raw = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&raw).ok()
}

/// Persist `answers` for `name` (best-effort; a write failure is swallowed so the
/// SDK-not-found message still prints — the promise is "remembered if possible", and a
/// failure to remember must not itself become a second error).
pub fn save(ctx: &Ctx, name: &str, answers: &RememberedAnswers) {
    if answers.is_empty() {
        return;
    }
    let path = answers_path(ctx, name);
    if let Some(parent) = path.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    if let Ok(body) = toml::to_string_pretty(answers) {
        let _ = std::fs::write(&path, body);
    }
}

/// Clear remembered answers for `name` (called after a successful scaffold — the
/// answers are spent). Best-effort.
pub fn clear(ctx: &Ctx, name: &str) {
    let _ = std::fs::remove_file(answers_path(ctx, name));
}

/// Whether a remembered-answers file exists for `name` (used to phrase the re-run hint).
pub fn exists(ctx: &Ctx, name: &str) -> bool {
    answers_path(ctx, name).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::tempdir;

    fn ctx_with_home(home: &Path) -> Ctx {
        Ctx {
            no_input: true,
            json: false,
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
        }
    }

    #[test]
    fn save_load_clear_roundtrip() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        let answers = RememberedAnswers {
            name: Some("clip-renamer".into()),
            author: Some("Jane Doe".into()),
            license: Some("MIT".into()),
            template: Some("default".into()),
            kind: Some("extension".into()),
            minimal: Some(false),
        };
        save(&ctx, "clip-renamer", &answers);
        assert!(exists(&ctx, "clip-renamer"));
        let back = load(&ctx, "clip-renamer").unwrap();
        assert_eq!(back, answers);
        clear(&ctx, "clip-renamer");
        assert!(!exists(&ctx, "clip-renamer"));
        assert!(load(&ctx, "clip-renamer").is_none());
    }

    #[test]
    fn empty_answers_are_not_written() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        save(&ctx, "x", &RememberedAnswers::default());
        assert!(!exists(&ctx, "x"));
    }

    #[test]
    fn missing_is_none() {
        let tmp = tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        assert!(load(&ctx, "never-saved").is_none());
    }

    #[test]
    fn safe_key_sanitizes() {
        assert_eq!(safe_key("clip-renamer"), "clip-renamer");
        assert_eq!(safe_key("My Cool Ext!"), "my-cool-ext");
        assert_eq!(safe_key("../etc/passwd"), "etc-passwd");
        assert_eq!(safe_key("***"), "unnamed");
    }
}
