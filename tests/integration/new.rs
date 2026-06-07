//! `rackabel new` (Extension) integration tests.
//!
//! These exercise the paths that the trycmd transcripts can't make deterministic on a
//! developer's real machine (where Ableton Live and a PATH node may be present): the
//! happy scaffold with a FIXTURE toolkit, the no-node friendly-skip, the
//! answer-persistence on the SDK-not-found re-run, and the git-init / `--no-git` /
//! `--minimal` behaviors. Every test pins env to temp dirs and a no-host fake `.app`
//! (via `ABLETON_APP`) so Live detection is deterministic, and clears `PATH` to a
//! node-free dir where the test needs the no-node skip.
//!
//! The fixture toolkit is fabricated tiny `.tgz` files under `tests/fixtures/toolkit/`
//! — we never depend on the real gated SDK/CLI tarballs in tests.

use std::path::{Path, PathBuf};

use crate::common::*;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

/// The committed fixture toolkit dir (two tiny valid `.tgz` files).
fn toolkit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/toolkit")
}

/// A fake `.app` with NO Extension Host (so `detect` returns an install with no bundled
/// node — combined with a node-free PATH, `node::any_usable` is None). We make a bare
/// directory ending in `.app`; `live::inspect` finds no host module there.
fn no_host_app(tmp: &Path) -> PathBuf {
    let app = tmp.join("Ableton Live 12 Beta.app");
    std::fs::create_dir_all(app.join("Contents")).unwrap();
    app
}

/// A PATH dir guaranteed to contain no `node` (so the PATH-node fallback fails too).
fn node_free_path(tmp: &Path) -> PathBuf {
    let dir = tmp.join("empty-bin");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Resolve a program on the real PATH (`command -v`), `None` if absent.
fn which_program(name: &str) -> Option<PathBuf> {
    let out = std::process::Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        None
    } else {
        Some(PathBuf::from(p))
    }
}

/// Symlink (or copy) `src` into `dst` so a stripped PATH can still reach a real binary.
fn link_into(src: &Path, dst: &Path) {
    #[cfg(unix)]
    {
        let _ = std::os::unix::fs::symlink(src, dst);
    }
    #[cfg(not(unix))]
    {
        let _ = std::fs::copy(src, dst);
    }
}

/// The no-node friendly-skip (DESIGN §6.2 aside): the project is created, the build is
/// skipped (no Live, no PATH node), and the exact resume instructions print. Exit 0.
#[test]
fn no_node_skip_creates_project_and_points_at_doctor() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let app = no_host_app(work.path());
    let path = node_free_path(work.path());

    rackabel_cmd(home.path(), work.path())
        .env("PATH", &path)
        .env("ABLETON_APP", &app)
        .args(["new", "clip-renamer", "--no-input", "--no-git"])
        .arg("--sdk-dir")
        .arg(toolkit_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("created clip-renamer/"))
        .stdout(predicate::str::contains("skipped the build"))
        .stdout(predicate::str::contains("rackabel doctor"))
        .stdout(predicate::str::contains("once Live is present"));

    // The full rackabel-form project exists.
    let proj = work.path().join("clip-renamer");
    assert!(proj.join("rackabel.toml").is_file());
    assert!(proj.join("package.json").is_file());
    assert!(proj.join("tsconfig.json").is_file());
    assert!(proj.join("src/extension.ts").is_file());
    assert!(proj.join(".env").is_file());
    assert!(proj.join(".gitignore").is_file());
    // Vendored toolkit, wired via file: deps.
    assert!(
        proj.join("vendor/ableton-extensions-sdk-1.0.0-beta.0.tgz")
            .is_file()
    );
    assert!(
        proj.join("vendor/ableton-extensions-cli-1.0.0-beta.0.tgz")
            .is_file()
    );
    let pkg = std::fs::read_to_string(proj.join("package.json")).unwrap();
    assert!(pkg.contains("file:./vendor/ableton-extensions-sdk-1.0.0-beta.0.tgz"));
    assert!(pkg.contains("file:./vendor/ableton-extensions-cli-1.0.0-beta.0.tgz"));
    // --no-git: no repo.
    assert!(!proj.join(".git").exists());
}

/// The happy scaffold with a fixture toolkit (DESIGN §6.2 happy path), under no-node so
/// the build is deterministically skipped — we assert the toolkit-found + created +
/// vendored lines and the full file set. (A real successful auto-build needs a node +
/// esbuild and is covered by `rackabel build`'s gated end-to-end test, not here.)
#[test]
fn happy_path_with_fixture_toolkit_scaffolds_rackabel_form() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let app = no_host_app(work.path());
    let path = node_free_path(work.path());

    rackabel_cmd(home.path(), work.path())
        .env("PATH", &path)
        .env("ABLETON_APP", &app)
        .env("HOME", home.path())
        .args(["new", "Clip Renamer", "--no-input", "--no-git"])
        .arg("--sdk-dir")
        .arg(toolkit_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "found the Ableton Extensions toolkit",
        ))
        .stdout(predicate::str::contains(
            "added it to clip-renamer/ (no internet or npm needed)",
        ))
        .stdout(predicate::str::contains("created clip-renamer/"));

    // Display name "Clip Renamer" -> dir/slug "clip-renamer" (sanitized).
    let proj = work.path().join("clip-renamer");
    assert!(proj.is_dir());
    let toml = std::fs::read_to_string(proj.join("rackabel.toml")).unwrap();
    assert!(toml.contains("name = \"Clip Renamer\""));
    // The default template ships a working command + right-click action.
    let ext = std::fs::read_to_string(proj.join("src/extension.ts")).unwrap();
    assert!(ext.contains("registerCommand"));
    assert!(ext.contains("registerContextMenuAction"));
    assert!(ext.contains("Rename this clip"));
}

/// `--minimal` emits a bare skeleton (no working example) and, being minimal, does NOT
/// git-init by default.
#[test]
fn minimal_is_bare_and_no_git_by_default() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let app = no_host_app(work.path());
    let path = node_free_path(work.path());

    rackabel_cmd(home.path(), work.path())
        .env("PATH", &path)
        .env("ABLETON_APP", &app)
        .args(["new", "bare-ext", "--no-input", "--minimal"])
        .arg("--sdk-dir")
        .arg(toolkit_dir())
        .assert()
        .success();

    let proj = work.path().join("bare-ext");
    let ext = std::fs::read_to_string(proj.join("src/extension.ts")).unwrap();
    assert!(ext.contains("export function activate"));
    assert!(!ext.contains("registerCommand"));
    // --minimal => git default off.
    assert!(!proj.join(".git").exists());
}

/// git-init is on by default for a non-minimal project (when git is available).
#[test]
fn git_init_on_by_default_for_non_minimal() {
    let git_bin = match which_program("git") {
        Some(g) => g,
        None => {
            eprintln!("skipping: git not available");
            return;
        }
    };
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let app = no_host_app(work.path());
    // A PATH dir with NO node but WITH git (symlinked), so the build skips but git-init
    // can still run.
    let path = node_free_path(work.path());
    link_into(&git_bin, &path.join("git"));

    rackabel_cmd(home.path(), work.path())
        .env("PATH", &path)
        .env("ABLETON_APP", &app)
        // git needs identity/config to init in a hermetic HOME; point config at temps.
        .env("GIT_CONFIG_GLOBAL", home.path().join("gitconfig"))
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["new", "with-git", "--no-input"])
        .arg("--sdk-dir")
        .arg(toolkit_dir())
        .assert()
        .success();

    assert!(work.path().join("with-git/.git").exists());
}

/// On SDK-not-found, the wizard answers are persisted under `$RACKABEL_HOME` so a re-run
/// can pick them up (DESIGN §6.2 "Your answers above are remembered"). We then prove the
/// re-run with a valid `--sdk-dir` succeeds (the answers seed defaults; no re-prompt).
#[test]
fn answers_persist_across_sdk_not_found_then_rerun() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let app = no_host_app(work.path());
    let path = node_free_path(work.path());

    // First run: no toolkit -> RK0201 (exit 3), answers remembered.
    rackabel_cmd(home.path(), work.path())
        .env("PATH", &path)
        .env("ABLETON_APP", &app)
        .args(["new", "clip-renamer", "--no-input", "--no-git"])
        .args(["--sdk-dir", "./definitely-empty"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0201"))
        .stderr(predicate::str::contains(
            "Your answers above are remembered",
        ));

    // The remembered-answers file exists under RACKABEL_HOME/new-answers/.
    let answers_file = home.path().join(".rackabel/new-answers/clip-renamer.toml");
    assert!(
        answers_file.is_file(),
        "expected remembered answers at {}",
        answers_file.display()
    );
    let saved = std::fs::read_to_string(&answers_file).unwrap();
    assert!(saved.contains("clip-renamer"));

    // Re-run with the real fixture toolkit: succeeds, scaffolds, and CLEARS the answers.
    rackabel_cmd(home.path(), work.path())
        .env("PATH", &path)
        .env("ABLETON_APP", &app)
        .args(["new", "clip-renamer", "--no-input", "--no-git"])
        .arg("--sdk-dir")
        .arg(toolkit_dir())
        .assert()
        .success()
        .stdout(predicate::str::contains("created clip-renamer/"));

    assert!(work.path().join("clip-renamer/rackabel.toml").is_file());
    // The answers are spent after a successful scaffold.
    assert!(!answers_file.exists());
}

/// `--kind device` still reaches the (unchanged) M4L scaffold path — proving the new
/// Extension front end didn't disturb the device branch.
#[test]
fn device_kind_still_scaffolds_m4l() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    rackabel_cmd(home.path(), work.path())
        .args(["new", "my-device", "--kind", "device", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created `my-device`"));

    let proj = work.path().join("my-device");
    assert!(proj.join("rackabel.toml").is_file());
    let toml = std::fs::read_to_string(proj.join("rackabel.toml")).unwrap();
    assert!(toml.contains("[device]"));
    assert!(proj.join("src/my-device.maxpat").is_file());
}

/// A remote `--template gh:…` under `--no-input` (no `--yes`) REFUSES at the §5.7
/// confirmation gate (RK0403, exit 3) — it must never silently fetch/build, and must never
/// fall back to the built-in default.
#[test]
fn remote_template_no_input_refuses() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    rackabel_cmd(home.path(), work.path())
        .args(["new", "x", "--no-input", "--template", "gh:user/repo"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("--no-input forbids the prompt"))
        .stderr(predicate::str::contains("RK0403"));
}

/// A malformed `--template` ref is a usage error (exit 2), caught by the frozen classifier.
#[test]
fn malformed_template_is_usage_error() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    rackabel_cmd(home.path(), work.path())
        .args(["new", "x", "--no-input", "--template", "gh:owner"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("not a valid template reference"));
}

/// `new --update` in a directory with no `.rackabel-template` is a clear "nothing to
/// update" (RK0402, exit 3), never a silent re-scaffold.
#[test]
fn update_without_template_lock_is_template_not_found() {
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

/// Scaffolding into an existing directory is a usage error (matches the device path +
/// the official scaffolder's empty-dir guard).
#[test]
fn existing_directory_is_usage_error() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    std::fs::create_dir_all(work.path().join("taken")).unwrap();

    rackabel_cmd(home.path(), work.path())
        .args(["new", "taken", "--no-input"])
        .arg("--sdk-dir")
        .arg(toolkit_dir())
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("already exists"));
}
