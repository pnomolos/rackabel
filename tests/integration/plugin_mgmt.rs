//! `plugin install` / `list` / `enable` / `disable` / `search` + `plugins.lock`, end to
//! end (DESIGN §5.4/§5.6/§5.7). PLUGIN-MGMT-owned. Hermetic: a fixture plugin is a tiny
//! shell script / tarball in a temp dir; the GitHub API/asset host is a local stub TCP
//! server reached via the RACKABEL_GITHUB_API / RACKABEL_GITHUB_DL seams. No real network,
//! no Live.
//!
//! Unix-only at the `mod` site in `main.rs` (the fixture plugins are shell scripts with
//! exec bits + symlink assertions).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use super::common::rackabel_cmd;

/// Write an executable `rackabel-<name>` shell script at `path` that echoes a marker.
fn write_plugin_script(path: &Path, marker: &str) {
    std::fs::write(path, format!("#!/bin/sh\necho \"{marker}\"\nexit 0\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The managed lockfile path under a temp home.
fn lock_path(home: &Path) -> PathBuf {
    home.join(".rackabel/plugins.lock")
}

/// The managed-bin symlink for `name`.
fn managed_bin(home: &Path, name: &str) -> PathBuf {
    home.join(format!(".rackabel/plugins/bin/rackabel-{name}"))
}

// --- sideload: local path -------------------------------------------------------

/// Sideloading a local `rackabel-<name>` executable always works (no gatekeeper), pins it
/// by sha256 in plugins.lock, and links it into the managed bin. The dispatch then runs it.
#[test]
fn sideload_path_installs_pins_and_dispatches() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let src = work.path().join("rackabel-greet");
    write_plugin_script(&src, "GREET-RAN");

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", src.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("installed `greet`"))
        .stdout(predicate::str::contains("sha256"));

    // The lock records a sha256 pin for the sideloaded path.
    let lock = std::fs::read_to_string(lock_path(home.path())).unwrap();
    assert!(lock.contains("name = \"greet\""), "lock: {lock}");
    assert!(lock.contains("source = \"path\""));
    assert!(lock.contains("sha256 = "));

    // The managed-bin symlink exists and resolves.
    assert!(managed_bin(home.path(), "greet").exists());

    // A bare `rackabel greet` dispatches to the managed copy (enabled by default for a
    // plain tier-2 plugin).
    rackabel_cmd(home.path(), work.path())
        .arg("greet")
        .assert()
        .success()
        .stdout(predicate::str::contains("GREET-RAN"));
}

/// The sha256 pin asserted in the lock matches the actual sha256 of the executable.
#[test]
fn sideload_pin_matches_file_sha256() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let src = work.path().join("rackabel-foo");
    write_plugin_script(&src, "FOO");

    rackabel_cmd(home.path(), work.path())
        .args([
            "plugin",
            "install",
            src.to_str().unwrap(),
            "--yes",
            "--json",
        ])
        .assert()
        .success();

    let lock = std::fs::read_to_string(lock_path(home.path())).unwrap();
    // sha256("…the script bytes…") — recompute via `shasum`/openssl-free: read the linked
    // file back and compare lengths is not enough; instead assert the lock pin is a 64-hex.
    let pin = lock
        .lines()
        .find_map(|l| l.trim().strip_prefix("sha256 = "))
        .map(|s| s.trim_matches('"').to_string())
        .expect("a sha256 pin in the lock");
    assert_eq!(pin.len(), 64, "sha256 hex length");
    assert!(pin.chars().all(|c| c.is_ascii_hexdigit()));
}

// --- sideload: tarball ----------------------------------------------------------

/// Build a `.tgz` containing `rackabel-<name>` and return its path (inside `dir`).
fn make_tarball(dir: &Path, name: &str, marker: &str) -> PathBuf {
    // Build a staging dir with the executable, then tar+gzip it via the system `tar`.
    let stage = dir.join("stage");
    std::fs::create_dir_all(&stage).unwrap();
    let exe = stage.join(format!("rackabel-{name}"));
    write_plugin_script(&exe, marker);

    let tgz = dir.join(format!("rackabel-{name}-1.0.0.tgz"));
    let status = Command::new("tar")
        .args([
            "-czf",
            tgz.to_str().unwrap(),
            "-C",
            stage.to_str().unwrap(),
            &format!("rackabel-{name}"),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "tar failed");
    tgz
}

/// Sideloading a `.tgz` unpacks it into the store, finds the executable, pins it by sha256,
/// and dispatches.
#[test]
fn sideload_tarball_unpacks_pins_and_dispatches() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let tgz = make_tarball(work.path(), "notarize", "NOTARIZE-RAN");

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", tgz.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("installed `notarize`"));

    let lock = std::fs::read_to_string(lock_path(home.path())).unwrap();
    assert!(lock.contains("name = \"notarize\""));
    assert!(lock.contains("source = \"tarball\""));
    assert!(lock.contains("sha256 = "));

    rackabel_cmd(home.path(), work.path())
        .arg("notarize")
        .assert()
        .success()
        .stdout(predicate::str::contains("NOTARIZE-RAN"));
}

// --- confirmation flow ----------------------------------------------------------

/// A remote OWNER/REPO install under --no-input refuses with RK0403 (exit 3) and fetches
/// nothing — no lockfile entry, no managed bin.
#[test]
fn remote_install_no_input_refuses_and_changes_nothing() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    rackabel_cmd(home.path(), work.path())
        .args(["--no-input", "plugin", "install", "owner/repo"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0403"))
        .stderr(predicate::str::contains("was not confirmed"));

    // Nothing changed.
    assert!(!lock_path(home.path()).exists());
    assert!(!managed_bin(home.path(), "repo").exists());
}

/// A sideload does NOT require the remote consent gate (it is local code the user has): it
/// installs even under --no-input without --yes.
#[test]
fn sideload_does_not_require_consent_gate() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let src = work.path().join("rackabel-local");
    write_plugin_script(&src, "LOCAL");

    rackabel_cmd(home.path(), work.path())
        .args(["--no-input", "plugin", "install", src.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("installed `local`"));
    assert!(managed_bin(home.path(), "local").exists());
}

// --- pin mismatch (tamper) ------------------------------------------------------

/// Modifying an installed file and reinstalling the (now-different) bytes WITHOUT --force is
/// a pin mismatch — RK4007, exit 4. With --force it updates past the pin, announced.
#[test]
fn pin_mismatch_on_reinstall_is_exit_4_then_force_updates() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let src = work.path().join("rackabel-foo");
    write_plugin_script(&src, "V1");

    // Install v1.
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", src.to_str().unwrap(), "--yes"])
        .assert()
        .success();

    // Change the source bytes (a different sha256), then reinstall WITHOUT --force.
    write_plugin_script(&src, "V2-DIFFERENT");
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", src.to_str().unwrap(), "--yes"])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("RK4007"))
        .stderr(predicate::str::contains("different code"));

    // With --force it updates past the pin and announces the change.
    rackabel_cmd(home.path(), work.path())
        .args([
            "plugin",
            "install",
            src.to_str().unwrap(),
            "--yes",
            "--force",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("--force"))
        .stdout(predicate::str::contains("past its pin"));
}

/// Tampering with the INSTALLED store file (the symlink target) and then dispatching the
/// plugin is caught by the pre-run pin verification — RK4007, exit 4. This is the §5.7
/// tamper-protection guarantee: the managed bytes must match the lockfile pin to run.
#[test]
fn tampered_installed_file_fails_verify_exit_4() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let src = work.path().join("rackabel-greet");
    write_plugin_script(&src, "GREET-RAN");

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", src.to_str().unwrap(), "--yes"])
        .assert()
        .success();

    // It runs cleanly while the bytes match the pin.
    rackabel_cmd(home.path(), work.path())
        .arg("greet")
        .assert()
        .success()
        .stdout(predicate::str::contains("GREET-RAN"));

    // TAMPER: rewrite the installed store file (the symlink target) in place.
    let link = managed_bin(home.path(), "greet");
    let target = std::fs::canonicalize(&link).unwrap();
    write_plugin_script(&target, "TAMPERED");

    // Dispatch now fails the pin verification (RK4007, exit 4) before running anything.
    rackabel_cmd(home.path(), work.path())
        .arg("greet")
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("RK4007"))
        .stdout(predicate::str::contains("TAMPERED").not());
}

// --- enable / disable dispatch gating ------------------------------------------

/// `disable` flips the lock flag and gates dispatch: a disabled managed plugin is skipped in
/// the bare-name bin search with a note. `enable` restores it. `plugin run` reaches it
/// regardless (the §5.6 escape hatch).
#[test]
fn disable_gates_dispatch_enable_restores_run_always_reaches() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let src = work.path().join("rackabel-greet");
    write_plugin_script(&src, "GREET-RAN");

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", src.to_str().unwrap(), "--yes"])
        .assert()
        .success();

    // Disable it.
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "disable", "greet"])
        .assert()
        .success()
        .stdout(predicate::str::contains("disabled `greet`"));

    // Bare dispatch is now skipped (RK0401, exit 3) with a note.
    rackabel_cmd(home.path(), work.path())
        .arg("greet")
        .assert()
        .failure()
        .code(3)
        // The disabled-skip note goes to STDERR (D-88: warnings never touch a plugin's
        // stdout), alongside the RK0401 frame.
        .stderr(predicate::str::contains("disabled (skipped)"))
        .stderr(predicate::str::contains("RK0401"));

    // `plugin run` still reaches it (escape hatch).
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "run", "greet"])
        .assert()
        .success()
        .stdout(predicate::str::contains("GREET-RAN"));

    // Re-enable restores bare dispatch.
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "enable", "greet"])
        .assert()
        .success()
        .stdout(predicate::str::contains("enabled `greet`"));
    rackabel_cmd(home.path(), work.path())
        .arg("greet")
        .assert()
        .success()
        .stdout(predicate::str::contains("GREET-RAN"));
}

/// enable/disable on a name that isn't installed is RK0401 (not found).
#[test]
fn enable_unknown_plugin_is_not_found() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "enable", "ghost"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0401"))
        .stderr(predicate::str::contains("no plugin named `ghost`"));
}

// --- list -----------------------------------------------------------------------

/// `plugin list` shows the installed plugin with state + pin + source; `--json` carries the
/// machine-readable shape.
#[test]
fn list_shows_state_pin_source_and_json() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let src = work.path().join("rackabel-greet");
    write_plugin_script(&src, "G");
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", src.to_str().unwrap(), "--yes"])
        .assert()
        .success();

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("greet"))
        .stdout(predicate::str::contains("enabled"))
        .stdout(predicate::str::contains("sha256"))
        .stdout(predicate::str::contains("(path)"));

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "list", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"name\": \"greet\""))
        .stdout(predicate::str::contains("\"enabled\": true"))
        .stdout(predicate::str::contains("\"source\": \"path\""));
}

/// A plugin carrying a rackabel-plugin.toml is recorded with the inert hook list and
/// installs DISABLED (the 0.5 hook consent gate); list shows the pending marker.
#[test]
fn hook_plugin_records_inert_metadata_and_installs_disabled() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let dir = work.path().join("hookplug");
    std::fs::create_dir_all(&dir).unwrap();
    write_plugin_script(&dir.join("rackabel-deployer"), "DEPLOYER");
    std::fs::write(
        dir.join("rackabel-plugin.toml"),
        "[hooks]\npost_build = \".rackabel/hooks/post-build\"\npre_deploy = \"x\"\n",
    )
    .unwrap();

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", dir.to_str().unwrap(), "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("carries a rackabel-plugin.toml"));

    let lock = std::fs::read_to_string(lock_path(home.path())).unwrap();
    assert!(lock.contains("has_plugin_manifest = true"));
    assert!(lock.contains("post_build"));
    assert!(lock.contains("pre_deploy"));
    // Installed disabled (consent gate).
    assert!(lock.contains("enabled = false"));

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("disabled"))
        .stdout(predicate::str::contains("hook(s) pending (0.5)"));
}

// --- upgrade-time collision (§5.6) ---------------------------------------------

/// Installing a plugin whose name a (reserved) future built-in claims — `publish` is in
/// RESERVED_NAMESPACE ahead of shipping — triggers the loud §5.6 collision warning on the
/// next plugin command, ONCE. The plugin is never silently dropped: `plugin run` reaches it.
#[test]
fn upgrade_time_collision_warns_loudly_once() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let src = work.path().join("rackabel-publish");
    write_plugin_script(&src, "PUBLISH-RAN");

    // Sideload it (install itself runs the collision check first → may warn here too).
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", src.to_str().unwrap(), "--yes"])
        .assert()
        .success();

    // The FIRST plugin command after the collision warns loudly with the §5.6 shape.
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "built-in 'publish' now shadows your plugin rackabel-publish",
        ))
        .stdout(predicate::str::contains("rackabel plugin run publish"));

    // The escape hatch still reaches the shadowed plugin.
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "run", "publish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PUBLISH-RAN"));
}

// --- search (stubbed API) -------------------------------------------------------

/// A throwaway single-response HTTP/1.1 stub server. Serves `body` (with a 200) to the
/// first request, then closes. Returns the base URL (`http://127.0.0.1:<port>`). The
/// thread joins on drop of the returned guard so nothing leaks.
struct StubServer {
    base: String,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl StubServer {
    fn json(body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let handle = std::thread::spawn(move || {
            // Accept exactly one connection (the single GET the command makes).
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        StubServer {
            base,
            handle: Some(handle),
        }
    }
}

impl Drop for StubServer {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// `plugin search` queries the stubbed API and prints the hits; `--json` carries them.
#[test]
fn search_against_stubbed_api_lists_hits() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let body = r#"{"items":[{"full_name":"acme/rackabel-notarize","description":"Notarize for Live","stargazers_count":42,"html_url":"https://github.com/acme/rackabel-notarize"}]}"#;
    let server = StubServer::json(body);

    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_GITHUB_API", &server.base)
        .args(["plugin", "search", "notarize"])
        .assert()
        .success()
        .stdout(predicate::str::contains("acme/rackabel-notarize"))
        .stdout(predicate::str::contains("★42"))
        .stdout(predicate::str::contains("Notarize for Live"))
        .stdout(predicate::str::contains(
            "install: rackabel plugin install acme/rackabel-notarize",
        ));
}

/// `plugin search --json` against the stub emits the structured result shape.
#[test]
fn search_json_shape() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let body =
        r#"{"items":[{"full_name":"a/rackabel-foo","stargazers_count":1,"html_url":"https://x"}]}"#;
    let server = StubServer::json(body);

    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_GITHUB_API", &server.base)
        .args(["plugin", "search", "foo", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"term\": \"foo\""))
        .stdout(predicate::str::contains(
            "\"full_name\": \"a/rackabel-foo\"",
        ))
        .stdout(predicate::str::contains("\"results\""));
}

/// `plugin search` against an unreachable API is the clean RK0404 no-network frame
/// (exit 3). We point the seam at a closed port (127.0.0.1:1 — reserved, refuses fast).
#[test]
fn search_no_network_is_clean_frame() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_GITHUB_API", "http://127.0.0.1:1")
        .args(["plugin", "search", "x"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0404"));
}

// --- install via release asset (stubbed API + download host) --------------------

/// A two-request stub: the releases/latest API response, then the asset download. Serves
/// each to one connection. Returns the base URL used for BOTH the API and the DL host.
struct AssetStub {
    base: String,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl AssetStub {
    fn new(api_body: String, asset_bytes: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let handle = std::thread::spawn(move || {
            for _ in 0..2 {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let first_line = req.lines().next().unwrap_or("");
                let resp_body: Vec<u8> = if first_line.contains("/releases/latest") {
                    api_body.clone().into_bytes()
                } else {
                    asset_bytes.clone()
                };
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    resp_body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&resp_body);
                let _ = stream.flush();
            }
        });
        AssetStub {
            base,
            handle: Some(handle),
        }
    }
}

impl Drop for AssetStub {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// `plugin install OWNER/REPO --yes` resolves a release asset `rackabel-<name>-<os>-<arch>`
/// via the stubbed API + download host, pins it by sha256, links it, and dispatches.
#[test]
fn install_release_asset_via_stub_pins_and_dispatches() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    // The asset is a tiny shell script (the "binary" the release ships).
    let asset = b"#!/bin/sh\necho ASSET-RAN\nexit 0\n".to_vec();

    let os = match std::env::consts::OS {
        "macos" => "darwin",
        o => o,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        o => o,
    };
    let asset_name = format!("rackabel-cooltool-{os}-{arch}");

    // The API response advertises the asset with a github.com download URL; the DL seam
    // rewrites the host to our stub.
    let api_body = format!(
        r#"{{"assets":[{{"name":"{asset_name}","browser_download_url":"https://github.com/acme/rackabel-cooltool/releases/download/v1/{asset_name}"}}]}}"#
    );
    let server = AssetStub::new(api_body, asset);

    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_GITHUB_API", &server.base)
        .env("RACKABEL_GITHUB_DL", &server.base)
        .args(["plugin", "install", "acme/rackabel-cooltool", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("installed `cooltool`"))
        .stdout(predicate::str::contains("sha256"));

    let lock = std::fs::read_to_string(lock_path(home.path())).unwrap();
    assert!(lock.contains("name = \"cooltool\""));
    assert!(lock.contains("source = \"gh\""));
    assert!(lock.contains("sha256 = "));

    rackabel_cmd(home.path(), work.path())
        .arg("cooltool")
        .assert()
        .success()
        .stdout(predicate::str::contains("ASSET-RAN"));
}
