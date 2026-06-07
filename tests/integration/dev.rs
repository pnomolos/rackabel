//! Dev-host FOUNDATION integration tests: the `dev` command surface's frozen exit-code
//! / routing contract, and the FakeHost fixture's behavior (SPEC D §6). The
//! daemon-lifecycle / watcher / crash-recovery tests that DRIVE the daemon are owned by
//! the daemon-core + watch-loop agents; the foundation proves (a) the surface routes
//! and exits as specified and (b) the FakeHost fixture honors its contract, since every
//! later hermetic daemon test depends on it.

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::time::{Duration, Instant};

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use crate::common::*;

// --- surface contract (no Live, no daemon) -------------------------------------

/// `dev register --name` + `--recursive` is rejected at PARSE time (exit 2) — one name
/// cannot label N members (§3.2).
#[test]
fn register_name_with_recursive_is_parse_error() {
    let home = TempDir::new().unwrap();
    rackabel_cmd(home.path(), home.path())
        .args(["dev", "register", "--name", "foo", "--recursive"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cannot be used with"));
}

/// A `dev` verb needing a daemon, with none up, is an ENVIRONMENT error (exit 3).
#[test]
fn dev_status_without_daemon_is_environment_error() {
    let home = TempDir::new().unwrap();
    rackabel_cmd(home.path(), home.path())
        .args(["dev", "status", "--no-input"])
        .assert()
        .failure()
        .code(3);
}

/// A `dev` verb token wins over a same-named extension: `dev test` is the subcommand
/// (the no-Live test runner), NOT the bare loop. With nothing registered and no project
/// in the (temp) cwd it has nothing to test → `RK0001` (exit 3) — crucially NOT the
/// bare loop's `RK0307`, which proves the verb routed to the test runner.
#[test]
fn dev_verb_wins_over_name() {
    let home = TempDir::new().unwrap();
    rackabel_cmd(home.path(), home.path())
        .args(["dev", "test", "--no-input"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0001"))
        .stderr(predicate::str::contains("RK0307").not());
}

/// `--only <token>` ALWAYS routes through the name matcher (the bare loop), never the
/// verb table — so `dev --only test` is the bare loop (exit 3 with no daemon), proving
/// the §3.3 scoping rule.
#[test]
fn only_routes_through_name_matcher_not_verbs() {
    let home = TempDir::new().unwrap();
    rackabel_cmd(home.path(), home.path())
        .args(["dev", "--only", "test", "--no-input"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0307"));
}

/// `dev reload` with no daemon is exit 3 (RK0309) — the scriptable CI trigger fails
/// deterministically rather than hanging (§2 dev table, §7).
#[test]
fn dev_reload_without_daemon_is_no_daemon() {
    let home = TempDir::new().unwrap();
    rackabel_cmd(home.path(), home.path())
        .args(["dev", "reload", "--no-input"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0309"));
}

// --- FakeHost fixture contract (SPEC D §6) -------------------------------------

/// The FakeHost prints the synthetic startup banner + connected marker + a per-ext
/// liveness line, then sleeps; SIGTERM (here via kill on drop) terminates it. We read
/// its stdout to confirm the markers the daemon's connect-wait keys on.
#[test]
fn fakehost_prints_banner_and_liveness() {
    let mut child = fakehost_cmd(&["alpha", "beta"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn fakehost");

    let stdout = child.stdout.take().unwrap();
    let reader = BufReader::new(stdout);

    let mut saw_started = false;
    let mut saw_connected = false;
    let mut saw_alpha = false;
    let mut saw_beta = false;

    let deadline = Instant::now() + Duration::from_secs(5);
    for line in reader.lines() {
        let line = line.unwrap();
        if line.contains("Started: Extension Host 1.0.0") {
            saw_started = true;
        }
        if line.contains("FlipMessageStreamSocket send success") {
            saw_connected = true;
        }
        if line.contains("[alpha]:") {
            saw_alpha = true;
        }
        if line.contains("[beta]:") {
            saw_beta = true;
        }
        if saw_started && saw_connected && saw_alpha && saw_beta {
            break;
        }
        if Instant::now() > deadline {
            break;
        }
    }

    // Reap the still-sleeping child.
    let _ = child.kill();
    let _ = child.wait();

    assert!(saw_started, "missing Started banner");
    assert!(saw_connected, "missing connected marker");
    assert!(saw_alpha, "missing [alpha] liveness");
    assert!(saw_beta, "missing [beta] liveness");
}

/// `RK_FAKEHOST_CRASH=<ms>` makes the FakeHost exit non-zero after the delay — the
/// crash-loop / crash-recovery fixture the daemon-core tests rely on.
#[test]
fn fakehost_crash_mode_exits_nonzero() {
    let status = fakehost_cmd(&["x"])
        .env("RK_FAKEHOST_CRASH", "50")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run fakehost");
    assert!(!status.success(), "crash mode should exit non-zero");
    assert_eq!(status.code(), Some(1));
}

/// `RK_FAKEHOST_HANG=1` prints the banner but never the connected marker — the
/// single-connection "stuck at Started" case the connect-timeout path keys on.
#[test]
fn fakehost_hang_mode_omits_connected_marker() {
    let mut child = fakehost_cmd(&["x"])
        .env("RK_FAKEHOST_HANG", "1")
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn fakehost");

    let stdout = child.stdout.take().unwrap();

    // Hang mode prints exactly the 3-line banner then sleeps forever, so a blocking
    // line reader would hang. Read on a background thread for a bounded window and
    // collect whatever banner lines arrived; the connected marker must be absent.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut saw_started = false;
    let mut saw_connected = false;
    let deadline = Instant::now() + Duration::from_millis(800);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                if line.contains("Started: Extension Host 1.0.0") {
                    saw_started = true;
                }
                if line.contains("FlipMessageStreamSocket send success") {
                    saw_connected = true;
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if saw_started {
                    // Banner arrived and the channel has gone quiet (host is sleeping).
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    assert!(saw_started, "hang mode still prints the Started banner");
    assert!(
        !saw_connected,
        "hang mode must NOT print the connected marker"
    );
}
