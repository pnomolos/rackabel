//! Integration tests for `rackabel deploy` (alias `install`) — the Extension path.
//!
//! These exercise the copy-into-fake-User-Library behavior, the `--undo` safety
//! contract, and `--json`, all hermetically: a temp `HOME`/`RACKABEL_HOME`, a fake
//! Live `.app` via `--live`, and a fake User Library via `--user-library`. Tests
//! NEVER touch the real machine.
//!
//! A deploy normally builds-if-stale, which would shell to node+esbuild. To keep
//! these tests offline/deterministic we pre-create a fresh `dist/extension.js` +
//! `manifest.json` (written *after* the source) so the build-if-stale mtime check
//! considers the bundle fresh and skips the build.

use std::fs;
use std::path::Path;
use std::time::Duration;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use crate::common::*;

/// Lay out a pure-JS extension project with a pre-built (fresh) bundle + manifest so
/// `deploy` skips the build step. Returns the project root.
fn prebuilt_project(work: &Path, slug: &str) -> std::path::PathBuf {
    let proj = work.join(slug);
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::write(
        proj.join("rackabel.toml"),
        "[extension]\nname = \"Clip Renamer\"\nauthor = \"Jane\"\nversion = \"0.1.0\"\n\
         entry = \"src/extension.ts\"\nminimum_api_version = \"1.0.0\"\n",
    )
    .unwrap();
    fs::write(
        proj.join("src/extension.ts"),
        "export function activate() {}\n",
    )
    .unwrap();

    // Write the bundle + manifest AFTER the source so build-if-stale sees them fresh.
    std::thread::sleep(Duration::from_millis(10));
    fs::create_dir_all(proj.join("dist")).unwrap();
    fs::write(
        proj.join("dist/extension.js"),
        "module.exports.activate = function(){};\n",
    )
    .unwrap();
    fs::write(
        proj.join("manifest.json"),
        r#"{"name":"Clip Renamer","author":"Jane","entry":"dist/extension.js","version":"0.1.0","minimumApiVersion":"1.0.0"}"#,
    )
    .unwrap();
    proj
}

#[test]
fn deploy_copies_manifest_and_bundle_into_user_library() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();
    let proj = prebuilt_project(work.path(), "clip-renamer");

    rackabel_cmd(home.path(), &proj)
        .arg("deploy")
        .arg("--live")
        .arg(live.app_path())
        .arg("--user-library")
        .arg(ul.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("deployed clip-renamer"));

    // The deploy target is <UserLibrary>/Extensions/<slug>/.
    let dest = ul.path().join("Extensions/clip-renamer");
    assert!(dest.join("manifest.json").is_file(), "manifest.json copied");
    assert!(
        dest.join("dist/extension.js").is_file(),
        "bundle copied under dist/"
    );
}

#[test]
fn deploy_copies_extra_dist_files() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();
    let proj = prebuilt_project(work.path(), "lidal");
    // Add an extra dist file + declare it in the manifest.
    fs::write(proj.join("dist/editor-client.js"), "console.log('ui');\n").unwrap();
    let toml = fs::read_to_string(proj.join("rackabel.toml")).unwrap();
    fs::write(
        proj.join("rackabel.toml"),
        format!("{toml}[extension.build]\nextra_dist_files = [\"editor-client.js\"]\n"),
    )
    .unwrap();

    rackabel_cmd(home.path(), &proj)
        .arg("deploy")
        .arg("--live")
        .arg(live.app_path())
        .arg("--user-library")
        .arg(ul.path())
        .assert()
        .success();

    let dest = ul.path().join("Extensions/lidal");
    assert!(
        dest.join("dist/editor-client.js").is_file(),
        "extra dist file copied under dist/"
    );
}

#[test]
fn deploy_json_reports_dest_and_slug() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();
    let proj = prebuilt_project(work.path(), "clip-renamer");

    rackabel_cmd(home.path(), &proj)
        .arg("--json")
        .arg("deploy")
        .arg("--live")
        .arg(live.app_path())
        .arg("--user-library")
        .arg(ul.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"slug\": \"clip-renamer\""))
        .stdout(predicate::str::contains("\"ok\": true"));
}

#[test]
fn install_alias_works() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();
    let proj = prebuilt_project(work.path(), "clip-renamer");

    rackabel_cmd(home.path(), &proj)
        .arg("install")
        .arg("--live")
        .arg(live.app_path())
        .arg("--user-library")
        .arg(ul.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("deployed clip-renamer"));
}

#[test]
fn undo_removes_a_rackabel_deployed_folder() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let ul = FakeUserLibrary::new();
    let proj = prebuilt_project(work.path(), "clip-renamer");

    // Fabricate a deployed folder with the rackabel shape (a manifest.json).
    let dest = ul.path().join("Extensions/clip-renamer");
    fs::create_dir_all(dest.join("dist")).unwrap();
    fs::write(dest.join("manifest.json"), "{}").unwrap();
    fs::write(dest.join("dist/extension.js"), "x").unwrap();

    rackabel_cmd(home.path(), &proj)
        .arg("deploy")
        .arg("--undo")
        .arg("--user-library")
        .arg(ul.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("removed clip-renamer"));

    assert!(!dest.exists(), "deployed folder removed");
}

#[test]
fn undo_refuses_a_non_rackabel_folder() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let ul = FakeUserLibrary::new();
    let proj = prebuilt_project(work.path(), "clip-renamer");

    // A folder that shares the slug but is NOT a rackabel deploy (no manifest.json) —
    // e.g. a user's own data. rackabel must refuse to remove it.
    let dest = ul.path().join("Extensions/clip-renamer");
    fs::create_dir_all(&dest).unwrap();
    fs::write(dest.join("important-user-data.txt"), "do not delete").unwrap();

    rackabel_cmd(home.path(), &proj)
        .arg("deploy")
        .arg("--undo")
        .arg("--user-library")
        .arg(ul.path())
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains(
            "doesn't look like a rackabel deploy",
        ));

    assert!(
        dest.join("important-user-data.txt").is_file(),
        "user data untouched"
    );
}

#[test]
fn undo_not_deployed_is_a_clean_no_op() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let ul = FakeUserLibrary::new();
    let proj = prebuilt_project(work.path(), "clip-renamer");

    rackabel_cmd(home.path(), &proj)
        .arg("deploy")
        .arg("--undo")
        .arg("--user-library")
        .arg(ul.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to undo"));
}

#[test]
fn deploy_user_library_not_found_is_environment_error() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let proj = prebuilt_project(work.path(), "clip-renamer");

    // No --user-library and no Music/Ableton under the temp HOME → RK0302.
    rackabel_cmd(home.path(), &proj)
        .arg("deploy")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0302"));
}
