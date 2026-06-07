//! `rackabel doctor` integration tests (assert_cmd-driven).
//!
//! Uses the `FakeLive` / `FakeUserLibrary` fixture builders so the environment is
//! fabricated under temp dirs — these tests NEVER touch the real `/Applications`,
//! `~/Music`, or `~/.rackabel`. Process-state probes (Live-running, bare-host) are
//! pinned via the `RACKABEL_DOCTOR_*` doctor probe seams so the output is deterministic
//! regardless of what's actually running on the test machine.

use crate::common::*;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

/// A `rackabel doctor` command wired with a fake Live + user library and pinned
/// process-state probes (Dev Mode off, Live not running, no bare host) — the §6.2
/// happy/Intel screen baseline.
fn doctor_cmd(home: &Path, cwd: &Path, live: &FakeLive, ul: &FakeUserLibrary) -> Command {
    let mut cmd = rackabel_cmd(home, cwd);
    cmd.arg("doctor")
        .arg("--no-input")
        .env("ABLETON_APP", live.app_path())
        .env("ABLETON_USER_LIBRARY", ul.path())
        .env("RACKABEL_DOCTOR_LIVE_RUNNING", "0")
        .env("RACKABEL_DOCTOR_BARE_HOST", "0");
    cmd
}

/// Happy path (Apple Silicon): the Live install line reads as success with the arch,
/// Developer-Mode-off is a non-failing warning, and the run exits 0 (warnings never
/// fail). Matches the §6.2 happy doctor transcript.
#[test]
fn happy_apple_silicon_exits_zero() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    doctor_cmd(home.path(), work.path(), &live, &ul)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ableton Live 12.4.5b3 Suite (beta) — Extensions supported (Apple Silicon)",
        ))
        .stdout(predicate::str::contains(
            "Developer Mode is OFF — the dev loop (live reload) can't run without it",
        ))
        .stdout(predicate::str::contains(
            "open Live → Settings → Extensions → turn on Developer Mode",
        ))
        .stdout(predicate::str::contains("checks passed"));
}

/// On an Intel Mac the arch line still reads as success — Rosetta is not an error
/// (§6.2). The fake Live carries a thin x86_64 Mach-O header.
#[test]
fn intel_rosetta_reads_as_success() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Intel, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    doctor_cmd(home.path(), work.path(), &live, &ul)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Ableton Live 12.4.5b3 Suite (beta) — Extensions supported (Intel, via Rosetta)",
        ));
}

/// The legacy `App-Resources` host layout is probed and reported (never hardcoded).
/// `--verbose` surfaces the resolved host module path with the layout name.
#[test]
fn legacy_host_layout_is_reported_in_verbose() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::AppResources);
    let ul = FakeUserLibrary::new();

    doctor_cmd(home.path(), work.path(), &live, &ul)
        .arg("--verbose")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Live's Extension components found",
        ))
        .stdout(predicate::str::contains(
            "Contents/App-Resources/Extensions/ExtensionHost",
        ));
}

/// Below the runtime floor: a Live whose bundled node reports an old version fails the
/// compatibility check with an "upgrade Live" remedy (never "install Node"). Exit 3.
#[cfg(unix)]
#[test]
fn below_node_floor_says_upgrade_live() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    // Overwrite the bundled-node stub to report an ancient version (< 22.11.0).
    let node = live.app_path().join("Contents/Helpers/ExtensionHost/node");
    std::fs::write(&node, "#!/bin/sh\necho v18.0.0\n").unwrap();
    let mut p = std::fs::metadata(&node).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(&node, p).unwrap();

    doctor_cmd(home.path(), work.path(), &live, &ul)
        .assert()
        .failure()
        .code(3)
        .stdout(predicate::str::contains("older than this SDK needs"))
        .stdout(predicate::str::contains("upgrade Ableton Live"))
        // The remedy is "upgrade Live", never "install Node" — the only mention of
        // Node is the explicit "Do not install Node separately" guard (§0).
        .stdout(predicate::str::contains("Do not install Node separately"));
}

/// Outside a project, the Extensions toolkit row is a non-blocking note ([!]), never a
/// red ✗ — the "check first, then create" order (DESIGN §2). Still exits 0.
#[test]
fn outside_project_toolkit_is_a_note_not_a_failure() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap(); // no rackabel.toml here
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    doctor_cmd(home.path(), work.path(), &live, &ul)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Extensions toolkit — not needed until you run `rackabel new`",
        ));
}

/// `--json` emits the machine shape: every row carries id/symbol/message and a summary
/// with a top-level `ok`. Deterministic with pinned probes.
#[test]
fn json_emits_rows_and_summary() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    let out = doctor_cmd(home.path(), work.path(), &live, &ul)
        .arg("--json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["checks"].is_array());
    assert_eq!(v["ok"], true);
    // The Live install row is present with id + symbol.
    let rows = v["checks"].as_array().unwrap();
    let install = rows.iter().find(|r| r["id"] == "live.install").unwrap();
    assert_eq!(install["symbol"], "ok");
    assert!(v["summary"]["total"].as_u64().unwrap() >= 1);
}

/// Deployed-vs-source drift: a built bundle with no deployed copy warns "built but not
/// deployed"; once a newer source bundle exists over an older deployed copy it warns
/// "newer than the copy deployed in Live" (the deploy-before-reload trap). Exit 0
/// (warning).
#[test]
fn drift_warns_when_source_newer_than_deployed() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    // A minimal extension project named so its slug is "clip-renamer".
    let proj = work.path().join("clip-renamer");
    std::fs::create_dir_all(proj.join("dist")).unwrap();
    std::fs::write(
        proj.join("rackabel.toml"),
        "[extension]\nname = \"Clip Renamer\"\n",
    )
    .unwrap();

    // Deployed copy first (older), then a newer source bundle.
    let deployed = ul.path().join("Extensions/clip-renamer/dist/extension.js");
    std::fs::create_dir_all(deployed.parent().unwrap()).unwrap();
    std::fs::write(&deployed, b"old-deployed-bundle").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(proj.join("dist/extension.js"), b"new-source-bundle").unwrap();

    doctor_cmd(home.path(), &proj, &live, &ul)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "your built bundle is newer than the copy deployed in Live",
        ))
        .stdout(predicate::str::contains("rackabel deploy"));
}

/// A bare (non-rackabel) host running flips the Developer-Mode row to the SIGHUP-unsafe
/// warning (DESIGN §6.3). Pinned via the bare-host probe seam.
#[test]
fn bare_host_running_warns_reload_unsafe() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    let mut cmd = rackabel_cmd(home.path(), work.path());
    cmd.arg("doctor")
        .arg("--no-input")
        .env("ABLETON_APP", live.app_path())
        .env("ABLETON_USER_LIBRARY", ul.path())
        .env("RACKABEL_DOCTOR_LIVE_RUNNING", "1")
        .env("RACKABEL_DOCTOR_BARE_HOST", "1");

    cmd.assert().success().stdout(predicate::str::contains(
        "a non-rackabel Extension Host appears to be running — reload is unsafe",
    ));
}
