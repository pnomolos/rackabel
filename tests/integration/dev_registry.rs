//! Registry CRUD verbs end to end (REGISTRY agent). These drive the real binary
//! against a hermetic `RACKABEL_HOME`; they never touch Live, the host, or the socket
//! (the registry is operable with a dead daemon, DESIGN §3.2). The trycmd transcripts
//! cover the single-project happy path + JSON shapes; here we cover the behaviors that
//! need a fabricated monorepo on disk (recursive workspace registration + library
//! skip) and the cwd-default register.

use std::path::Path;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use crate::common::*;

fn write_ext(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("rackabel.toml"),
        format!("[extension]\nname = \"{name}\"\nauthor = \"t\"\n"),
    )
    .unwrap();
}

fn write_workspace(dir: &Path, members: &[&str]) {
    std::fs::create_dir_all(dir).unwrap();
    let list = members
        .iter()
        .map(|m| format!("\"{m}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        dir.join("rackabel.toml"),
        format!("[workspace]\nmembers = [{list}]\n"),
    )
    .unwrap();
}

/// `register` with no PATH defaults to the cwd project; the entry name is the dir
/// basename.
#[test]
fn register_defaults_to_cwd() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let proj = work.path().join("cool-ext");
    write_ext(&proj, "cool");

    rackabel_cmd(home.path(), &proj)
        .args(["dev", "register"])
        .assert()
        .success()
        .stdout(predicate::str::contains("registered cool-ext"));

    rackabel_cmd(home.path(), &proj)
        .args(["dev", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"cool-ext\""));
}

/// `--recursive` over a `[workspace].members` monorepo registers each extension member
/// and SKIPS library members (no `[extension]`), per §4.4.
#[test]
fn register_recursive_workspace_skips_library() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let root = work.path().join("mono");
    write_workspace(&root, &["packages/*"]);
    write_ext(&root.join("packages/foo"), "foo");
    write_ext(&root.join("packages/bar"), "bar");
    write_workspace(&root.join("packages/shared"), &[]); // library: skipped

    rackabel_cmd(home.path(), work.path())
        .args(["dev", "register", "--recursive"])
        .arg(&root)
        .assert()
        .success()
        .stdout(predicate::str::contains("2 extensions registered"));

    let assert = rackabel_cmd(home.path(), work.path())
        .args(["dev", "list", "--json"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("\"name\": \"foo\""), "foo registered: {out}");
    assert!(out.contains("\"name\": \"bar\""), "bar registered: {out}");
    assert!(
        !out.contains("\"name\": \"shared\""),
        "library member must be skipped: {out}"
    );
}

/// disable → enable flips the one `enabled` flag, visible in `list --json`.
#[test]
fn disable_then_enable_round_trip() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let proj = work.path().join("toggle");
    write_ext(&proj, "toggle");
    rackabel_cmd(home.path(), &proj)
        .args(["dev", "register"])
        .assert()
        .success();

    rackabel_cmd(home.path(), &proj)
        .args(["dev", "disable", "toggle"])
        .assert()
        .success();
    let assert = rackabel_cmd(home.path(), &proj)
        .args(["dev", "list", "--json"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("\"enabled\": false"), "disabled: {out}");

    rackabel_cmd(home.path(), &proj)
        .args(["dev", "enable", "toggle"])
        .assert()
        .success();
    let assert = rackabel_cmd(home.path(), &proj)
        .args(["dev", "list", "--json"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("\"enabled\": true"), "re-enabled: {out}");
}

/// `unregister --recursive` drops every entry under a path (the recursive-register
/// inverse).
#[test]
fn unregister_recursive_removes_subtree() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let root = work.path().join("mono");
    write_workspace(&root, &["packages/*"]);
    write_ext(&root.join("packages/foo"), "foo");
    write_ext(&root.join("packages/bar"), "bar");
    rackabel_cmd(home.path(), work.path())
        .args(["dev", "register", "--recursive"])
        .arg(&root)
        .assert()
        .success();

    rackabel_cmd(home.path(), work.path())
        .args(["dev", "unregister", "--recursive"])
        .arg(&root)
        .assert()
        .success();

    let assert = rackabel_cmd(home.path(), work.path())
        .args(["dev", "list", "--json"])
        .assert()
        .success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    assert!(out.contains("\"extensions\": []"), "all removed: {out}");
}

/// Registering a path with no manifest is an environment error (RK0001), not a panic.
#[test]
fn register_missing_manifest_is_error() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    std::fs::create_dir_all(work.path().join("empty")).unwrap();
    rackabel_cmd(home.path(), work.path())
        .args(["dev", "register", "empty"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0001"));
}
