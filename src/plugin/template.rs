//! Template models (DESIGN §5.5): `rackabel-template.toml` (the template's declared
//! prompts + merge-exclude globs) and `.rackabel-template` (the per-project lockfile
//! that persists repo + ref + commit + the rendered answers for `new --update`).
//!
//! FOUNDATION-OWNED models + parse/serialize + the small pure helpers. The templates
//! agent fills the rendering and the copier-style 3-way merge; the foundation freezes
//! the on-disk shapes both the render and the update read/write.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{CmdResult, ErrorCode, RkError};

/// The template manifest filename at a template repo's root. A directory without this is
/// not a template (`RK0402 TemplateNotFound`).
pub const TEMPLATE_MANIFEST_NAME: &str = "rackabel-template.toml";

/// The per-project lockfile `new` writes into a scaffolded project so `new --update` can
/// reconstruct the baseline (copier persists answers for exactly this reason, §5.5).
pub const TEMPLATE_LOCK_NAME: &str = ".rackabel-template";

/// A `rackabel-template.toml` (the template author's declaration). `[prompts]` drives the
/// `new` wizard; `[merge].exclude` lists globs the `new --update` 3-way text merge skips
/// (binary/generated files like vendored SDK tarballs — §5.5).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateManifest {
    /// The wizard prompts, keyed by the placeholder name they answer. A BTreeMap so the
    /// on-disk order is deterministic (TOML tables are unordered).
    #[serde(default)]
    pub prompts: BTreeMap<String, Prompt>,
    /// Merge controls for `new --update`.
    #[serde(default)]
    pub merge: Merge,
}

/// One wizard prompt (§5.5). `type` (renamed `kind` in Rust) is `string`/`bool`/`choice`;
/// `choices` is required-ish for a `choice` prompt; `default` seeds the bracketed value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prompt {
    /// Human-readable prompt label. Defaults to the prompt key if omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The answer type.
    #[serde(rename = "type", default = "default_prompt_type")]
    pub kind: PromptType,
    /// The default value (a string; a bool default is `"true"`/`"false"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The allowed values for a `choice` prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
}

fn default_prompt_type() -> PromptType {
    PromptType::String
}

/// The answer type a prompt accepts (§5.5 `[prompts] type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptType {
    String,
    Bool,
    Choice,
}

/// `[merge]` — controls for `new --update` (§5.5).
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Merge {
    /// Globs (relative to the project root) excluded from the 3-way TEXT merge — binary
    /// or generated files (vendored SDK/CLI tarballs, etc.). These are overwritten from
    /// the new render or left to the user; the marker-based merge never touches them.
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl TemplateManifest {
    /// Parse a `rackabel-template.toml` from `dir`. `RK0402 TemplateNotFound` if the file
    /// is absent (the dir is not a template); a parse error is framed `RK0003`.
    pub fn load(dir: &Path) -> CmdResult<Self> {
        let path = dir.join(TEMPLATE_MANIFEST_NAME);
        if !path.is_file() {
            return Err(RkError::of(
                ErrorCode::TemplateNotFound,
                "that template has no rackabel-template.toml",
                "a template is a directory/repo with a rackabel-template.toml at its root",
            )
            .at(dir.display().to_string()));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| {
            RkError::of(
                ErrorCode::TemplateNotFound,
                "could not read the template manifest",
                "check the file's permissions and try again",
            )
            .at(path.display().to_string())
            .raw(e.into())
        })?;
        toml::from_str(&text).map_err(|e| {
            RkError::of(
                ErrorCode::ManifestParse,
                "rackabel-template.toml could not be parsed",
                "fix the TOML (or unknown field) shown above",
            )
            .at(path.display().to_string())
            .raw(e.into())
        })
    }

    /// Validate the prompt declarations are internally consistent: a `choice` prompt must
    /// list `choices`, and a `default` for a `choice` must be one of them. Returns the
    /// first problem (framed `RK0402`) or `Ok`.
    pub fn validate(&self) -> CmdResult<()> {
        for (key, p) in &self.prompts {
            if p.kind == PromptType::Choice {
                if p.choices.is_empty() {
                    return Err(RkError::of(
                        ErrorCode::TemplateNotFound,
                        format!("prompt `{key}` is a choice but lists no choices"),
                        "add a `choices = [...]` to the prompt in rackabel-template.toml",
                    ));
                }
                if let Some(d) = &p.default
                    && !p.choices.contains(d)
                {
                    return Err(RkError::of(
                        ErrorCode::TemplateNotFound,
                        format!("prompt `{key}` default `{d}` is not one of its choices"),
                        "set the default to one of the listed choices",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The `.rackabel-template` lockfile persisted into a scaffolded project (§5.5). It
/// records the template ORIGIN (repo + ref), the resolved COMMIT it was rendered at, and
/// the ANSWERS used — everything `new --update` needs to re-render the old baseline and
/// 3-way-merge against the new commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateLock {
    /// The template source as the user gave it (`gh:owner/repo`, `@scope/name`, or a
    /// local path) — round-trips back through [`super::source::TemplateSource::parse`].
    pub repo: String,
    /// The ref the user asked for (branch/tag), if any. The COMMIT below is what was
    /// actually resolved/pinned; `ref` is kept so `--update` can re-fetch the same line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    /// The resolved commit the project was rendered from (the merge BASE anchor). Absent
    /// for a local-path template that is not a git repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The rendered answers, by prompt key. Re-used verbatim by `--update` (re-prompting
    /// only for prompts that are NEW in the updated template).
    #[serde(default)]
    pub answers: BTreeMap<String, String>,
}

impl TemplateLock {
    /// Load `<project>/.rackabel-template`. `None` when absent (the project wasn't made
    /// from a tracked template — `--update` then has nothing to do and says so).
    pub fn load(project_dir: &Path) -> CmdResult<Option<Self>> {
        let path = project_dir.join(TEMPLATE_LOCK_NAME);
        if !path.is_file() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).map_err(|e| {
            RkError::of(
                ErrorCode::ManifestParse,
                "could not read .rackabel-template",
                "check the file's permissions and try again",
            )
            .at(path.display().to_string())
            .raw(e.into())
        })?;
        let lock = toml::from_str(&text).map_err(|e| {
            RkError::of(
                ErrorCode::ManifestParse,
                ".rackabel-template could not be parsed",
                "fix the TOML shown above (or delete it to forget the template link)",
            )
            .at(path.display().to_string())
            .raw(e.into())
        })?;
        Ok(Some(lock))
    }

    /// Write `<project>/.rackabel-template` (atomic).
    pub fn save(&self, project_dir: &Path) -> CmdResult<()> {
        let path = project_dir.join(TEMPLATE_LOCK_NAME);
        let body = toml::to_string_pretty(self).map_err(|e| {
            RkError::of(
                ErrorCode::ManifestParse,
                "could not serialize .rackabel-template",
                "this is a bug; please report it",
            )
            .raw(e.into())
        })?;
        let header = "# .rackabel-template — records the template this project was made from (for `new --update`)\n";
        let tmp = path.with_extension("template.tmp");
        std::fs::write(&tmp, format!("{header}{body}")).map_err(|e| io_err(&tmp, e))?;
        std::fs::rename(&tmp, &path).map_err(|e| io_err(&path, e))?;
        Ok(())
    }
}

fn io_err(path: &Path, e: std::io::Error) -> RkError {
    RkError::of(
        ErrorCode::ManifestParse,
        "could not write .rackabel-template",
        "check write permissions on the project directory and retry",
    )
    .at(path.display().to_string())
    .raw(e.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_prompts_and_merge_exclude() {
        let tmp = tempdir().unwrap();
        std::fs::write(
            tmp.path().join(TEMPLATE_MANIFEST_NAME),
            r#"
[prompts.name]
label = "Extension name"
type = "string"
default = "my-ext"

[prompts.flavor]
type = "choice"
choices = ["plain", "fancy"]
default = "plain"

[prompts.git]
type = "bool"
default = "true"

[merge]
exclude = ["vendor/**", "*.tgz"]
"#,
        )
        .unwrap();
        let m = TemplateManifest::load(tmp.path()).unwrap();
        m.validate().unwrap();
        assert_eq!(m.prompts.len(), 3);
        assert_eq!(m.prompts["name"].kind, PromptType::String);
        assert_eq!(m.prompts["flavor"].kind, PromptType::Choice);
        assert_eq!(m.prompts["flavor"].choices, vec!["plain", "fancy"]);
        assert_eq!(m.prompts["git"].kind, PromptType::Bool);
        assert_eq!(m.merge.exclude, vec!["vendor/**", "*.tgz"]);
    }

    #[test]
    fn missing_manifest_is_template_not_found() {
        let tmp = tempdir().unwrap();
        let err = TemplateManifest::load(tmp.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::TemplateNotFound);
    }

    #[test]
    fn default_type_is_string_and_unknown_field_rejected() {
        let m: TemplateManifest = toml::from_str("[prompts.x]\n").unwrap();
        assert_eq!(m.prompts["x"].kind, PromptType::String);
        // deny_unknown_fields: a typo'd key is a parse error.
        assert!(toml::from_str::<TemplateManifest>("[prompts.x]\ntyop = \"string\"\n").is_err());
    }

    #[test]
    fn choice_without_choices_fails_validate() {
        let m: TemplateManifest = toml::from_str("[prompts.x]\ntype = \"choice\"\n").unwrap();
        assert_eq!(m.validate().unwrap_err().code, ErrorCode::TemplateNotFound);
    }

    #[test]
    fn choice_default_must_be_a_choice() {
        let m: TemplateManifest = toml::from_str(
            "[prompts.x]\ntype = \"choice\"\nchoices = [\"a\", \"b\"]\ndefault = \"c\"\n",
        )
        .unwrap();
        assert_eq!(m.validate().unwrap_err().code, ErrorCode::TemplateNotFound);
    }

    #[test]
    fn template_lock_round_trips() {
        let tmp = tempdir().unwrap();
        let mut answers = BTreeMap::new();
        answers.insert("name".to_string(), "clip-renamer".to_string());
        answers.insert("flavor".to_string(), "fancy".to_string());
        let lock = TemplateLock {
            repo: "gh:acme/starter".to_string(),
            r#ref: Some("v2".to_string()),
            commit: Some("abc1234def".to_string()),
            answers,
        };
        lock.save(tmp.path()).unwrap();

        let back = TemplateLock::load(tmp.path()).unwrap().unwrap();
        assert_eq!(back, lock);
        // The repo string round-trips through the source parser.
        assert!(super::super::source::TemplateSource::parse(&back.repo).is_some());
    }

    #[test]
    fn template_lock_absent_is_none() {
        let tmp = tempdir().unwrap();
        assert!(TemplateLock::load(tmp.path()).unwrap().is_none());
    }
}
