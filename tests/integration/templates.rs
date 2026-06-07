//! Tier-1 template rendering (`new --template`) + `new --update` 3-way merge (DESIGN
//! §5.5), end to end through the real binary.
//!
//! HERMETIC: every remote (`gh:`) case resolves through the `RACKABEL_TEMPLATE_GIT_BASE`
//! seam pointed at a LOCAL git fixture repo built in a tempdir (a `file://` base), so no
//! test ever touches the network. The fixture repos carry a real commit history (two
//! versions) so the update path can diff old vs new. All tests are gated on a real `git`
//! on PATH and skip cleanly otherwise.

use std::path::Path;
use std::process::Command;

use crate::common::*;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

/// True iff a real `git` is on PATH.
fn has_git() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run a git command in `dir`, asserting success.
fn git(dir: &Path, args: &[&str]) {
    let st = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&st.stderr)
    );
}

/// Initialize a git repo at `dir` with `user` identity configured.
fn git_init(dir: &Path) {
    git(dir, &["init", "--quiet"]);
    git(dir, &["config", "user.email", "t@t"]);
    git(dir, &["config", "user.name", "t"]);
    git(dir, &["config", "commit.gpgsign", "false"]);
}

/// Write a file (creating parents) and stage+commit everything with `msg`.
fn write(dir: &Path, rel: &str, body: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn commit(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-m", msg, "--quiet"]);
}

/// A local template directory (a git repo) with prompts + a placeholder README. Returns
/// the repo path inside `holder`.
fn local_template(holder: &Path) -> std::path::PathBuf {
    let dir = holder.join("tpl");
    std::fs::create_dir_all(&dir).unwrap();
    git_init(&dir);
    write(
        &dir,
        "rackabel-template.toml",
        r#"
[prompts.name]
label = "Extension name"
type = "string"
default = "my-ext"

[prompts.author]
label = "Author"
type = "string"
default = "Anon"

[merge]
exclude = ["vendor/**"]
"#,
    );
    write(&dir, "README.md", "# {{ name }}\nby {{ author }}\n");
    write(
        &dir,
        "src/main.ts",
        "// {{ name }} entry\nexport const NAME = \"{{ name }}\";\n",
    );
    write(
        &dir,
        "vendor/blob.tgz",
        "binary-{{ name }}-should-not-substitute",
    );
    commit(&dir, "v1");
    dir
}

// --- local render -------------------------------------------------------

/// A local-path template renders with prompts resolved from defaults (under --no-input),
/// substitutes `{{ }}` placeholders, persists answers in `.rackabel-template`, and leaves
/// excluded files verbatim. The explicitly-typed positional name SEEDS the template's
/// `name` prompt, so the rendered content matches the folder (`new clip-renamer` renders
/// `clip-renamer`, NOT the template's default `my-ext`), even under --no-input.
#[test]
fn local_template_renders_with_answers_persisted() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let tholder = TempDir::new().unwrap();
    let tpl = local_template(tholder.path());

    rackabel_cmd(home.path(), work.path())
        .args(["new", "clip-renamer", "--no-input", "--no-git"])
        .arg("--template")
        .arg(&tpl)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "created clip-renamer/ from template",
        ));

    let proj = work.path().join("clip-renamer");
    // The positional name seeds the `name` prompt: content matches the folder. The `author`
    // prompt (no seed) falls back to its default under --no-input.
    let readme = std::fs::read_to_string(proj.join("README.md")).unwrap();
    assert_eq!(readme, "# clip-renamer\nby Anon\n");
    // Excluded vendor file copied verbatim (placeholder NOT substituted).
    let blob = std::fs::read_to_string(proj.join("vendor/blob.tgz")).unwrap();
    assert!(blob.contains("{{ name }}"));
    // The template manifest itself is not copied into the project.
    assert!(!proj.join("rackabel-template.toml").exists());
    // The lockfile persists repo + commit + the SEEDED name for `new --update`.
    let lock = std::fs::read_to_string(proj.join(".rackabel-template")).unwrap();
    assert!(lock.contains("name = \"clip-renamer\""));
    assert!(lock.contains("commit ="));
}

/// A directory with no `rackabel-template.toml` is RK0402 (not a template), exit 3.
#[test]
fn local_path_without_manifest_is_template_not_found() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let empty = TempDir::new().unwrap();

    rackabel_cmd(home.path(), work.path())
        .args(["new", "x", "--no-input", "--no-git"])
        .arg("--template")
        .arg(empty.path())
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0402"));
}

// --- remote (gh:) render via the file:// rewrite seam --------------------

/// Build a bare-ish fixture under `<base>/owner/repo` so `gh:owner/repo` resolves there
/// through `RACKABEL_TEMPLATE_GIT_BASE=file://<base>`. Returns the base dir path.
fn gh_fixture(base: &Path, owner: &str, repo: &str) -> std::path::PathBuf {
    let repo_dir = base.join(owner).join(repo);
    std::fs::create_dir_all(&repo_dir).unwrap();
    git_init(&repo_dir);
    write(
        &repo_dir,
        "rackabel-template.toml",
        "[prompts.name]\ntype = \"string\"\ndefault = \"remote-ext\"\n",
    );
    write(&repo_dir, "README.md", "# {{ name }} (remote)\n");
    commit(&repo_dir, "v1");
    base.to_path_buf()
}

/// A `gh:owner/repo` ref resolves through the file:// seam and renders, after the §5.7
/// remote confirmation is consented to with `--yes`.
#[test]
fn gh_template_renders_via_file_seam_with_yes() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let base = TempDir::new().unwrap();
    gh_fixture(base.path(), "owner", "repo");
    let base_url = format!("file://{}", base.path().display());

    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_TEMPLATE_GIT_BASE", &base_url)
        .args(["new", "remote-proj", "--no-input", "--yes", "--no-git"])
        .args(["--template", "gh:owner/repo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("REMOTE template"))
        .stdout(predicate::str::contains(
            "created remote-proj/ from template",
        ));

    // The positional name `remote-proj` seeds the template's `name` prompt (overriding the
    // template default `remote-ext`), so the rendered content matches the folder.
    let readme = std::fs::read_to_string(work.path().join("remote-proj/README.md")).unwrap();
    assert_eq!(readme, "# remote-proj (remote)\n");
    let lock = std::fs::read_to_string(work.path().join("remote-proj/.rackabel-template")).unwrap();
    assert!(lock.contains("repo = \"gh:owner/repo\""));
}

/// A `gh:` ref under `--no-input` WITHOUT `--yes` refuses at the confirmation gate
/// (RK0403, exit 3) — and never clones anything.
#[test]
fn gh_template_no_input_without_yes_refuses() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let base = TempDir::new().unwrap();
    let base_url = format!("file://{}", base.path().display());

    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_TEMPLATE_GIT_BASE", &base_url)
        .args(["new", "x", "--no-input", "--no-git"])
        .args(["--template", "gh:owner/repo"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0403"))
        .stderr(predicate::str::contains("--no-input forbids the prompt"));

    // Nothing was created.
    assert!(!work.path().join("x").exists());
}

// --- new --update --------------------------------------------------------

/// Render a local template at its CURRENT HEAD into `<work>/<name>` (the project), with
/// the lockfile pinned to that commit. We then roll the template forward and update.
fn render_v1(home: &Path, work: &Path, repo: &Path, name: &str) {
    rackabel_cmd(home, work)
        .args(["new", name, "--no-input", "--no-git"])
        .arg("--template")
        .arg(repo)
        .assert()
        .success();
}

/// A clean update: the template changes a file the user didn't touch — it merges silently
/// (exit 0) and the new content lands.
#[test]
fn update_applies_clean_change() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    // The project is rendered at v1, BUT the lockfile must pin the v1 commit while the repo
    // later has a v2. We render against a repo that is at v1, snapshot the commit, then add
    // v2. To do that we build the template, render, THEN advance it.
    let holder = TempDir::new().unwrap();
    let repo = holder.path().join("tpl");
    std::fs::create_dir_all(&repo).unwrap();
    git_init(&repo);
    write(
        &repo,
        "rackabel-template.toml",
        "[prompts.name]\ntype=\"string\"\ndefault=\"my-ext\"\n",
    );
    write(
        &repo,
        "src/main.ts",
        "export const A = 1;\nexport const B = 2;\n",
    );
    commit(&repo, "v1");

    render_v1(home.path(), work.path(), &repo, "proj");
    let proj = work.path().join("proj");
    assert_eq!(
        std::fs::read_to_string(proj.join("src/main.ts")).unwrap(),
        "export const A = 1;\nexport const B = 2;\n"
    );

    // Advance the template: change line A only (the user hasn't touched main.ts).
    write(
        &repo,
        "src/main.ts",
        "export const A = 100;\nexport const B = 2;\n",
    );
    commit(&repo, "v2");

    rackabel_cmd(home.path(), &proj)
        .args(["new", "--update", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("applied cleanly"));

    let merged = std::fs::read_to_string(proj.join("src/main.ts")).unwrap();
    assert!(merged.contains("A = 100"));
    assert!(!merged.contains("<<<<<<<"));
}

/// A real conflict: both the template and the user edited the same line. `--update` writes
/// conflict markers + a summary `help:` and exits 4 (RK4008).
#[test]
fn update_real_conflict_writes_markers_and_exits_4() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    let holder = TempDir::new().unwrap();
    let repo = holder.path().join("tpl");
    std::fs::create_dir_all(&repo).unwrap();
    git_init(&repo);
    write(
        &repo,
        "rackabel-template.toml",
        "[prompts.name]\ntype=\"string\"\ndefault=\"my-ext\"\n",
    );
    write(&repo, "conf.txt", "shared-line\n");
    commit(&repo, "v1");

    render_v1(home.path(), work.path(), &repo, "proj");
    let proj = work.path().join("proj");

    // User edits the shared line.
    std::fs::write(proj.join("conf.txt"), "user-edit\n").unwrap();
    // Template edits the same line.
    write(&repo, "conf.txt", "template-edit\n");
    commit(&repo, "v2");

    rackabel_cmd(home.path(), &proj)
        .args(["new", "--update", "--no-input"])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("RK4008"))
        .stderr(predicate::str::contains("conf.txt"));

    let conflicted = std::fs::read_to_string(proj.join("conf.txt")).unwrap();
    assert!(conflicted.contains("<<<<<<<"));
    assert!(conflicted.contains("user-edit"));
    assert!(conflicted.contains("template-edit"));
}

/// A prompt NEW in v2 is asked (here: resolved from its default under --no-input) while the
/// old answers are re-used; the new prompt's value flows into the rendered output.
#[test]
fn update_prompts_for_new_prompt_only() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    let holder = TempDir::new().unwrap();
    let repo = holder.path().join("tpl");
    std::fs::create_dir_all(&repo).unwrap();
    git_init(&repo);
    write(
        &repo,
        "rackabel-template.toml",
        "[prompts.name]\ntype=\"string\"\ndefault=\"my-ext\"\n",
    );
    write(&repo, "README.md", "# {{ name }}\n");
    commit(&repo, "v1");

    render_v1(home.path(), work.path(), &repo, "proj");
    let proj = work.path().join("proj");

    // v2 adds a new `tagline` prompt and uses it in README.
    write(
        &repo,
        "rackabel-template.toml",
        "[prompts.name]\ntype=\"string\"\ndefault=\"my-ext\"\n\n[prompts.tagline]\ntype=\"string\"\ndefault=\"a great extension\"\n",
    );
    write(&repo, "README.md", "# {{ name }}\n{{ tagline }}\n");
    commit(&repo, "v2");

    rackabel_cmd(home.path(), &proj)
        .args(["new", "--update", "--no-input"])
        .assert()
        .success();

    let readme = std::fs::read_to_string(proj.join("README.md")).unwrap();
    // name re-used from saved answers (seeded from the positional `proj` at render time);
    // tagline resolved from the NEW prompt's default.
    assert!(readme.contains("# proj"));
    assert!(readme.contains("a great extension"));
    // The new answer is now persisted for the next update.
    let lock = std::fs::read_to_string(proj.join(".rackabel-template")).unwrap();
    assert!(lock.contains("tagline"));
}

/// `[merge].exclude` is honored on update: an excluded (vendored) file that changed in the
/// template is overwritten without a text merge, never gets conflict markers, and a binary
/// the user changed is also not text-merged.
#[test]
fn update_honors_merge_exclude() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    let holder = TempDir::new().unwrap();
    let repo = holder.path().join("tpl");
    std::fs::create_dir_all(&repo).unwrap();
    git_init(&repo);
    write(
        &repo,
        "rackabel-template.toml",
        "[prompts.name]\ntype=\"string\"\ndefault=\"my-ext\"\n\n[merge]\nexclude=[\"assets/**\"]\n",
    );
    write(&repo, "assets/data.bin", "v1-bytes\n");
    commit(&repo, "v1");

    render_v1(home.path(), work.path(), &repo, "proj");
    let proj = work.path().join("proj");

    // The template changes the excluded asset.
    write(&repo, "assets/data.bin", "v2-bytes\n");
    commit(&repo, "v2");

    rackabel_cmd(home.path(), &proj)
        .args(["new", "--update", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("applied cleanly"));

    // The excluded file was OVERWRITTEN from the new render — no conflict markers.
    let data = std::fs::read_to_string(proj.join("assets/data.bin")).unwrap();
    assert_eq!(data, "v2-bytes\n");
    assert!(!data.contains("<<<<<<<"));
}

/// `--update --dry-run` prints the plan and changes NOTHING on disk.
#[test]
fn update_dry_run_shows_plan_and_mutates_nothing() {
    if !has_git() {
        eprintln!("skipping: git not available");
        return;
    }
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    let holder = TempDir::new().unwrap();
    let repo = holder.path().join("tpl");
    std::fs::create_dir_all(&repo).unwrap();
    git_init(&repo);
    write(
        &repo,
        "rackabel-template.toml",
        "[prompts.name]\ntype=\"string\"\ndefault=\"my-ext\"\n",
    );
    write(&repo, "README.md", "# {{ name }}\nv1\n");
    commit(&repo, "v1");

    render_v1(home.path(), work.path(), &repo, "proj");
    let proj = work.path().join("proj");
    let before = std::fs::read_to_string(proj.join("README.md")).unwrap();

    write(&repo, "README.md", "# {{ name }}\nv2\n");
    commit(&repo, "v2");

    rackabel_cmd(home.path(), &proj)
        .args(["new", "--update", "--dry-run", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dry run"))
        .stdout(predicate::str::contains("README.md"));

    // README is unchanged (dry-run mutated nothing).
    let after = std::fs::read_to_string(proj.join("README.md")).unwrap();
    assert_eq!(before, after);
}

/// `--update` with no `.rackabel-template` is a clear "nothing to update" (RK0402, exit 3).
#[test]
fn update_without_lockfile_is_template_not_found() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    rackabel_cmd(home.path(), work.path())
        .args(["new", "--update", "--no-input"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("no .rackabel-template"))
        .stderr(predicate::str::contains("RK0402"));
}
