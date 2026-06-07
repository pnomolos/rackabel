//! Template resolution + rendering (DESIGN §5.5).
//!
//! A template is a git repo / local directory holding a `rackabel-template.toml` at its
//! root, declaring `[prompts]` (rendered as the `new` wizard) plus files containing
//! placeholders. This module:
//!   - resolves a [`TemplateSource`] to an on-disk directory (a local path verbatim, or a
//!     remote `gh:`/`@scope` clone via the foundation [`git`] wrapper behind the
//!     `RACKABEL_TEMPLATE_GIT_BASE` seam);
//!   - prints the §5.7 will-fetch-AND-build confirmation for a REMOTE source (local /
//!     built-in skip it; `--yes` consents in a script; `--no-input` refuses);
//!   - runs the declared prompts as the wizard (Enter-to-accept defaults), or resolves
//!     them from supplied/remembered answers under `--yes`/`--no-input`;
//!   - copies the template tree into the destination with placeholder substitution
//!     ([`super::placeholder`]'s `{{ key }}` syntax), skipping the template's own
//!     manifest, the `.git` dir, and (for the substitution pass) the `[merge].exclude`
//!     globs which are copied verbatim;
//!   - writes the `.rackabel-template` lockfile (repo + ref + resolved commit + answers)
//!     so `new --update` can reconstruct the baseline.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::plugin::git;
use crate::plugin::source::TemplateSource;
use crate::plugin::template::{
    Prompt, PromptType, TEMPLATE_LOCK_NAME, TEMPLATE_MANIFEST_NAME, TemplateLock, TemplateManifest,
};
use crate::ui;

use super::exclude::ExcludeSet;
use super::placeholder;

/// A resolved template: the directory it lives in, the parsed manifest, and the origin
/// metadata needed to write the `.rackabel-template` lockfile.
#[derive(Debug)]
pub struct ResolvedTemplate {
    /// The on-disk root of the template (a local path, or a clone in a tempdir).
    pub dir: PathBuf,
    /// The parsed `rackabel-template.toml`.
    pub manifest: TemplateManifest,
    /// The source string, exactly as the user gave it (round-trips through
    /// [`TemplateSource::parse`]) — persisted into the lockfile `repo`.
    pub repo: String,
    /// The ref the user asked for (only `gh:…@ref`), persisted as the lockfile `ref`.
    pub git_ref: Option<String>,
    /// The resolved commit (a git repo's `HEAD`), persisted as the lockfile `commit`.
    /// `None` for a local-path template that is not a git repo.
    pub commit: Option<String>,
    /// A temp clone holder: kept alive so the clone dir survives while `dir` points into
    /// it. `None` for a local-path template.
    _clone: Option<tempfile::TempDir>,
}

/// Resolve `source` into a [`ResolvedTemplate`].
///
/// For a REMOTE source (`gh:`/`@scope`), this prints the §5.7 confirmation (resolved
/// repo/ref + the will-fetch-AND-build warning) and asks before cloning; `--yes` consents,
/// `--no-input` refuses with [`ErrorCode::TemplateFetchDeclined`]. A LOCAL source skips the
/// prompt and is read verbatim. A `@scope/name` source is classified but npm-registry
/// resolution is out of scope for 0.4 (D-87): it returns a clear not-yet-supported frame.
pub fn resolve(
    source: &TemplateSource,
    raw: &str,
    accept_yes: bool,
    ctx: &Ctx,
) -> CmdResult<ResolvedTemplate> {
    match source {
        TemplateSource::Local(path) => resolve_local(path, raw),
        TemplateSource::Scope { scope, name } => Err(scope_not_supported(scope, name, raw)),
        TemplateSource::Gh { .. } => resolve_remote(source, raw, accept_yes, ctx),
    }
}

/// A local-path template: read verbatim. The dir must hold a `rackabel-template.toml`
/// (else `RK0402`). When the dir is itself a git repo we pin its `HEAD` so a local
/// template can still be `--update`d later; a non-git dir simply has no commit anchor.
fn resolve_local(path: &Path, raw: &str) -> CmdResult<ResolvedTemplate> {
    if !path.is_dir() {
        return Err(RkError::of(
            ErrorCode::TemplateNotFound,
            format!("the template path `{}` is not a directory", path.display()),
            "point --template at a directory holding a rackabel-template.toml, or use \
             gh:owner/repo[@ref] for a remote template",
        )
        .at(path.display().to_string()));
    }
    let manifest = TemplateManifest::load(path)?;
    manifest.validate()?;
    // Pin the commit if it happens to be a git work tree (best-effort; a non-repo is fine).
    let commit = if path.join(".git").exists() {
        git::rev_parse_head(path, ErrorCode::TemplateNotFound).ok()
    } else {
        None
    };
    Ok(ResolvedTemplate {
        dir: path.to_path_buf(),
        manifest,
        repo: raw.to_string(),
        git_ref: None,
        commit,
        _clone: None,
    })
}

/// A remote `gh:` template: print the §5.7 confirmation, then shallow-clone it into a
/// tempdir (behind the `RACKABEL_TEMPLATE_GIT_BASE` seam) and pin its `HEAD`.
fn resolve_remote(
    source: &TemplateSource,
    raw: &str,
    accept_yes: bool,
    ctx: &Ctx,
) -> CmdResult<ResolvedTemplate> {
    let url = source.clone_url().ok_or_else(|| {
        RkError::of(
            ErrorCode::TemplateNotFound,
            format!("could not build a clone URL for `{}`", source.display()),
            "use gh:owner/repo[@ref] or a local path",
        )
    })?;
    let git_ref = source.git_ref().map(|s| s.to_string());

    confirm_remote(source, &url, accept_yes, ctx)?;

    let holder = tempfile::tempdir().map_err(|e| {
        RkError::new(
            ErrorCode::TemplateNotFound,
            ExitClass::Environment,
            "could not create a temporary directory for the template clone",
            "check that the system temp dir is writable, then retry",
        )
        .raw(e.into())
    })?;
    let dest = holder.path().join("template");
    git::clone_shallow(&url, git_ref.as_deref(), &dest, ErrorCode::TemplateNotFound)?;

    let manifest = TemplateManifest::load(&dest)?;
    manifest.validate()?;
    let commit = git::rev_parse_head(&dest, ErrorCode::TemplateNotFound).ok();

    Ok(ResolvedTemplate {
        dir: dest,
        manifest,
        repo: raw.to_string(),
        git_ref,
        commit,
        _clone: Some(holder),
    })
}

/// The §5.7 remote-confirmation gate. Prints the resolved repo/ref and the
/// will-fetch-AND-build warning, then requires consent: interactive prompt, `--yes`
/// consents non-interactively, `--no-input` REFUSES (a hard `RK0403` rather than a silent
/// default). Returns `Ok` only when consent is given.
fn confirm_remote(
    source: &TemplateSource,
    url: &str,
    accept_yes: bool,
    ctx: &Ctx,
) -> CmdResult<()> {
    // The Persona-A no-flag happy path never reaches here (it uses the built-in default
    // and a local path skips the prompt) — only an explicit remote ref does.
    if ctx.echo_on() {
        println!("This is a REMOTE template — unreviewed third-party code.");
        println!("  source:  {}", source.display());
        println!("  fetch:   {url}");
        println!("  warning: `new` will fetch this repo AND run its build configuration with your");
        println!("           full privileges (scaffold-time code execution). Only proceed if you");
        println!("           trust the source.");
    }

    if accept_yes {
        // --yes is standing consent to fetch + build the named source (§5.7).
        if ctx.echo_on() {
            println!("  (proceeding: --yes given)");
        }
        return Ok(());
    }
    if ctx.no_input {
        // --no-input must REFUSE, not silently default (§5.5/§5.7).
        return Err(RkError::of(
            ErrorCode::TemplateFetchDeclined,
            format!(
                "remote template `{}` needs confirmation, but --no-input forbids the prompt",
                source.display()
            ),
            "pass --yes to consent to fetching AND building this third-party template in a \
             script, or use the built-in default / a local path instead",
        )
        .at(source.display().to_string()));
    }

    let ok = ui::prompt::confirm("Fetch and build this remote template?", false, ctx)?;
    if !ok {
        return Err(RkError::of(
            ErrorCode::TemplateFetchDeclined,
            format!("declined to fetch remote template `{}`", source.display()),
            "rerun and confirm, or pass --yes to consent non-interactively; nothing was fetched",
        )
        .at(source.display().to_string()));
    }
    Ok(())
}

/// Public wrapper over [`confirm_remote`] so `new --update`'s remote re-fetch reuses the
/// exact same §5.7 confirmation gate (printed warning + `--yes`/`--no-input` semantics).
pub fn confirm_remote_public(
    source: &TemplateSource,
    url: &str,
    accept_yes: bool,
    ctx: &Ctx,
) -> CmdResult<()> {
    confirm_remote(source, url, accept_yes, ctx)
}

/// `@scope/name` is accepted + classified, but npm-registry resolution is out of scope for
/// 0.4 (D-87) — a clear not-yet-supported frame rather than a half-implementation.
fn scope_not_supported(scope: &str, name: &str, raw: &str) -> RkError {
    RkError::of(
        ErrorCode::TemplateNotFound,
        format!("scoped templates like `@{scope}/{name}` are not supported yet"),
        "0.4 resolves gh:owner/repo[@ref] and local paths; for a scoped package, point \
         --template at its GitHub repo (gh:owner/repo) or a local checkout instead",
    )
    .at(raw.to_string())
}

/// Run the declared prompts as the wizard, returning the answers by prompt key.
///
/// `seed` supplies pre-set answers (e.g. a `--update` re-render re-uses saved answers, and
/// only prompts for keys NOT in `seed`). Under `--yes`/`--no-input` every prompt resolves
/// to its seed/default (a prompt with no default + no seed under `--no-input` is a usage
/// error — there is nothing to invent).
pub fn run_prompts(
    manifest: &TemplateManifest,
    seed: &BTreeMap<String, String>,
    accept_yes: bool,
    ctx: &Ctx,
) -> CmdResult<BTreeMap<String, String>> {
    let mut answers = BTreeMap::new();
    let accept_defaults = accept_yes || ctx.no_input;

    for (key, prompt) in &manifest.prompts {
        // A seeded answer (from a saved lockfile or an explicit value) is used verbatim
        // and never re-prompted — this is the §5.5 "re-prompt only for NEW prompts" rule.
        if let Some(v) = seed.get(key) {
            answers.insert(key.clone(), v.clone());
            continue;
        }
        let value = ask_one(key, prompt, accept_defaults, ctx)?;
        answers.insert(key.clone(), value);
    }
    Ok(answers)
}

/// Ask (or resolve) one prompt.
fn ask_one(key: &str, prompt: &Prompt, accept_defaults: bool, ctx: &Ctx) -> CmdResult<String> {
    let label = prompt.label.clone().unwrap_or_else(|| key.to_string());
    match prompt.kind {
        PromptType::String => {
            if accept_defaults {
                return match &prompt.default {
                    Some(d) => Ok(d.clone()),
                    None => Err(missing_answer(key, &label, ctx)),
                };
            }
            ui::prompt::text(&label, prompt.default.as_deref(), ctx)
        }
        PromptType::Bool => {
            let default = matches!(prompt.default.as_deref(), Some("true"));
            if accept_defaults {
                return Ok(bool_str(default));
            }
            let v = ui::prompt::confirm(&label, default, ctx)?;
            Ok(bool_str(v))
        }
        PromptType::Choice => {
            if prompt.choices.is_empty() {
                // validate() already rejects this; defensive.
                return Err(missing_answer(key, &label, ctx));
            }
            if accept_defaults {
                return match &prompt.default {
                    Some(d) => Ok(d.clone()),
                    // No default for a choice under non-interactive: take the first
                    // (deterministic) choice and echo it, rather than dead-ending.
                    None => Ok(prompt.choices[0].clone()),
                };
            }
            let idx = ui::prompt::select(&label, &prompt.choices, ctx)?;
            Ok(prompt.choices.get(idx).cloned().unwrap_or_default())
        }
    }
}

fn bool_str(b: bool) -> String {
    if b {
        "true".to_string()
    } else {
        "false".to_string()
    }
}

fn missing_answer(key: &str, label: &str, _ctx: &Ctx) -> RkError {
    RkError::new(
        ErrorCode::ManifestIncomplete,
        ExitClass::Usage,
        format!("template prompt `{key}` ({label}) has no value and no default"),
        "rerun without --no-input to answer it, or add a `default = …` to the prompt in \
         the template's rackabel-template.toml",
    )
}

/// Copy the template tree at `template_dir` into `dest`, substituting `{{ key }}`
/// placeholders in text files. The template's own `rackabel-template.toml`, any
/// `.rackabel-template` lockfile, and the `.git` dir are NOT copied. Files matching the
/// `[merge].exclude` globs are copied VERBATIM (no substitution) — they are
/// binary/generated (vendored tarballs etc.) and must not be mangled.
///
/// `dest` is created if absent. Returns the relative paths written (for the caller's
/// summary), sorted.
pub fn render_tree(
    template_dir: &Path,
    dest: &Path,
    answers: &BTreeMap<String, String>,
    exclude: &ExcludeSet,
) -> CmdResult<Vec<String>> {
    std::fs::create_dir_all(dest).map_err(|e| io_err(dest, e))?;
    let mut written = Vec::new();
    copy_recursive(
        template_dir,
        template_dir,
        dest,
        answers,
        exclude,
        &mut written,
    )?;
    written.sort();
    Ok(written)
}

fn copy_recursive(
    root: &Path,
    src: &Path,
    dest: &Path,
    answers: &BTreeMap<String, String>,
    exclude: &ExcludeSet,
    written: &mut Vec<String>,
) -> CmdResult<()> {
    for entry in std::fs::read_dir(src).map_err(|e| io_err(src, e))? {
        let entry = entry.map_err(|e| io_err(src, e))?;
        let from = entry.path();
        let name = entry.file_name();

        // Skip the template scaffolding itself + VCS metadata at any depth.
        if name == ".git" || name == TEMPLATE_MANIFEST_NAME || name == TEMPLATE_LOCK_NAME {
            continue;
        }

        let rel = from
            .strip_prefix(root)
            .expect("entry under root")
            .to_path_buf();
        let to = dest.join(&rel);

        if from.is_dir() {
            std::fs::create_dir_all(&to).map_err(|e| io_err(&to, e))?;
            copy_recursive(root, &from, dest, answers, exclude, written)?;
            continue;
        }

        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }

        // Excluded (or binary) files: copy verbatim, never substitute.
        if exclude.is_excluded(&rel_str) {
            std::fs::copy(&from, &to).map_err(|e| io_err(&to, e))?;
            written.push(rel_str);
            continue;
        }

        match std::fs::read(&from) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => {
                    let rendered = placeholder::render(&text, answers);
                    std::fs::write(&to, rendered).map_err(|e| io_err(&to, e))?;
                }
                Err(e) => {
                    // Non-UTF8: a binary file the author didn't list in [merge].exclude.
                    // Copy verbatim rather than corrupt it.
                    std::fs::write(&to, e.into_bytes()).map_err(|e| io_err(&to, e))?;
                }
            },
            Err(e) => return Err(io_err(&from, e)),
        }
        written.push(rel_str);
    }
    Ok(())
}

/// Write the `.rackabel-template` lockfile into `dest` recording the origin + answers so
/// `new --update` can reconstruct the baseline (§5.5).
pub fn write_lock(
    resolved: &ResolvedTemplate,
    dest: &Path,
    answers: &BTreeMap<String, String>,
) -> CmdResult<()> {
    let lock = TemplateLock {
        repo: resolved.repo.clone(),
        r#ref: resolved.git_ref.clone(),
        commit: resolved.commit.clone(),
        answers: answers.clone(),
    };
    lock.save(dest)
}

fn io_err(path: &Path, e: std::io::Error) -> RkError {
    RkError::new(
        ErrorCode::DeployCopyFailed,
        ExitClass::BuildRuntime,
        "could not write a template file",
        "check write permissions for the target directory, then retry",
    )
    .at(path.display().to_string())
    .raw(e.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::color::ColorMode;
    use tempfile::tempdir;

    fn ctx(no_input: bool, home: &Path) -> Ctx {
        Ctx {
            no_input,
            json: false,
            quiet: false,
            verbose: false,
            raw: false,
            color: ColorMode::Never,
            color_err: ColorMode::Never,
            cwd: home.to_path_buf(),
            rackabel_home: home.join(".rackabel"),
            home: home.to_path_buf(),
            ableton_app: None,
            ableton_user_library: None,
            ableton_eh_mod: None,
            ableton_eh_node: None,
            ableton_extensions_dir: None,
            ableton_storage_base: None,
            rackabel_host_cmd: None,
        }
    }

    fn write_template(dir: &Path) {
        std::fs::write(
            dir.join(TEMPLATE_MANIFEST_NAME),
            r#"
[prompts.name]
label = "Name"
type = "string"
default = "my-ext"

[prompts.fancy]
type = "bool"
default = "false"

[merge]
exclude = ["vendor/**"]
"#,
        )
        .unwrap();
        std::fs::write(dir.join("README.md"), "# {{ name }}\nfancy={{ fancy }}\n").unwrap();
        std::fs::create_dir_all(dir.join("vendor")).unwrap();
        std::fs::write(
            dir.join("vendor/blob.bin"),
            "{{ name }} must NOT be substituted",
        )
        .unwrap();
    }

    #[test]
    fn local_resolve_reads_manifest() {
        let tmp = tempdir().unwrap();
        write_template(tmp.path());
        let r = resolve_local(tmp.path(), "./tpl").unwrap();
        assert_eq!(r.manifest.prompts.len(), 2);
        assert_eq!(r.repo, "./tpl");
        assert!(r.commit.is_none());
    }

    #[test]
    fn local_resolve_without_manifest_is_not_found() {
        let tmp = tempdir().unwrap();
        let err = resolve_local(tmp.path(), "./tpl").unwrap_err();
        assert_eq!(err.code, ErrorCode::TemplateNotFound);
    }

    #[test]
    fn prompts_seeded_answers_are_not_reprompted() {
        let tmp = tempdir().unwrap();
        write_template(tmp.path());
        let m = TemplateManifest::load(tmp.path()).unwrap();
        let mut seed = BTreeMap::new();
        seed.insert("name".to_string(), "seeded".to_string());
        // no_input so an UNSEEDED prompt would use its default (not prompt).
        let home = tempdir().unwrap();
        let answers = run_prompts(&m, &seed, false, &ctx(true, home.path())).unwrap();
        assert_eq!(answers["name"], "seeded");
        assert_eq!(answers["fancy"], "false"); // default
    }

    #[test]
    fn render_tree_substitutes_text_and_skips_excluded_and_manifest() {
        let src = tempdir().unwrap();
        write_template(src.path());
        let dst = tempdir().unwrap();
        let m = TemplateManifest::load(src.path()).unwrap();
        let exclude = ExcludeSet::new(&m.merge.exclude);
        let mut answers = BTreeMap::new();
        answers.insert("name".to_string(), "clip-renamer".to_string());
        answers.insert("fancy".to_string(), "true".to_string());

        let written = render_tree(src.path(), dst.path(), &answers, &exclude).unwrap();

        // The manifest is NOT copied.
        assert!(!dst.path().join(TEMPLATE_MANIFEST_NAME).exists());
        // README placeholders substituted.
        let readme = std::fs::read_to_string(dst.path().join("README.md")).unwrap();
        assert_eq!(readme, "# clip-renamer\nfancy=true\n");
        // Excluded vendor file copied VERBATIM (placeholders untouched).
        let blob = std::fs::read_to_string(dst.path().join("vendor/blob.bin")).unwrap();
        assert!(blob.contains("{{ name }}"));
        assert!(written.contains(&"README.md".to_string()));
        assert!(written.contains(&"vendor/blob.bin".to_string()));
    }

    #[test]
    fn scope_source_is_not_supported_frame() {
        let home = tempdir().unwrap();
        let src = TemplateSource::parse("@acme/starter").unwrap();
        let err = match resolve(&src, "@acme/starter", false, &ctx(true, home.path())) {
            Err(e) => e,
            Ok(_) => panic!("expected a not-supported frame"),
        };
        assert_eq!(err.code, ErrorCode::TemplateNotFound);
        assert!(err.problem.contains("not supported yet"));
    }

    #[test]
    fn remote_no_input_refuses_without_yes() {
        let home = tempdir().unwrap();
        let src = TemplateSource::parse("gh:owner/repo").unwrap();
        let err = match resolve(&src, "gh:owner/repo", false, &ctx(true, home.path())) {
            Err(e) => e,
            Ok(_) => panic!("expected a fetch-declined frame"),
        };
        assert_eq!(err.code, ErrorCode::TemplateFetchDeclined);
    }
}
