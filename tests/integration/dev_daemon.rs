//! Hermetic daemon-lifecycle integration tests (DAEMON-CORE, SPEC D §6).
//!
//! These drive the REAL `rackabel` binary but point the daemon at the `rk-fakehost`
//! fixture via the `RACKABEL_HOST_CMD` seam, and pin the behavioral environment probes
//! (`RACKABEL_DOCTOR_LIVE_RUNNING=1`, `RACKABEL_DEV_MODE=1`) so nothing real is touched:
//! no real Live, no real User Library, no real `~/.rackabel`, no real host process. A
//! `FakeLive` `.app` satisfies the host-path resolution (its bundled-node + host-module
//! stubs); the actual host child is the FakeHost.
//!
//! Coverage: start → pidfile+socket+ping → status Running → stop killpg's the group
//! (the FakeHost child PID is gone) → stale-pidfile reclaim → double-start idempotent →
//! crash-respawn → crash-loop → socket protocol round-trip.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use crate::common::*;

/// A `rackabel dev …` command wired for the hermetic daemon path: temp HOME +
/// RACKABEL_HOME, the FakeHost seam, the pinned env probes, and a FakeLive `.app`.
fn dev_cmd(home: &Path, cwd: &Path, live: &Path) -> Command {
    let mut cmd = rackabel_cmd(home, cwd);
    cmd.env("RACKABEL_HOST_CMD", fakehost_bin())
        .env("RACKABEL_DOCTOR_LIVE_RUNNING", "1")
        .env("RACKABEL_DEV_MODE", "1")
        .env("ABLETON_APP", live)
        // A fake User Library so resolve_newest doesn't reach a real one.
        .env("ABLETON_USER_LIBRARY", home.join("UserLibrary"));
    cmd
}

/// Read the per-Live pidfile PID by scanning the daemon dir for the single .pid.
fn read_daemon_pid(home: &Path) -> Option<i32> {
    let dir = home.join(".rackabel/daemon");
    let entries = std::fs::read_dir(&dir).ok()?;
    for e in entries.flatten() {
        if e.path().extension().and_then(|s| s.to_str()) == Some("pid") {
            let text = std::fs::read_to_string(e.path()).ok()?;
            let v: toml::Value = toml::from_str(&text).ok()?;
            return v.get("pid").and_then(|p| p.as_integer()).map(|i| i as i32);
        }
    }
    None
}

fn pid_alive(pid: i32) -> bool {
    // kill -0 via /bin/kill (no nix dep in the test crate).
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Wait until the daemon dir has a live pidfile, or a deadline.
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

/// Find the single control socket the daemon bound (the `<hash>.sock` under the daemon
/// dir), so a raw socket round-trip can exercise verbs whose CLI is owned by another
/// agent (e.g. `reload`).
fn find_socket(home: &Path) -> Option<PathBuf> {
    let dir = home.join(".rackabel/daemon");
    std::fs::read_dir(&dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        (p.extension().and_then(|s| s.to_str()) == Some("sock")).then_some(p)
    })
}

/// One JSON-Lines request → one response line over the control socket.
fn sock_call(sock: &Path, request_json: &str) -> String {
    let mut stream = UnixStream::connect(sock).expect("connect socket");
    stream.write_all(request_json.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    line
}

#[test]
fn start_status_stop_lifecycle() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();

    // dev start → daemon comes up.
    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "start"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dev host running"));

    let pid = wait_for_daemon(home.path()).expect("daemon pidfile appears");
    assert!(pid_alive(pid), "daemon should be alive");

    // dev status → Running.
    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dev host running"))
        .stdout(predicate::str::contains("Live:"));

    // dev status --json → machine-readable snapshot with the running host.
    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "status", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"state\": \"running\""));

    // dev stop → daemon (and its host group) gone.
    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "stop"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dev host stopped"));

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && pid_alive(pid) {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !pid_alive(pid),
        "daemon (and host group) must be gone after stop"
    );
}

#[test]
fn double_start_is_idempotent() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();

    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "start"])
        .assert()
        .success();
    let pid1 = wait_for_daemon(home.path()).expect("first daemon");

    // A second start reuses the live daemon (same pid).
    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "start"])
        .assert()
        .success();
    let pid2 = read_daemon_pid(home.path()).expect("second read");
    assert_eq!(pid1, pid2, "double-start must reuse the running daemon");
}

#[test]
fn stop_without_daemon_is_no_daemon() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();

    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "stop"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0309"));
}

#[test]
fn stale_pidfile_is_reclaimed() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();

    // Start once to learn the per-Live hash filename, then stop, then forge a stale
    // pidfile pointing at a dead PID.
    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "start"])
        .assert()
        .success();
    wait_for_daemon(home.path());
    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "stop"])
        .assert()
        .success();

    // Forge a stale pidfile (PID 2 is launchd/never-ours-to-signal; use an unlikely PID).
    let daemon_dir = home.path().join(".rackabel/daemon");
    std::fs::create_dir_all(&daemon_dir).unwrap();
    // Find any prior .pid name to reuse the hash, else fabricate one.
    let name = std::fs::read_dir(&daemon_dir)
        .unwrap()
        .flatten()
        .find_map(|e| {
            let p = e.path();
            (p.extension().and_then(|s| s.to_str()) == Some("pid"))
                .then(|| p.file_name().unwrap().to_string_lossy().into_owned())
        });
    if let Some(name) = name {
        let dead_pid = 999_999; // almost certainly not a live process
        let body = format!(
            "pid = {dead_pid}\npgid = {dead_pid}\nversion = 1\nsock = \"x\"\n\
             live_app = \"{}\"\nhost_module = \"m\"\neh_node = \"n\"\nstarted_at = 0\n",
            live.app_path().display()
        );
        std::fs::write(daemon_dir.join(&name), body).unwrap();
    }

    // Start again: the stale pidfile must be reclaimed and a fresh daemon come up.
    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "start"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dev host running"));
    let pid = wait_for_daemon(home.path()).expect("fresh daemon after reclaim");
    assert!(pid_alive(pid));
}

#[test]
fn no_input_dev_mode_off_is_environment_exit_3() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());

    // Live up but Dev Mode off + --no-input ⇒ deterministic exit 3 RK0306 (no hang).
    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_HOST_CMD", fakehost_bin())
        .env("RACKABEL_DOCTOR_LIVE_RUNNING", "1")
        .env("RACKABEL_DEV_MODE", "0")
        .env("ABLETON_APP", live.app_path())
        .args(["dev", "start", "--no-input"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0306"));
}

#[test]
fn no_input_live_down_is_environment_exit_3() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());

    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_HOST_CMD", fakehost_bin())
        .env("RACKABEL_DOCTOR_LIVE_RUNNING", "0")
        .env("ABLETON_APP", live.app_path())
        .args(["dev", "start", "--no-input"])
        .assert()
        .failure()
        .code(3);
}

#[test]
fn socket_round_trip_ping_reload_and_bad_version() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();

    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "start"])
        .assert()
        .success();
    wait_for_daemon(home.path()).expect("daemon up");
    let sock = find_socket(home.path()).expect("socket exists");

    // ping → pong with the protocol version.
    let pong = sock_call(&sock, r#"{"v":1,"type":"ping"}"#);
    assert!(pong.contains("\"type\":\"pong\""), "got: {pong}");
    assert!(pong.contains("\"protocol_v\":1"));

    // reload (no extensions registered) → a reload_result, host still running.
    let reload = sock_call(&sock, r#"{"v":1,"type":"reload","strict":false}"#);
    assert!(
        reload.contains("\"type\":\"reload_result\""),
        "got: {reload}"
    );
    assert!(reload.contains("\"ok\":true"));

    // a bad protocol version is rejected with RK0308 and the connection closed.
    let bad = sock_call(&sock, r#"{"v":999,"type":"ping"}"#);
    assert!(bad.contains("RK0308"), "got: {bad}");
}

/// REGRESSION: several requests on ONE persistent connection (what the bare-`dev`/watch
/// UI does — it keeps a single `Client` open for SetWorkingSet, Status, and every Reload).
/// macOS `accept(2)` inherits the listener's non-blocking flag onto the accepted socket;
/// if the daemon doesn't clear it, the per-connection blocking read returns `WouldBlock`
/// after the first request and the connection is torn down — so the watch loop's *second*
/// call (the reload after an edit) fails with `RK0309 lost the connection`. The earlier
/// round-trip test masked this by opening a fresh connection per request. This test issues
/// three requests on the SAME stream and asserts all three get a reply.
#[test]
fn multiple_requests_on_one_connection() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();

    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "start"])
        .assert()
        .success();
    wait_for_daemon(home.path()).expect("daemon up");
    let sock = find_socket(home.path()).expect("socket exists");

    let stream = UnixStream::connect(&sock).expect("connect socket");
    let mut writer = stream.try_clone().unwrap();
    let mut reader = BufReader::new(stream);
    let mut call = |req: &str| -> String {
        writer.write_all(req.as_bytes()).unwrap();
        writer.write_all(b"\n").unwrap();
        writer.flush().unwrap();
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read a reply");
        assert!(
            n > 0,
            "daemon closed the connection mid-session (req: {req})"
        );
        line
    };

    let r1 = call(r#"{"v":1,"type":"ping"}"#);
    assert!(r1.contains("\"type\":\"pong\""), "1st: {r1}");
    // The SECOND request on the SAME connection is the one the non-blocking-accept bug
    // dropped.
    let r2 = call(r#"{"v":1,"type":"status"}"#);
    assert!(r2.contains("\"type\":\"status\""), "2nd: {r2}");
    let r3 = call(r#"{"v":1,"type":"ping"}"#);
    assert!(r3.contains("\"type\":\"pong\""), "3rd: {r3}");
}

#[test]
fn crash_looping_is_reported_by_status() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();

    // A connect-then-crash host: the daemon sees Running, then crash-recovers with
    // backoff, and after the bounded window flips to CrashLooping (DESIGN §3.5).
    dev_cmd(home.path(), work.path(), live.app_path())
        .env("RK_FAKEHOST_CRASH", "120")
        .args(["dev", "start"])
        .assert()
        .success();
    wait_for_daemon(home.path()).expect("daemon up");
    let sock = find_socket(home.path()).expect("socket");

    // Poll status until the host reports crash-looping (bounded wait covering the
    // backoff schedule + window).
    let mut saw_crash_looping = false;
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        let status = sock_call(&sock, r#"{"v":1,"type":"status"}"#);
        if status.contains("\"state\":\"crash_looping\"") {
            saw_crash_looping = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    assert!(
        saw_crash_looping,
        "status should eventually report crash_looping"
    );
}

#[test]
fn status_without_daemon_is_no_daemon_exit_3() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();

    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "status"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0309"));
}
