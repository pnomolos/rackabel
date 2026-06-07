//! Hermetic `dev logs` integration tests (LOGS agent, DESIGN §3.4).
//!
//! Two paths are exercised: (1) the live daemon path — `dev start` with the FakeHost
//! seam writes per-extension session sinks, then `dev logs <name>` reads/streams them;
//! (2) the dead-daemon file-tail fallback — pre-seeded session files are read with NO
//! daemon up (read-only must work with a dead daemon). `--since`/`--level`/`--json`
//! filters and session rotation are asserted against the persisted files (the file
//! record format is the public contract the fallback reconstructs `LogLine`s from).

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use crate::common::*;

/// A `rackabel dev …` command wired for the hermetic daemon path (mirrors dev_daemon).
fn dev_cmd(home: &Path, cwd: &Path, live: &Path) -> Command {
    let mut cmd = rackabel_cmd(home, cwd);
    cmd.env("RACKABEL_HOST_CMD", fakehost_bin())
        .env("RACKABEL_DOCTOR_LIVE_RUNNING", "1")
        .env("RACKABEL_DEV_MODE", "1")
        .env("ABLETON_APP", live)
        .env("ABLETON_USER_LIBRARY", home.join("UserLibrary"));
    cmd
}

fn read_daemon_pid(home: &Path) -> Option<i32> {
    let dir = home.join(".rackabel/daemon");
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        if e.path().extension().and_then(|s| s.to_str()) == Some("pid") {
            let text = std::fs::read_to_string(e.path()).ok()?;
            let v: toml::Value = toml::from_str(&text).ok()?;
            return v.get("pid").and_then(|p| p.as_integer()).map(|i| i as i32);
        }
    }
    None
}

fn pid_alive(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn wait_for_daemon(home: &Path) -> Option<i32> {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if let Some(pid) = read_daemon_pid(home)
            && pid_alive(pid)
        {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

fn force_stop(home: &Path, cwd: &Path, live: &Path) {
    let _ = dev_cmd(home, cwd, live).args(["dev", "stop"]).output();
    if let Some(pid) = read_daemon_pid(home) {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    }
}

/// Write one persisted session-log record (`ts\tlevel\tkind\text\ttext`) for the
/// fallback to read. `ext = None` writes to the shared `_session` stream.
fn seed_record(
    home: &Path,
    session: &str,
    ext: Option<&str>,
    ts_ms: u64,
    level: &str,
    kind: &str,
    text: &str,
) {
    let root = home.join(".rackabel/logs");
    let dir = match ext {
        Some(n) => root.join(n),
        None => root.join("_session"),
    };
    std::fs::create_dir_all(&dir).unwrap();
    let ext_field = ext.unwrap_or("-");
    let record = format!("{ts_ms}\t{level}\t{kind}\t{ext_field}\t{text}\n");
    let path = dir.join(format!("{session}.log"));
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    f.write_all(record.as_bytes()).unwrap();
}

// --- live daemon path ----------------------------------------------------------

/// `dev start` (FakeHost) → the per-extension session sink is written; `dev logs <name>`
/// reads it with the daemon up.
#[test]
fn logs_reads_per_extension_sink_with_daemon_up() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();

    // The FakeHost emits `[petri]:` liveness lines; the daemon attributes + persists them
    // to ~/.rackabel/logs/petri/<session>.log.
    dev_cmd(home.path(), work.path(), live.app_path())
        .env("RK_FAKEHOST_EXT", "petri")
        .args(["dev", "start"])
        .assert()
        .success();
    wait_for_daemon(home.path()).expect("daemon up");

    // Give the host a moment to emit its banner + liveness into the sink.
    let petri_dir = home.path().join(".rackabel/logs/petri");
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if petri_dir.exists()
            && std::fs::read_dir(&petri_dir)
                .map(|mut d| d.next().is_some())
                .unwrap_or(false)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // `dev logs petri` (non-follow) reads the persisted session file for that extension.
    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "logs", "petri"])
        .assert()
        .success()
        .stdout(predicate::str::contains("petri"));

    force_stop(home.path(), work.path(), live.app_path());
}

// --- dead-daemon file fallback -------------------------------------------------

/// `dev logs` with NO daemon reads the persisted session files (read-only fallback).
#[test]
fn logs_dead_daemon_file_fallback() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    seed_record(
        home.path(),
        "1000",
        Some("petri"),
        1000,
        "info",
        "console",
        "cells dividing",
    );
    seed_record(
        home.path(),
        "1000",
        Some("petri"),
        1001,
        "error",
        "lifecycle",
        "petri failed to activate",
    );

    // No daemon is up — the command falls back to the saved files and exits 0.
    rackabel_cmd(home.path(), work.path())
        .args(["dev", "logs", "petri", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cells dividing"))
        .stdout(predicate::str::contains("failed to activate"));
}

/// `--level error` keeps only error-and-above lines (dead-daemon path).
#[test]
fn logs_level_filter() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    seed_record(
        home.path(),
        "1000",
        Some("petri"),
        1000,
        "info",
        "console",
        "an info line",
    );
    seed_record(
        home.path(),
        "1000",
        Some("petri"),
        1001,
        "error",
        "lifecycle",
        "an error line",
    );

    rackabel_cmd(home.path(), work.path())
        .args(["dev", "logs", "petri", "--level", "error", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("an error line"))
        .stdout(predicate::str::contains("an info line").not());
}

/// `--since` drops lines older than the window. Seeding a very old ts and a fresh one,
/// `--since 1h` keeps only the fresh line.
#[test]
fn logs_since_filter() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    seed_record(
        home.path(),
        "1",
        Some("petri"),
        1000,
        "info",
        "console",
        "ancient line",
    );
    seed_record(
        home.path(),
        "1",
        Some("petri"),
        now_ms,
        "info",
        "console",
        "fresh line",
    );

    rackabel_cmd(home.path(), work.path())
        .args(["dev", "logs", "petri", "--since", "1h", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fresh line"))
        .stdout(predicate::str::contains("ancient line").not());
}

/// `--json` emits one JSON object per line with the stable field set.
#[test]
fn logs_json_line_shape() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    seed_record(
        home.path(),
        "1000",
        Some("petri"),
        1234,
        "error",
        "lifecycle",
        "boom",
    );

    rackabel_cmd(home.path(), work.path())
        .args(["dev", "logs", "petri", "--json", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"ts\":1234"))
        .stdout(predicate::str::contains("\"level\":\"error\""))
        .stdout(predicate::str::contains("\"ext\":\"petri\""))
        .stdout(predicate::str::contains("\"kind\":\"lifecycle\""))
        .stdout(predicate::str::contains("\"text\":\"boom\""));
}

/// Session rotation: lines from multiple `<session>.log` files for the same extension
/// are merged in timestamp order.
#[test]
fn logs_session_rotation_merges_in_order() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    // Two sessions, out-of-order session ids but timestamped lines that must interleave.
    seed_record(
        home.path(),
        "2000",
        Some("petri"),
        2000,
        "info",
        "console",
        "second session line",
    );
    seed_record(
        home.path(),
        "1000",
        Some("petri"),
        1000,
        "info",
        "console",
        "first session line",
    );

    let output = rackabel_cmd(home.path(), work.path())
        .args(["dev", "logs", "petri", "--no-input"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.find("first session line").expect("first present");
    let second = stdout.find("second session line").expect("second present");
    assert!(
        first < second,
        "lines must be ordered by timestamp across sessions"
    );
}

/// Without `--raw`, unattributed host/Node internal lines are suppressed; with `--raw`
/// they're shown.
#[test]
fn logs_raw_toggles_host_internals() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    seed_record(
        home.path(),
        "1000",
        None,
        1000,
        "info",
        "host",
        "FlipMessageStreamSocket internal",
    );

    // Default: the noisy host internal is hidden.
    rackabel_cmd(home.path(), work.path())
        .args(["dev", "logs", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("FlipMessageStreamSocket").not());

    // --raw: it's shown.
    rackabel_cmd(home.path(), work.path())
        .args(["dev", "logs", "--raw", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("FlipMessageStreamSocket internal"));
}

/// An empty/missing log dir for an extension is a clean success (no daemon, nothing
/// saved): exit 0 with a friendly note on stderr.
#[test]
fn logs_no_saved_lines_is_clean() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    rackabel_cmd(home.path(), work.path())
        .args(["dev", "logs", "ghost", "--no-input"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no saved log lines"));
}

/// A bad `--since` value is a usage error (exit 2).
#[test]
fn logs_bad_since_is_usage_error() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    rackabel_cmd(home.path(), work.path())
        .args(["dev", "logs", "--since", "5x", "--no-input"])
        .assert()
        .failure()
        .code(2);
}
