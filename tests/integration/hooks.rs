//! Lifecycle-hook verbs end to end (DESIGN §5.3/§5.7): the `doctor_check` + `new_template`
//! enumerate hooks, the `plugin enable` consent gate, the pin-change auto-disable, and the
//! `plugin migrate` surface. HOOK-VERBS-owned.
//!
//! Hermetic: a fixture hook is a tiny shell script sideloaded as a `rackabel-<name>` plugin
//! carrying a `rackabel-plugin.toml`; the hook subprocess runs under the timeout machinery
//! (a HANGING hook fixture is reaped by that machinery itself — asserted below). No real
//! Live, no network; a fake Live `.app` + User Library are used only so `doctor`'s built-in
//! checks don't short-circuit before the hook rows. Unix-only at the `mod` site in `main.rs`
//! (fixture scripts have exec bits).

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use super::common::{FakeArch, FakeLayout, FakeLive, FakeUserLibrary, rackabel_cmd};

/// Install a fixture hook plugin: a `rackabel-<name>` executable + a `rackabel-plugin.toml`
/// declaring `<kind> = "hook.sh"`, with `hook.sh` set to `body`. Sideloaded via
/// `plugin install <dir> --yes` so it goes through the real install/lock path (installs
/// DISABLED — the consent gate). The hook's wall-clock timeout uses the 30s default — fast
/// enough not to flake under test-machine load (only the dedicated HANG test forces a short
/// timeout, via [`install_hook_plugin_timeout`]).
fn install_hook_plugin(home: &Path, work: &Path, name: &str, kind: &str, body: &str) {
    install_hook_plugin_timeout(home, work, name, kind, body, None);
}

/// As [`install_hook_plugin`], but with an explicit per-hook timeout override in ms (used by
/// the HANG test so it does not wait the full 30s default).
fn install_hook_plugin_timeout(
    home: &Path,
    work: &Path,
    name: &str,
    kind: &str,
    body: &str,
    timeout_ms: Option<u64>,
) {
    let dir = work.join(format!("src-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    // The executable (a plain tier-2 entry point; unused by the hook path but required so the
    // sideload resolves a `rackabel-<name>`).
    let exe = dir.join(format!("rackabel-{name}"));
    std::fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The hook script.
    let hook = dir.join("hook.sh");
    std::fs::write(&hook, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    // The manifest binding the kind to the hook script (relative to the plugin store root),
    // with an optional [hooks.timeouts] override.
    let timeouts = timeout_ms
        .map(|ms| format!("[hooks.timeouts]\n{kind} = {ms}\n"))
        .unwrap_or_default();
    std::fs::write(
        dir.join("rackabel-plugin.toml"),
        format!("[hooks]\n{kind} = \"hook.sh\"\n{timeouts}"),
    )
    .unwrap();

    rackabel_cmd(home, work)
        .args(["plugin", "install", dir.to_str().unwrap(), "--yes"])
        .assert()
        .success();
}

/// Enable a plugin's hooks non-interactively (consent via --yes).
fn enable_yes(home: &Path, work: &Path, name: &str) {
    rackabel_cmd(home, work)
        .args(["plugin", "enable", name, "--yes"])
        .assert()
        .success();
}

/// A `doctor` command pre-wired with a fake Live + User Library so the built-in checks run
/// and the doctor_check hook rows are appended (and rendered, since a warn/fail forces the
/// full checklist). `--verbose` is added so passing rows render too where needed.
fn doctor_cmd(home: &Path, work: &Path, live: &Path, ul: &Path) -> std::process::Command {
    let mut cmd = rackabel_cmd(home, work);
    cmd.env("RACKABEL_DOCTOR_LIVE_RUNNING", "0")
        .arg("--live")
        .arg(live)
        .arg("--user-library")
        .arg(ul)
        .arg("doctor");
    cmd
}

// --- doctor_check: the four a-d combinations ----------------------------------------

#[test]
fn doctor_check_line_wins_on_exit_zero_combination_a() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    install_hook_plugin(
        home.path(),
        work.path(),
        "notarize",
        "doctor_check",
        r#"echo '{"symbol":"warn","message":"notarize creds missing","help":"set NOTARY_KEY"}'; exit 0"#,
    );
    enable_yes(home.path(), work.path(), "notarize");

    doctor_cmd(home.path(), work.path(), live.app_path(), ul.path())
        .assert()
        .stdout(predicate::str::contains("notarize creds missing"))
        .stdout(predicate::str::contains("set NOTARY_KEY"))
        // attributed to the plugin
        .stdout(predicate::str::contains("plugin notarize"));
}

#[test]
fn doctor_check_line_wins_on_nonzero_exit_combination_b() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    install_hook_plugin(
        home.path(),
        work.path(),
        "checker",
        "doctor_check",
        // A valid fail row, then a nonzero exit — the row still shows (combination b).
        r#"echo '{"symbol":"fail","message":"thing is broken","help":"fix the thing"}'; exit 7"#,
    );
    enable_yes(home.path(), work.path(), "checker");

    doctor_cmd(home.path(), work.path(), live.app_path(), ul.path())
        .assert()
        .failure() // a fail row → environment exit (3)
        .code(3)
        .stdout(predicate::str::contains("thing is broken"))
        .stdout(predicate::str::contains("fix the thing"));
}

#[test]
fn doctor_check_exit_zero_no_line_is_pass_combination_c() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    install_hook_plugin(
        home.path(),
        work.path(),
        "silent",
        "doctor_check",
        "echo 'just some log chatter'; exit 0",
    );
    enable_yes(home.path(), work.path(), "silent");

    // A silent pass renders only under --verbose (passing rows collapse otherwise). Use
    // --verbose to assert the pass row is present and attributed.
    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_DOCTOR_LIVE_RUNNING", "0")
        .args(["--live", live.app_path().to_str().unwrap()])
        .args(["--user-library", ul.path().to_str().unwrap()])
        .args(["doctor", "--verbose"])
        .assert()
        .stdout(predicate::str::contains("plugin silent"))
        .stdout(predicate::str::contains("doctor_check passed"));
}

#[test]
fn doctor_check_nonzero_no_line_is_generic_fail_combination_d() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    install_hook_plugin(
        home.path(),
        work.path(),
        "crasher",
        "doctor_check",
        "echo oops 1>&2; exit 3",
    );
    enable_yes(home.path(), work.path(), "crasher");

    doctor_cmd(home.path(), work.path(), live.app_path(), ul.path())
        .assert()
        .failure()
        .code(3)
        .stdout(predicate::str::contains("doctor_check crasher failed"));
}

#[test]
fn doctor_check_timeout_is_generic_fail_and_is_reaped() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    // The hook sleeps far past its 400ms timeout (set in install_hook_plugin). The engine's
    // SIGTERM→SIGKILL machinery must reap it; the run must return WELL before the 30s sleep,
    // proving the hang is bounded (combination d = generic fail).
    install_hook_plugin_timeout(
        home.path(),
        work.path(),
        "hanger",
        "doctor_check",
        "sleep 30; echo never",
        Some(400), // a short timeout so the test does not wait the 30s default.
    );
    enable_yes(home.path(), work.path(), "hanger");

    let start = std::time::Instant::now();
    doctor_cmd(home.path(), work.path(), live.app_path(), ul.path())
        .assert()
        .failure()
        .code(3)
        .stdout(predicate::str::contains("doctor_check hanger failed"));
    assert!(
        start.elapsed() < std::time::Duration::from_secs(20),
        "the hanging hook should be reaped by the timeout, took {:?}",
        start.elapsed()
    );
}

#[test]
fn doctor_check_malformed_line_falls_back_to_exit_code() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    // Malformed JSON on stdout is NOT a contract line ⇒ treated as log; exit 0 ⇒ pass (c).
    install_hook_plugin(
        home.path(),
        work.path(),
        "malformed",
        "doctor_check",
        r#"echo '{"symbol":"bogus"'; exit 0"#,
    );
    enable_yes(home.path(), work.path(), "malformed");

    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_DOCTOR_LIVE_RUNNING", "0")
        .args(["--live", live.app_path().to_str().unwrap()])
        .args(["--user-library", ul.path().to_str().unwrap()])
        .args(["doctor", "--verbose"])
        .assert()
        .stdout(predicate::str::contains("doctor_check passed"));
}

#[test]
fn doctor_check_unenabled_plugin_contributes_no_row() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    // Installed but NOT enabled → no consent → no hook row.
    install_hook_plugin(
        home.path(),
        work.path(),
        "dormant",
        "doctor_check",
        r#"echo '{"symbol":"fail","message":"should not appear"}'; exit 1"#,
    );
    // (no enable)

    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_DOCTOR_LIVE_RUNNING", "0")
        .args(["--live", live.app_path().to_str().unwrap()])
        .args(["--user-library", ul.path().to_str().unwrap()])
        .args(["doctor", "--verbose"])
        .assert()
        .stdout(predicate::str::contains("should not appear").not())
        .stdout(predicate::str::contains("plugin dormant").not());
}

#[test]
fn doctor_check_outside_project_omits_project_fields() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap(); // NOT a project (no rackabel.toml)
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    // The hook fails LOUDLY only if it sees a project_dir; outside a project it must not.
    install_hook_plugin(
        home.path(),
        work.path(),
        "tolerant",
        "doctor_check",
        r#"in=$(cat); case "$in" in *project_dir*) echo '{"symbol":"fail","message":"leaked project"}';; *) echo '{"symbol":"ok","message":"no-project tolerated"}';; esac"#,
    );
    enable_yes(home.path(), work.path(), "tolerant");

    rackabel_cmd(home.path(), work.path())
        .env("RACKABEL_DOCTOR_LIVE_RUNNING", "0")
        .args(["--live", live.app_path().to_str().unwrap()])
        .args(["--user-library", ul.path().to_str().unwrap()])
        .args(["doctor", "--verbose"])
        .assert()
        .stdout(predicate::str::contains("no-project tolerated"))
        .stdout(predicate::str::contains("leaked project").not());
}

// --- new_template enumerate hook ----------------------------------------------------

/// A `new_template` hook from an ENABLED plugin contributes a wizard CHOICE. Under
/// `--no-input` the wizard never prompts, so the enumerate pick-list is skipped and the
/// hook does not even run (the built-in default path is taken) — assert it stays out of the
/// way and `new` still proceeds (here it hits the deterministic no-name usage error, proving
/// the enumerate path did not hijack the flow).
#[test]
fn new_template_hook_does_not_run_under_no_input() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    install_hook_plugin(
        home.path(),
        work.path(),
        "starter",
        "new_template",
        // If this ever ran under --no-input it would write a marker file we can detect.
        "echo gh:acme/house-starter@v1 > should-not-exist.marker; echo gh:acme/house-starter@v1",
    );
    enable_yes(home.path(), work.path(), "starter");

    // --no-input new (no name) → the deterministic usage error, NOT a hook invocation.
    rackabel_cmd(home.path(), work.path())
        .args(["new", "--no-input"])
        .assert()
        .failure()
        .code(2);
    // The enumerate hook must not have run under --no-input.
    assert!(
        !work.path().join("should-not-exist.marker").exists(),
        "new_template hook must not run under --no-input"
    );
}

// --- consent gate (§5.7) ------------------------------------------------------------

/// Enabling a hook plugin prints WHAT hooks will run at WHICH points and requires consent.
/// `--no-input` REFUSES (no implicit consent) with RK0403; the plugin stays disabled.
#[test]
fn enable_hook_plugin_no_input_refuses_consent() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    install_hook_plugin(home.path(), work.path(), "notarize", "pre_deploy", "exit 0");

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "enable", "notarize", "--no-input"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0403"))
        .stderr(predicate::str::contains("was not confirmed"));

    // Still disabled.
    let lock = std::fs::read_to_string(home.path().join(".rackabel/plugins.lock")).unwrap();
    assert!(lock.contains("enabled = false"), "lock: {lock}");
}

/// `--yes` scripts the consent: the transcript shows the hooks + lifecycle points, and the
/// plugin ends up enabled.
#[test]
fn enable_hook_plugin_yes_consents_and_enables() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    install_hook_plugin(home.path(), work.path(), "notarize", "pre_deploy", "exit 0");

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "enable", "notarize", "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("consents to running"))
        .stdout(predicate::str::contains("pre_deploy"))
        .stdout(predicate::str::contains("enabled `notarize`"));

    let lock = std::fs::read_to_string(home.path().join(".rackabel/plugins.lock")).unwrap();
    assert!(lock.contains("enabled = true"), "lock: {lock}");
}

/// A pin change (a `--force` reinstall at the same name with different bytes) DISABLES the
/// plugin's hooks in the lock and prints the re-enable instruction — new code never runs
/// on-save under consent given for the old code (§5.7).
#[test]
fn pin_change_disables_hooks_and_requires_re_enable() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();

    // Install + consent to v1.
    let dir = work.path().join("v1");
    write_hook_plugin_dir(&dir, "notarize", "pre_deploy", "exit 0", "VERSION-1");
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", dir.to_str().unwrap(), "--yes"])
        .assert()
        .success();
    enable_yes(home.path(), work.path(), "notarize");

    // Confirm enabled.
    let lock = std::fs::read_to_string(home.path().join(".rackabel/plugins.lock")).unwrap();
    assert!(lock.contains("enabled = true"));

    // Reinstall DIFFERENT bytes past the pin with --force (a code change).
    let dir2 = work.path().join("v2");
    write_hook_plugin_dir(
        &dir2,
        "notarize",
        "pre_deploy",
        "exit 0",
        "VERSION-2-DIFFERENT",
    );
    rackabel_cmd(home.path(), work.path())
        .args([
            "plugin",
            "install",
            dir2.to_str().unwrap(),
            "--yes",
            "--force",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("re-consent").or(predicate::str::contains("DISABLED")));

    // Now DISABLED again — old consent does not carry to new code.
    let lock = std::fs::read_to_string(home.path().join(".rackabel/plugins.lock")).unwrap();
    assert!(
        lock.contains("enabled = false"),
        "pin change must disable the hook plugin; lock: {lock}"
    );
}

/// Write a hook plugin directory whose executable carries a distinct `marker` in its bytes
/// (so two versions hash differently → a pin change), plus the hook script + manifest.
fn write_hook_plugin_dir(dir: &Path, name: &str, kind: &str, body: &str, marker: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let exe = dir.join(format!("rackabel-{name}"));
    std::fs::write(&exe, format!("#!/bin/sh\n# {marker}\nexit 0\n")).unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let hook = dir.join("hook.sh");
    std::fs::write(&hook, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        dir.join("rackabel-plugin.toml"),
        format!("[hooks]\n{kind} = \"hook.sh\"\n"),
    )
    .unwrap();
}

// --- plugin migrate surface ---------------------------------------------------------

#[test]
fn migrate_hook_api_one_is_nothing_to_migrate() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let dir = work.path().join("p1");
    write_migrate_plugin(&dir, "p1", Some(1));
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", dir.to_str().unwrap(), "--yes"])
        .assert()
        .success();

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "migrate", "p1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("nothing to migrate"));
}

#[test]
fn migrate_higher_hook_api_is_unsupported_frame() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let dir = work.path().join("p2");
    write_migrate_plugin(&dir, "p2", Some(2));
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", dir.to_str().unwrap(), "--yes"])
        .assert()
        .success();

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "migrate", "p2"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("RK0104"))
        .stderr(predicate::str::contains("hook_api 2"));
}

#[test]
fn migrate_json_carries_versions_and_decision() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let dir = work.path().join("p3");
    write_migrate_plugin(&dir, "p3", Some(1));
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "install", dir.to_str().unwrap(), "--yes"])
        .assert()
        .success();

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "migrate", "p3", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"declared_hook_api\": 1"))
        .stdout(predicate::str::contains("\"supported_hook_api\": 1"))
        .stdout(predicate::str::contains(
            "\"decision\": \"nothing-to-migrate\"",
        ));
}

#[test]
fn migrate_unknown_plugin_is_not_found() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "migrate", "ghost"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0401"));
}

/// Write a plugin dir declaring `hook_api` (or omitting it) plus one hook so it has a
/// manifest. The executable distinguishes it as a `rackabel-<name>` sideload.
fn write_migrate_plugin(dir: &Path, name: &str, hook_api: Option<u32>) {
    std::fs::create_dir_all(dir).unwrap();
    let exe = dir.join(format!("rackabel-{name}"));
    std::fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let hook = dir.join("hook.sh");
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let api_line = hook_api
        .map(|v| format!("hook_api = {v}\n"))
        .unwrap_or_default();
    std::fs::write(
        dir.join("rackabel-plugin.toml"),
        format!("{api_line}[hooks]\npost_build = \"hook.sh\"\n"),
    )
    .unwrap();
}
