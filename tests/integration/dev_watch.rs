//! Watch-loop integration tests (WATCH-LOOP, SPEC D §6).
//!
//! These drive the REAL `rackabel` binary with the daemon pointed at the FakeHost seam
//! (no real Live/node/User-Library). The hermetic chain-ordering "trap" test (reload
//! NEVER precedes deploy) lives as a unit test in `src/dev/watch.rs` where it can call
//! `build_deploy_reload` against a fake daemon without esbuild; here we cover the
//! end-to-end surface the loop presents: the non-TTY legend, the per-extension liveness
//! lines, and the >4-enabled scope hint (§3.3).

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
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

/// Read the daemon PID (single .pid under the daemon dir), for teardown.
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

/// Seed registry.toml with N enabled extensions whose project roots exist (so the
/// working-set resolution + plan derivation succeed). The FakeHost advertises the same
/// names so `dev status` reports them Loaded.
fn seed_registry(home: &Path, work: &Path, names: &[&str]) {
    let dir = home.join(".rackabel");
    std::fs::create_dir_all(&dir).unwrap();
    let mut body = String::new();
    for name in names {
        let proj = work.join(name);
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(
            proj.join("rackabel.toml"),
            format!("[extension]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
        )
        .unwrap();
        body.push_str(&format!(
            "[[extension]]\nname = \"{name}\"\npath = {:?}\nsource = \"dist\"\nenabled = true\n\n",
            proj.display()
        ));
    }
    std::fs::write(dir.join("registry.toml"), body).unwrap();
}

/// Wait until the daemon pidfile appears (the daemon bound its socket).
fn wait_for_daemon(home: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if read_daemon_pid(home).is_some() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Spawn `dev watch` (which needs a running daemon, never starts one), read its stdout
/// for a bounded window collecting lines, then kill it (the loop blocks forever).
fn run_watch_collect(home: &Path, work: &Path, live: &Path, extra: &[&str]) -> Vec<String> {
    let mut args = vec!["dev", "watch"];
    args.extend_from_slice(extra);
    let mut child = dev_cmd(home, work, live)
        .args(&args)
        // Non-TTY: piped stdio means the loop runs hotkey-free (§7 --no-input path).
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn dev watch");

    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut lines = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(line) => lines.push(line),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                // Once we've seen the legend the loop is idle (watching) — stop early.
                if lines.iter().any(|l| l.contains("watching for changes")) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    lines
}

/// `dev watch` with no daemon up is the clean RK0309 (the explicit form NEVER starts a
/// host — it tells you to run `rackabel dev`), §7.
#[test]
fn dev_watch_without_daemon_is_no_daemon() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();
    seed_registry(home.path(), work.path(), &["a"]);

    dev_cmd(home.path(), work.path(), live.app_path())
        .args(["dev", "watch"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicates::str::contains("RK0309"));
}

/// The non-TTY watch loop prints the connected banner, the per-extension liveness line,
/// and the hotkey-free "watching for changes" legend (no `[r]/[l]/[q]` keys without a
/// TTY, §7).
#[test]
fn watch_non_tty_prints_liveness_and_no_hotkeys() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();
    seed_registry(home.path(), work.path(), &["clip-renamer"]);

    // Start the daemon with the FakeHost advertising the extension.
    dev_cmd(home.path(), work.path(), live.app_path())
        .env("RK_FAKEHOST_EXT", "clip-renamer")
        .args(["dev", "start"])
        .assert()
        .success();
    assert!(wait_for_daemon(home.path()), "daemon up");

    let lines = run_watch_collect(home.path(), work.path(), live.app_path(), &[]);
    let joined = lines.join("\n");

    assert!(
        joined.contains("connected to Live"),
        "expected connected banner, got:\n{joined}"
    );
    assert!(
        joined.contains("watching for changes"),
        "expected the non-TTY legend, got:\n{joined}"
    );
    // No TTY hotkey legend in non-interactive mode.
    assert!(
        !joined.contains("[q] quit"),
        "non-TTY must NOT print the hotkey legend, got:\n{joined}"
    );
}

/// With more than 4 enabled extensions, bare `dev` prints the one-time scope hint (§3.3).
/// We drive `dev watch` (same loop) over a 5-extension registry and assert the hint.
#[test]
fn scope_hint_fires_above_four_enabled() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();
    let names = ["a", "b", "c", "d", "e"];
    seed_registry(home.path(), work.path(), &names);

    dev_cmd(home.path(), work.path(), live.app_path())
        .env("RK_FAKEHOST_EXT", names.join(","))
        .args(["dev", "start"])
        .assert()
        .success();
    assert!(wait_for_daemon(home.path()), "daemon up");

    let lines = run_watch_collect(home.path(), work.path(), live.app_path(), &[]);
    let joined = lines.join("\n");
    assert!(
        joined.contains("extensions loaded — reloads re-run all of them"),
        "expected the >4 scope hint, got:\n{joined}"
    );
}

/// `--only <glob>` scopes the working set to matching names: the loop loads just those,
/// so the scope hint does NOT fire even though the registry has >4 enabled.
#[test]
fn only_glob_scopes_and_suppresses_hint() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::AppleSilicon, FakeLayout::Helpers);
    let _guard = DaemonGuard::new(home.path(), work.path(), live.app_path());
    std::fs::create_dir_all(home.path().join("UserLibrary/Extensions")).unwrap();
    let names = ["harmonic-lens", "harmonic-x", "groove", "petri", "lidal"];
    seed_registry(home.path(), work.path(), &names);

    dev_cmd(home.path(), work.path(), live.app_path())
        .env("RK_FAKEHOST_EXT", names.join(","))
        .args(["dev", "start"])
        .assert()
        .success();
    assert!(wait_for_daemon(home.path()), "daemon up");

    // Scope to 2 of the 5 — the hint must NOT fire.
    let lines = run_watch_collect(
        home.path(),
        work.path(),
        live.app_path(),
        &["--only", "harmonic-*"],
    );
    let joined = lines.join("\n");
    assert!(
        !joined.contains("extensions loaded — reloads re-run all of them"),
        "a scoped working set must suppress the >4 hint, got:\n{joined}"
    );
    assert!(
        joined.contains("watching for changes"),
        "still expected the legend, got:\n{joined}"
    );
}
