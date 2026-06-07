//! `rackabel validate` integration tests (assert_cmd-driven).
//!
//! Determinism: `rackabel_cmd` clears the inherited `ABLETON_*` overrides, so to keep
//! the host-apiVersion rule off the real machine we point `ABLETON_APP` at a
//! non-existent app (no Live detected → that rule skips). State is isolated via
//! `RACKABEL_HOME`/`HOME`. Where a test needs an empty author it suppresses git
//! config with `GIT_CONFIG_GLOBAL=/dev/null`.

use crate::common::*;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::path::Path;
use tempfile::TempDir;

/// Write a `rackabel.toml` body into `dir`.
fn write_toml(dir: &Path, body: &str) {
    std::fs::write(dir.join("rackabel.toml"), body).unwrap();
}

/// A complete, ship-ready extension passes (exit 0) with no failures.
#[test]
fn complete_project_passes() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    write_toml(
        work.path(),
        "[extension]\nname=\"Cool\"\nauthor=\"Jane\"\nversion=\"1.2.0\"\nentry=\"src/extension.ts\"\nminimum_api_version=\"1.0.0\"\n",
    );
    std::fs::write(work.path().join("CHANGELOG.md"), "## 1.2.0\n- thing\n").unwrap();

    rackabel_cmd(home.path(), work.path())
        .env("ABLETON_APP", "/nonexistent-ableton-live.app")
        .arg("validate")
        .assert()
        .success()
        .stdout(predicate::str::contains("[✓] manifest complete"))
        .stdout(predicate::str::contains(
            "CHANGELOG.md has an entry for 1.2.0",
        ))
        .stdout(predicate::str::contains("all checks passed"));
}

/// `--strict` on a clean project is still a pass (no warning-tier rule fires yet).
#[test]
fn complete_project_passes_under_strict() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    write_toml(
        work.path(),
        "[extension]\nname=\"Cool\"\nauthor=\"Jane\"\nversion=\"1.2.0\"\nentry=\"src/extension.ts\"\nminimum_api_version=\"1.0.0\"\n",
    );
    std::fs::write(work.path().join("CHANGELOG.md"), "## 1.2.0\n").unwrap();

    rackabel_cmd(home.path(), work.path())
        .env("ABLETON_APP", "/nonexistent-ableton-live.app")
        .args(["validate", "--strict"])
        .assert()
        .success();
}

/// A missing author (with git config suppressed) fails completeness — exit 4, framed,
/// carrying RK4001 so `rackabel explain RK4001` is discoverable from the error.
#[test]
fn missing_author_fails_validation() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    write_toml(
        work.path(),
        "[extension]\nname=\"Cool\"\nversion=\"1.2.0\"\nentry=\"src/extension.ts\"\nminimum_api_version=\"1.0.0\"\n",
    );
    std::fs::write(work.path().join("CHANGELOG.md"), "## 1.2.0\n").unwrap();

    rackabel_cmd(home.path(), work.path())
        .env("ABLETON_APP", "/nonexistent-ableton-live.app")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .arg("validate")
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("[✗] manifest incomplete"))
        .stderr(predicate::str::contains("error: validation failed"))
        .stderr(predicate::str::contains("RK4001"))
        .stderr(predicate::str::contains("help:"));
}

/// A complete manifest with no CHANGELOG.md fails the changelog rule (exit 4).
#[test]
fn missing_changelog_fails_validation() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    write_toml(
        work.path(),
        "[extension]\nname=\"Cool\"\nauthor=\"Jane\"\nversion=\"1.2.0\"\nentry=\"src/extension.ts\"\nminimum_api_version=\"1.0.0\"\n",
    );

    rackabel_cmd(home.path(), work.path())
        .env("ABLETON_APP", "/nonexistent-ableton-live.app")
        .arg("validate")
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("CHANGELOG.md not found"))
        .stderr(predicate::str::contains("RK4001"));
}

/// A version equal to the last packed version fails the version-bump rule (exit 4)
/// and reports the precise code RK4003.
#[test]
fn stale_version_fails_with_rk4003() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    write_toml(
        work.path(),
        "[extension]\nname=\"Cool\"\nauthor=\"Jane\"\nversion=\"1.2.0\"\nentry=\"src/extension.ts\"\nminimum_api_version=\"1.0.0\"\n",
    );
    std::fs::write(work.path().join("CHANGELOG.md"), "## 1.2.0\n").unwrap();
    // Seed the sidecar so the last packed version equals the current one.
    let rk = work.path().join(".rackabel");
    std::fs::create_dir_all(&rk).unwrap();
    std::fs::write(rk.join("state.toml"), "last_packed_version = \"1.2.0\"\n").unwrap();

    rackabel_cmd(home.path(), work.path())
        .env("ABLETON_APP", "/nonexistent-ableton-live.app")
        .arg("validate")
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains(
            "is not newer than the last packed 1.2.0",
        ))
        .stderr(predicate::str::contains("RK4003"));
}

/// A declared native dep with no compiled `.node` fails (exit 4) with the
/// `deploy --fix` remedy and the precise RK0304 code.
#[test]
fn native_dep_without_dot_node_fails() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    write_toml(
        work.path(),
        "[extension]\nname=\"Cool\"\nauthor=\"Jane\"\nversion=\"1.2.0\"\nentry=\"src/extension.ts\"\nminimum_api_version=\"1.0.0\"\n\n[extension.build]\nnative_deps = [\"abletonlink\"]\n",
    );
    std::fs::write(work.path().join("CHANGELOG.md"), "## 1.2.0\n").unwrap();
    // Install the dep dir but with no .node binary.
    std::fs::create_dir_all(work.path().join("node_modules/abletonlink/build")).unwrap();

    rackabel_cmd(home.path(), work.path())
        .env("ABLETON_APP", "/nonexistent-ableton-live.app")
        .arg("validate")
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("no compiled .node binary"))
        .stderr(predicate::str::contains("RK0304"));
}

/// `--json` is machine-readable: an array of checks plus a summary, exit-coded the
/// same as the human path.
#[test]
fn json_output_is_structured_and_exit_coded() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    write_toml(
        work.path(),
        "[extension]\nname=\"Cool\"\nauthor=\"Jane\"\nversion=\"1.2.0\"\nentry=\"src/extension.ts\"\nminimum_api_version=\"1.0.0\"\n",
    );
    // No CHANGELOG -> one failure -> ok:false, exit 4.
    rackabel_cmd(home.path(), work.path())
        .env("ABLETON_APP", "/nonexistent-ableton-live.app")
        .args(["--json", "validate"])
        .assert()
        .failure()
        .code(4)
        .stdout(predicate::str::contains("\"ok\": false"))
        .stdout(predicate::str::contains("\"failed\": 1"))
        .stdout(predicate::str::contains("\"id\": \"changelog\""))
        .stdout(predicate::str::contains("\"status\": \"fail\""))
        // No bare error frame on stdout under --json (the checklist is JSON).
        .stdout(predicate::str::contains("[✗]").not());
}

/// validate outside any project surfaces the no-manifest environment error (exit 3),
/// not a validation error — environment precedes validation.
#[test]
fn no_project_is_environment_error() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    rackabel_cmd(home.path(), work.path())
        .arg("validate")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0001"));
}
