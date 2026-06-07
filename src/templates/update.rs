//! `new --update` — the copier-style 3-way template merge (DESIGN §5.5).
//!
//! `--update` is an explicit developer action over the CURRENT project; it NEVER runs on
//! the Persona-A happy path. It reads the project's `.rackabel-template` lockfile
//! (repo + ref + commit + saved answers persisted at render time), then:
//!   1. clones the template with full history (behind the `RACKABEL_TEMPLATE_GIT_BASE`
//!      seam) so it can materialize two commits;
//!   2. re-renders the OLD baseline (template@oldcommit + saved answers) → the merge BASE;
//!   3. re-renders the NEW version (template@newcommit + the SAME answers, prompting ONLY
//!      for prompts that are NEW in the updated template) → "theirs";
//!   4. treats the user's working tree as "ours" and 3-way-merges each TEXT file
//!      (`git merge-file`): clean files apply silently; conflicting files get conflict
//!      markers + a summary `help:` line (`RK4008`);
//!   5. files matching `[merge].exclude` (∪ the always-excluded binary/tarball set) are
//!      NEVER text-merged — overwritten from the new render (when changed) or left alone.
//!
//! `--dry-run` prints the plan (apply / conflict / overwrite / skip per file) and exits,
//! mutating nothing.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::plugin::git;
use crate::plugin::source::TemplateSource;
use crate::plugin::template::{TemplateLock, TemplateManifest};
use crate::ui;

use super::exclude::ExcludeSet;
use super::render;

/// Run `new --update` for the project at `ctx.cwd`. `dry_run` shows the plan without
/// touching the tree. `accept_yes` consents to the remote fetch in a script (§5.7).
pub fn run(ctx: &Ctx, dry_run: bool, accept_yes: bool) -> CmdResult<()> {
    let project = ctx.cwd.clone();

    // 1. Read the lockfile — a project NOT made from a tracked template has nothing to do.
    let Some(lock) = TemplateLock::load(&project)? else {
        return Err(RkError::of(
            ErrorCode::TemplateNotFound,
            "this project has no .rackabel-template to update from",
            "`new --update` re-runs the template that scaffolded a project; this directory \
             was not created from a tracked template (no .rackabel-template present)",
        )
        .at(project.display().to_string()));
    };

    let source = TemplateSource::parse(&lock.repo).ok_or_else(|| {
        RkError::of(
            ErrorCode::TemplateNotFound,
            format!(
                "the .rackabel-template `repo = {:?}` is not a valid template ref",
                lock.repo
            ),
            "fix the repo field in .rackabel-template (gh:owner/repo[@ref] or a local path)",
        )
    })?;

    let old_commit = lock.commit.clone().ok_or_else(|| {
        RkError::of(
            ErrorCode::TemplateNotFound,
            "this project's .rackabel-template has no pinned commit to update from",
            "--update needs a commit anchor; it is only available for templates rendered \
             from a git repo (gh:owner/repo or a local git checkout)",
        )
    })?;

    // 2. Materialize the template repo (full history) so we can check out two commits.
    let work = materialize(&source, &lock, accept_yes, ctx)?;

    // The new manifest drives the prompt set + the exclude globs.
    git::checkout(
        &work.repo_dir,
        &work.new_commit,
        ErrorCode::TemplateNotFound,
    )?;
    let new_manifest = TemplateManifest::load(&work.repo_dir)?;
    new_manifest.validate()?;

    // Up to date already?
    if work.new_commit == old_commit {
        if ctx.echo_on() {
            ui::frame::emit(
                ui::frame::Symbol::Good,
                "already up to date with the template (no new commit)",
                ctx,
            );
        }
        return Ok(());
    }

    // 3. Answers: re-use the saved ones; prompt ONLY for prompts new in the updated set.
    let answers = render::run_prompts(&new_manifest, &lock.answers, accept_yes, ctx)?;

    // 4. Render OLD baseline (base) and NEW (theirs) into temp dirs.
    let base_dir = tempfile::tempdir().map_err(temp_err)?;
    let new_dir = tempfile::tempdir().map_err(temp_err)?;

    let old_exclude = {
        git::checkout(&work.repo_dir, &old_commit, ErrorCode::TemplateNotFound)?;
        let old_manifest = TemplateManifest::load(&work.repo_dir)?;
        let ex = ExcludeSet::new(&old_manifest.merge.exclude);
        // The old baseline only needs the answers that EXISTED then; extra keys are
        // harmless (unknown placeholders are left verbatim, but answers are a superset).
        render::render_tree(&work.repo_dir, base_dir.path(), &lock.answers, &ex)?;
        ex
    };
    let new_exclude = {
        git::checkout(
            &work.repo_dir,
            &work.new_commit,
            ErrorCode::TemplateNotFound,
        )?;
        let ex = ExcludeSet::new(&new_manifest.merge.exclude);
        render::render_tree(&work.repo_dir, new_dir.path(), &answers, &ex)?;
        ex
    };

    // 5. Plan + apply the merge.
    let plan = build_plan(
        base_dir.path(),
        new_dir.path(),
        &project,
        &new_exclude,
        &old_exclude,
    )?;

    if dry_run {
        print_plan(&plan, ctx);
        return Ok(());
    }

    apply_plan(&plan, base_dir.path(), new_dir.path(), &project, ctx)?;

    // 6. Persist the new commit + answers so the NEXT --update bases off this point.
    let new_lock = TemplateLock {
        repo: lock.repo.clone(),
        r#ref: lock.r#ref.clone(),
        commit: Some(work.new_commit.clone()),
        answers,
    };
    new_lock.save(&project)?;

    finish(&plan, ctx)
}

/// A materialized template clone plus the resolved new commit.
struct Materialized {
    repo_dir: PathBuf,
    new_commit: String,
    _holder: Option<tempfile::TempDir>,
}

/// Clone/locate the template repo with full history and resolve the NEW commit to update
/// to (the tip of the saved ref / default branch).
fn materialize(
    source: &TemplateSource,
    lock: &TemplateLock,
    accept_yes: bool,
    ctx: &Ctx,
) -> CmdResult<Materialized> {
    match source {
        TemplateSource::Local(path) => {
            if !path.join(".git").exists() {
                return Err(RkError::of(
                    ErrorCode::TemplateNotFound,
                    "the local template is not a git repository, so --update can't diff commits",
                    "--update needs a git history; re-scaffold from a git checkout or a gh: ref",
                )
                .at(path.display().to_string()));
            }
            // CLONE the local repo into a tempdir (a file:// remote) rather than checking
            // out commits in the user's own template work tree — `--update` must not mutate
            // the source repo's HEAD/working tree.
            let abs = path.canonicalize().unwrap_or_else(|_| path.clone());
            let url = format!("file://{}", abs.display());
            let holder = tempfile::tempdir().map_err(temp_err)?;
            let dest = holder.path().join("template");
            git::clone_full(
                &url,
                lock.r#ref.as_deref(),
                &dest,
                ErrorCode::TemplateNotFound,
            )?;
            let new_commit = git::rev_parse_head(&dest, ErrorCode::TemplateNotFound)?;
            Ok(Materialized {
                repo_dir: dest,
                new_commit,
                _holder: Some(holder),
            })
        }
        TemplateSource::Scope { .. } => Err(RkError::of(
            ErrorCode::TemplateNotFound,
            "scoped templates are not supported yet (so neither is --update for them)",
            "re-scaffold from the GitHub repo (gh:owner/repo) or a local checkout",
        )),
        TemplateSource::Gh { .. } => {
            let url = source.clone_url().ok_or_else(|| {
                RkError::of(
                    ErrorCode::TemplateNotFound,
                    "could not build a clone URL for the template",
                    "fix the repo field in .rackabel-template",
                )
            })?;
            // --update fetches + (re-)builds: same §5.7 confirmation as a fresh remote.
            render::confirm_remote_public(source, &url, accept_yes, ctx)?;

            let holder = tempfile::tempdir().map_err(temp_err)?;
            let dest = holder.path().join("template");
            git::clone_full(
                &url,
                lock.r#ref.as_deref(),
                &dest,
                ErrorCode::TemplateNotFound,
            )?;
            let new_commit = git::rev_parse_head(&dest, ErrorCode::TemplateNotFound)?;
            Ok(Materialized {
                repo_dir: dest,
                new_commit,
                _holder: Some(holder),
            })
        }
    }
}

/// What `--update` will do to one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    /// New file in the template not in the project: created.
    Create,
    /// Text file changed in the template; merged cleanly into the project.
    MergeClean,
    /// Text file changed in both; conflict markers written.
    Conflict,
    /// Excluded (binary/generated) file that changed: overwritten from the new render.
    Overwrite,
    /// Unchanged between old & new template render: nothing to do.
    Unchanged,
}

struct PlanItem {
    rel: String,
    action: Action,
}

struct Plan {
    items: Vec<PlanItem>,
}

impl Plan {
    fn conflicts(&self) -> Vec<&str> {
        self.items
            .iter()
            .filter(|i| i.action == Action::Conflict)
            .map(|i| i.rel.as_str())
            .collect()
    }
    fn changed(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.action != Action::Unchanged)
            .count()
    }
}

/// Build the merge plan by walking the union of files in the OLD and NEW template renders.
/// A file the template never produced (purely the user's own) is not in the plan and is
/// left untouched, by construction.
fn build_plan(
    base: &Path,
    new: &Path,
    project: &Path,
    new_exclude: &ExcludeSet,
    _old_exclude: &ExcludeSet,
) -> CmdResult<Plan> {
    let mut rels: BTreeSet<String> = BTreeSet::new();
    collect_rel(base, base, &mut rels)?;
    collect_rel(new, new, &mut rels)?;

    let mut items = Vec::new();
    for rel in rels {
        let in_base = base.join(&rel);
        let in_new = new.join(&rel);
        let in_proj = project.join(&rel);

        let new_bytes = read_opt(&in_new)?;
        let base_bytes = read_opt(&in_base)?;

        // A file only present in the OLD render (template removed it): leave the user's
        // copy alone — we never delete the user's files on update.
        let Some(new_bytes) = new_bytes else {
            items.push(PlanItem {
                rel,
                action: Action::Unchanged,
            });
            continue;
        };

        // Excluded (binary/generated): overwrite from new IFF it changed vs base; never
        // text-merge.
        if new_exclude.is_excluded(&rel) {
            let changed = base_bytes.as_deref() != Some(new_bytes.as_slice());
            items.push(PlanItem {
                rel,
                action: if changed {
                    Action::Overwrite
                } else {
                    Action::Unchanged
                },
            });
            continue;
        }

        if !in_proj.exists() {
            // Template ships a file the user doesn't have (newly added, or the user
            // deleted it): (re)create it from the new render.
            items.push(PlanItem {
                rel,
                action: Action::Create,
            });
            continue;
        }

        // Did the template change this file between old and new?
        if base_bytes.as_deref() == Some(new_bytes.as_slice()) {
            items.push(PlanItem {
                rel,
                action: Action::Unchanged,
            });
            continue;
        }

        // The template changed it; merge against the user's copy. Decide clean vs conflict
        // by a trial merge on a scratch copy (the real apply re-runs it in place).
        let clean = trial_merge(&in_proj, &in_base, &in_new)?;
        items.push(PlanItem {
            rel,
            action: if clean {
                Action::MergeClean
            } else {
                Action::Conflict
            },
        });
    }
    Ok(Plan { items })
}

/// Trial-merge into a scratch copy of the user's file to classify clean vs conflict
/// without mutating the project (the dry-run plan must not write).
fn trial_merge(proj_file: &Path, base: &Path, new: &Path) -> CmdResult<bool> {
    let scratch = tempfile::NamedTempFile::new().map_err(temp_err)?;
    std::fs::copy(proj_file, scratch.path()).map_err(|e| io_err(scratch.path(), e))?;
    git::merge_file(scratch.path(), base, new, ErrorCode::UpdateConflicts)
}

/// Apply the plan in place.
fn apply_plan(plan: &Plan, base: &Path, new: &Path, project: &Path, ctx: &Ctx) -> CmdResult<()> {
    for item in &plan.items {
        let in_new = new.join(&item.rel);
        let in_base = base.join(&item.rel);
        let in_proj = project.join(&item.rel);
        match item.action {
            Action::Unchanged => {}
            Action::Create | Action::Overwrite => {
                if let Some(parent) = in_proj.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
                }
                std::fs::copy(&in_new, &in_proj).map_err(|e| io_err(&in_proj, e))?;
            }
            Action::MergeClean | Action::Conflict => {
                // In-place 3-way merge (writes conflict markers on conflict).
                git::merge_file(&in_proj, &in_base, &in_new, ErrorCode::UpdateConflicts)?;
            }
        }
        if ctx.echo_on()
            && let Some(line) = applied_line(item)
        {
            println!("  {line}");
        }
    }
    Ok(())
}

fn applied_line(item: &PlanItem) -> Option<String> {
    let verb = match item.action {
        Action::Create => "created",
        Action::MergeClean => "merged",
        Action::Conflict => "CONFLICT",
        Action::Overwrite => "overwrote",
        Action::Unchanged => return None,
    };
    Some(format!("{verb:>9}  {}", item.rel))
}

/// Print the dry-run plan without mutating anything.
fn print_plan(plan: &Plan, ctx: &Ctx) {
    if !ctx.echo_on() {
        return;
    }
    println!("update plan (dry run — nothing was changed):");
    let mut any = false;
    for item in &plan.items {
        let verb = match item.action {
            Action::Create => "create",
            Action::MergeClean => "merge",
            Action::Conflict => "conflict",
            Action::Overwrite => "overwrite",
            Action::Unchanged => continue,
        };
        any = true;
        println!("  {verb:>9}  {}", item.rel);
    }
    if !any {
        println!("  (nothing to do — already up to date)");
    }
    let conflicts = plan.conflicts();
    if !conflicts.is_empty() {
        println!(
            "  {} file(s) would conflict; rerun without --dry-run to write conflict markers.",
            conflicts.len()
        );
    }
}

/// Final summary: success, or `RK4008` listing the conflicting files.
fn finish(plan: &Plan, ctx: &Ctx) -> CmdResult<()> {
    let conflicts = plan.conflicts();
    if conflicts.is_empty() {
        if ctx.echo_on() {
            let n = plan.changed();
            ui::frame::emit(
                ui::frame::Symbol::Good,
                &format!("template update applied cleanly ({n} file(s) changed)"),
                ctx,
            );
        }
        return Ok(());
    }
    let list = conflicts.join(", ");
    Err(RkError::of(
        ErrorCode::UpdateConflicts,
        format!(
            "template update left {} file(s) with conflicts",
            conflicts.len()
        ),
        format!(
            "resolve the <<<<<<< / ======= / >>>>>>> markers in: {list} — then save. \
             (clean files were applied; this is a deliberate developer action, so nothing \
             on the Persona-A happy path was touched.)"
        ),
    ))
}

// --- helpers ---------------------------------------------------------------

fn collect_rel(root: &Path, dir: &Path, out: &mut BTreeSet<String>) -> CmdResult<()> {
    for entry in std::fs::read_dir(dir).map_err(|e| io_err(dir, e))? {
        let entry = entry.map_err(|e| io_err(dir, e))?;
        let p = entry.path();
        if p.is_dir() {
            collect_rel(root, &p, out)?;
        } else {
            let rel = p
                .strip_prefix(root)
                .expect("under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.insert(rel);
        }
    }
    Ok(())
}

fn read_opt(p: &Path) -> CmdResult<Option<Vec<u8>>> {
    match std::fs::read(p) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(p, e)),
    }
}

fn temp_err(e: std::io::Error) -> RkError {
    RkError::new(
        ErrorCode::TemplateNotFound,
        ExitClass::Environment,
        "could not create a temporary directory for the update merge",
        "check that the system temp dir is writable, then retry",
    )
    .raw(e.into())
}

fn io_err(path: &Path, e: std::io::Error) -> RkError {
    RkError::new(
        ErrorCode::DeployCopyFailed,
        ExitClass::BuildRuntime,
        "could not read/write a file during the template update",
        "check permissions on the project directory, then retry",
    )
    .at(path.display().to_string())
    .raw(e.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_classifies_actions() {
        let base = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();

        // unchanged: same in base & new, present in project.
        std::fs::write(base.path().join("same.txt"), "x\n").unwrap();
        std::fs::write(new.path().join("same.txt"), "x\n").unwrap();
        std::fs::write(proj.path().join("same.txt"), "x\n").unwrap();

        // clean merge: template changed a line the user didn't touch.
        std::fs::write(base.path().join("a.txt"), "line1\nline2\n").unwrap();
        std::fs::write(new.path().join("a.txt"), "line1-new\nline2\n").unwrap();
        std::fs::write(proj.path().join("a.txt"), "line1\nline2\n").unwrap();

        // conflict: both changed the same line.
        std::fs::write(base.path().join("c.txt"), "v0\n").unwrap();
        std::fs::write(new.path().join("c.txt"), "v-template\n").unwrap();
        std::fs::write(proj.path().join("c.txt"), "v-user\n").unwrap();

        // create: new file not in project.
        std::fs::write(new.path().join("fresh.txt"), "hi\n").unwrap();

        // overwrite: excluded binary changed.
        std::fs::write(base.path().join("blob.tgz"), "old").unwrap();
        std::fs::write(new.path().join("blob.tgz"), "new").unwrap();
        std::fs::write(proj.path().join("blob.tgz"), "old").unwrap();

        let ex = ExcludeSet::new(&[]);
        let plan = build_plan(base.path(), new.path(), proj.path(), &ex, &ex).unwrap();

        let by = |name: &str| plan.items.iter().find(|i| i.rel == name).unwrap().action;
        assert_eq!(by("same.txt"), Action::Unchanged);
        assert_eq!(by("a.txt"), Action::MergeClean);
        assert_eq!(by("c.txt"), Action::Conflict);
        assert_eq!(by("fresh.txt"), Action::Create);
        assert_eq!(by("blob.tgz"), Action::Overwrite);
    }

    #[test]
    fn apply_writes_clean_and_conflict_markers() {
        let base = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        let proj = tempfile::tempdir().unwrap();

        std::fs::write(base.path().join("c.txt"), "v0\n").unwrap();
        std::fs::write(new.path().join("c.txt"), "v-template\n").unwrap();
        std::fs::write(proj.path().join("c.txt"), "v-user\n").unwrap();
        std::fs::write(base.path().join("a.txt"), "l1\nl2\n").unwrap();
        std::fs::write(new.path().join("a.txt"), "l1-new\nl2\n").unwrap();
        std::fs::write(proj.path().join("a.txt"), "l1\nl2\n").unwrap();

        let ex = ExcludeSet::new(&[]);
        let plan = build_plan(base.path(), new.path(), proj.path(), &ex, &ex).unwrap();
        let home = tempfile::tempdir().unwrap();
        let ctx = test_ctx(home.path());
        apply_plan(&plan, base.path(), new.path(), proj.path(), &ctx).unwrap();

        let merged = std::fs::read_to_string(proj.path().join("a.txt")).unwrap();
        assert!(merged.contains("l1-new"));
        let conflicted = std::fs::read_to_string(proj.path().join("c.txt")).unwrap();
        assert!(conflicted.contains("<<<<<<<"));
        assert!(conflicted.contains(">>>>>>>"));
        assert!(conflicted.contains("v-user"));
        assert!(conflicted.contains("v-template"));
    }

    fn test_ctx(home: &Path) -> Ctx {
        use crate::ui::color::ColorMode;
        Ctx {
            no_input: true,
            json: false,
            quiet: true,
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
}
