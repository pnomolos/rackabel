//! Integration tests (assert_cmd-driven). One test binary; command-owners add
//! their own `mod <command>;` files here. The foundation lands `common` (the fixture
//! builder) + the cross-cutting tests below (exit codes, error framing, resolution
//! orders observed end-to-end).

mod build;
mod common;
mod deploy;
// The managed dev host surface + FakeHost fixture tests (milestone 0.3, Unix-only).
#[cfg(unix)]
mod dev;
// The hermetic daemon-lifecycle tests (DAEMON-CORE): real binary + FakeHost seam.
#[cfg(unix)]
mod dev_daemon;
mod doctor;
mod new;
mod pack;
mod validate;

use assert_cmd::prelude::*;
use common::*;
use predicates::prelude::*;
use tempfile::TempDir;

/// Bare `rackabel` with no subcommand is a usage error (exit 2), per clap.
#[test]
fn bare_invocation_is_usage_error() {
    let home = TempDir::new().unwrap();
    rackabel_cmd(home.path(), home.path())
        .assert()
        .failure()
        .code(2);
}

/// `--help` exits 0 (DESIGN deviation from the official CLI's bare-exit-1).
#[test]
fn help_exits_zero() {
    let home = TempDir::new().unwrap();
    rackabel_cmd(home.path(), home.path())
        .arg("--help")
        .assert()
        .success();
}

/// An unknown error code to `explain` is a usage error (exit 2) and lists valid codes.
#[test]
fn explain_unknown_code_is_usage() {
    let home = TempDir::new().unwrap();
    rackabel_cmd(home.path(), home.path())
        .args(["explain", "RK9999"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("no such error code"))
        .stderr(predicate::str::contains("RK0001"));
}

/// `explain RK0001` succeeds and prints the long-form prose.
#[test]
fn explain_known_code_succeeds() {
    let home = TempDir::new().unwrap();
    rackabel_cmd(home.path(), home.path())
        .args(["explain", "RK0001"])
        .assert()
        .success()
        .stdout(predicate::str::contains("RK0001"))
        .stdout(predicate::str::contains("rackabel.toml"));
}

/// `build` with no manifest is an environment error (exit 3) with the three-part frame.
#[test]
fn build_without_manifest_is_environment_error() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    rackabel_cmd(home.path(), work.path())
        .arg("build")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("error: no manifest found"))
        .stderr(predicate::str::contains("--> looked for rackabel.toml"))
        .stderr(predicate::str::contains("help:"))
        .stderr(predicate::str::contains("RK0001"));
}

/// A device project's `build` reaches the (unchanged) M4L assembly stub — proving
/// the `[device]` path still dispatches correctly through the new manifest model.
#[test]
fn device_build_dispatches_to_m4l_path() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let proj = work.path().join("my-device");
    std::fs::create_dir_all(proj.join("src")).unwrap();
    std::fs::write(
        proj.join("rackabel.toml"),
        "[device]\nname = \"my-device\"\nkind = \"audio-effect\"\nentry = \"src/my-device.maxpat\"\n",
    )
    .unwrap();
    std::fs::write(proj.join("src/my-device.maxpat"), "{}").unwrap();

    rackabel_cmd(home.path(), &proj)
        .arg("build")
        .assert()
        .failure()
        .code(1) // build/runtime: device assembly not implemented yet
        .stderr(predicate::str::contains(
            "device `build` isn't implemented yet",
        ));
}

/// A project declaring both kinds is an ambiguous-kind environment error (exit 3).
#[test]
fn both_kinds_is_ambiguous() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    std::fs::write(
        work.path().join("rackabel.toml"),
        "[extension]\n[device]\nname=\"d\"\nkind=\"audio-effect\"\nentry=\"x.maxpat\"\n",
    )
    .unwrap();
    rackabel_cmd(home.path(), work.path())
        .arg("build")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0002"));
}

/// A syntactically-malformed rackabel.toml is a parse error (RK0003, exit 3) end to
/// end through a command, distinct from the both-kinds-declared (RK0002) case.
#[test]
fn malformed_manifest_is_parse_error() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    // Unterminated string => invalid TOML.
    std::fs::write(
        work.path().join("rackabel.toml"),
        "[extension]\nname = \"unterminated\n",
    )
    .unwrap();
    rackabel_cmd(home.path(), work.path())
        .arg("build")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0003"));
}

/// The fake Live fixture is detected via `--live` (the testability seam): a
/// `deploy --dry-run` of an extension resolves the target and prints the plan,
/// proving discovery + dispatch + User-Library resolution wire up end-to-end without
/// building or copying anything.
#[test]
fn extension_deploy_dry_run_with_fake_env() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let proj = work.path().join("clip-renamer");
    std::fs::create_dir_all(&proj).unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    std::fs::write(
        proj.join("rackabel.toml"),
        "[extension]\nname = \"Clip Renamer\"\nauthor = \"Jane\"\nversion = \"0.1.0\"\nentry = \"src/extension.ts\"\nminimum_api_version = \"1.0.0\"\n",
    )
    .unwrap();

    rackabel_cmd(home.path(), &proj)
        .arg("deploy")
        .arg("--dry-run")
        .arg("--live")
        .arg(live.app_path())
        .arg("--user-library")
        .arg(ul.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("planned deploy"))
        // slug = the project directory basename, not the manifest name.
        .stdout(predicate::str::contains("slug: clip-renamer"))
        .stdout(predicate::str::contains("Extensions/clip-renamer"));
}
