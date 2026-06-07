//! `rackabel dev reload` exit-code contract against a FAKE daemon (REGISTRY agent).
//!
//! The reload verb is a thin IPC client; its job is to map the daemon's `ReloadResult`
//! to the §7 exit contract. We stand up a minimal Unix-socket "daemon" under the test's
//! `RACKABEL_HOME/daemon/<x>.sock`, have it return a scripted `reload_result` line, and
//! assert the real binary's exit code + surfaced output. No Live, no real host, fully
//! hermetic — `connect_daemon` locates our socket by scanning the daemon dir.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::thread;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use crate::common::*;

/// Bind a fake daemon socket under `home/.rackabel/daemon/test.sock` and answer the
/// first `reload` request with `response_line`. Runs on a background thread; returns
/// the listener-holding handle the test keeps alive for the command's lifetime.
fn fake_daemon(home: &Path, response_line: String) -> thread::JoinHandle<()> {
    let dir = home.join(".rackabel").join("daemon");
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("test.sock");
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).unwrap();
    thread::spawn(move || {
        // Serve a couple of connections so a stray ping/probe doesn't starve the real
        // reload request.
        for _ in 0..4 {
            let Ok((stream, _)) = listener.accept() else {
                break;
            };
            let resp = response_line.clone();
            thread::spawn(move || {
                let mut writer = stream.try_clone().unwrap();
                let reader = BufReader::new(stream);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    // Reply to whatever request with the scripted reload_result.
                    let _ = writer.write_all(resp.as_bytes());
                    let _ = writer.write_all(b"\n");
                    let _ = writer.flush();
                    break;
                }
            });
        }
    })
}

const RUNNING_HOST: &str =
    r#""host_state":{"state":"running","pid":1,"since_ms":0,"api_version":"1.0.0"}"#;

/// All targeted extensions loaded → exit 0.
#[test]
fn reload_all_loaded_is_success() {
    let home = TempDir::new().unwrap();
    let resp = format!(
        r#"{{"v":1,"type":"reload_result","ok":true,"reloaded":["alpha","beta"],"failed":[],"skipped":[],"reload_ms":42,{RUNNING_HOST}}}"#
    );
    let _daemon = fake_daemon(home.path(), resp);

    rackabel_cmd(home.path(), home.path())
        .args(["dev", "reload", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("reloaded alpha, beta"));
}

/// Any targeted extension threw in activate() → exit 1, RK1306.
#[test]
fn reload_activate_failure_is_exit_1() {
    let home = TempDir::new().unwrap();
    let resp = format!(
        r#"{{"v":1,"type":"reload_result","ok":false,"reloaded":["alpha"],"failed":[{{"name":"beta","error":"boom"}}],"skipped":[],"reload_ms":10,{RUNNING_HOST}}}"#
    );
    let _daemon = fake_daemon(home.path(), resp);

    rackabel_cmd(home.path(), home.path())
        .args(["dev", "reload", "--no-input"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("RK1306"))
        .stderr(predicate::str::contains("beta"));
}

/// A pre-filtered (host-incompatible) skip is reported but NOT fatal without --strict.
#[test]
fn reload_skip_without_strict_is_success() {
    let home = TempDir::new().unwrap();
    let resp = format!(
        r#"{{"v":1,"type":"reload_result","ok":true,"reloaded":["alpha"],"failed":[],"skipped":[{{"name":"beta","reason":"minimumApiVersion=2.0.0 > host 1.0.0"}}],"reload_ms":12,{RUNNING_HOST}}}"#
    );
    let _daemon = fake_daemon(home.path(), resp);

    rackabel_cmd(home.path(), home.path())
        .args(["dev", "reload", "--no-input"])
        .assert()
        .success()
        .stderr(predicate::str::contains("Skipped: beta"));
}

/// `--strict` promotes a skip to a fatal exit 1, RK4006.
#[test]
fn reload_skip_with_strict_is_exit_1() {
    let home = TempDir::new().unwrap();
    let resp = format!(
        r#"{{"v":1,"type":"reload_result","ok":true,"reloaded":["alpha"],"failed":[],"skipped":[{{"name":"beta","reason":"minimumApiVersion=2.0.0 > host 1.0.0"}}],"reload_ms":12,{RUNNING_HOST}}}"#
    );
    let _daemon = fake_daemon(home.path(), resp);

    rackabel_cmd(home.path(), home.path())
        .args(["dev", "reload", "--strict", "--no-input"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("RK4006"));
}

/// `--json` emits the stable reload shape and the exit code still follows the contract.
#[test]
fn reload_json_shape() {
    let home = TempDir::new().unwrap();
    let resp = format!(
        r#"{{"v":1,"type":"reload_result","ok":true,"reloaded":["alpha"],"failed":[],"skipped":[{{"name":"beta","reason":"too new"}}],"reload_ms":7,{RUNNING_HOST}}}"#
    );
    let _daemon = fake_daemon(home.path(), resp);

    rackabel_cmd(home.path(), home.path())
        .args(["dev", "reload", "--json", "--no-input"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"reload_ms\": 7"))
        .stdout(predicate::str::contains("\"reloaded\""))
        .stdout(predicate::str::contains("\"skipped\""));
}

/// No daemon at all → exit 3, RK0309 (the deterministic CI failure, never a hang).
#[test]
fn reload_no_daemon_is_exit_3() {
    let home = TempDir::new().unwrap();
    rackabel_cmd(home.path(), home.path())
        .args(["dev", "reload", "--no-input"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0309"));
}
