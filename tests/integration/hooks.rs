//! Integration tests for the lifecycle-hook engine + wiring (DESIGN §5.3, §5.5, §5.7).
//!
//! Part 1 (HOOK-ENGINE) exercises the `pre_deploy` veto end-to-end through the real
//! `rackabel deploy` binary, using PROJECT-LOCAL `[hooks]` (§5.5 — no manifest, no enable
//! step, implicit trust) so no plugin install ceremony is needed. Part 2 (HOOK-VERBS)
//! exercises the `doctor_check` + `new_template` enumerate hooks, the `plugin enable`
//! consent gate, the pin-change auto-disable, and the `plugin migrate` surface using fixture
//! hook plugins sideloaded via the real install/lock path.
//!
//! Hermetic as always: a temp `HOME`/`RACKABEL_HOME`, a fake Live `.app`, a fake User
//! Library, and shell fixture hook scripts. A prebuilt-fresh bundle makes `deploy` skip its
//! build step so nothing shells to node/esbuild — the deploy reaches the hook + copy stages
//! offline. A HANGING hook fixture is reaped by the timeout machinery itself (asserted).
//! Unix-only (gated where declared in `main.rs`); fixtures are shell scripts.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use crate::common::*;

/// Lay out a pure-JS extension project with a pre-built (fresh) bundle + manifest so
/// `deploy` skips the build step, plus a project-local `[hooks]` table (`hooks_toml` is the
/// `[hooks]` body appended verbatim). Returns the project root.
fn project_with_hooks(work: &Path, slug: &str, hooks_toml: &str) -> std::path::PathBuf {
    let proj = work.join(slug);
    fs::create_dir_all(proj.join("src")).unwrap();
    fs::create_dir_all(proj.join(".rackabel/hooks")).unwrap();
    fs::write(
        proj.join("rackabel.toml"),
        format!(
            "[extension]\nname = \"Clip Renamer\"\nauthor = \"Jane\"\nversion = \"0.1.0\"\n\
             entry = \"src/extension.ts\"\nminimum_api_version = \"1.0.0\"\n\n{hooks_toml}\n"
        ),
    )
    .unwrap();
    fs::write(
        proj.join("src/extension.ts"),
        "export function activate() {}\n",
    )
    .unwrap();

    // Bundle + manifest written AFTER the source so build-if-stale sees them fresh (skip).
    std::thread::sleep(Duration::from_millis(10));
    fs::create_dir_all(proj.join("dist")).unwrap();
    fs::write(
        proj.join("dist/extension.js"),
        "module.exports.activate = function(){};\n",
    )
    .unwrap();
    fs::write(
        proj.join("manifest.json"),
        r#"{"name":"Clip Renamer","author":"Jane","entry":"dist/extension.js","version":"0.1.0","minimumApiVersion":"1.0.0"}"#,
    )
    .unwrap();
    proj
}

/// Write an executable shell script under the project at `rel` with `body`.
fn write_hook(proj: &Path, rel: &str, body: &str) {
    let path = proj.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, body).unwrap();
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).unwrap();
}

fn deploy(home: &Path, proj: &Path, live: &FakeLive, ul: &FakeUserLibrary) -> assert_cmd::Command {
    let mut cmd = rackabel_cmd(home, proj);
    cmd.arg("deploy")
        .arg("--live")
        .arg(live.app_path())
        .arg("--user-library")
        .arg(ul.path());
    assert_cmd::Command::from_std(cmd)
}

#[test]
fn pre_deploy_clean_exit_lets_the_deploy_proceed() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();
    let proj = project_with_hooks(
        work.path(),
        "clip-renamer",
        "[hooks]\npre_deploy = \".rackabel/hooks/pd\"",
    );
    write_hook(
        &proj,
        ".rackabel/hooks/pd",
        "#!/bin/sh\ncat >/dev/null\nexit 0\n",
    );

    deploy(home.path(), &proj, &live, &ul)
        .assert()
        .success()
        .stdout(predicate::str::contains("deployed clip-renamer"));
    assert!(
        ul.path()
            .join("Extensions/clip-renamer/manifest.json")
            .is_file(),
        "a clean pre_deploy allows the copy"
    );
}

#[test]
fn pre_deploy_nonzero_aborts_the_deploy_with_rk1310() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();
    let proj = project_with_hooks(
        work.path(),
        "clip-renamer",
        "[hooks]\npre_deploy = \".rackabel/hooks/pd\"",
    );
    // A notarize-style gate that refuses: nonzero exit ⇒ the deploy aborts.
    write_hook(
        &proj,
        ".rackabel/hooks/pd",
        "#!/bin/sh\ncat >/dev/null\necho 'notarize creds missing' 1>&2\nexit 1\n",
    );

    deploy(home.path(), &proj, &live, &ul)
        .assert()
        .failure()
        .code(1) // PreDeployVetoed is build/runtime class (exit 1).
        .stderr(predicate::str::contains("RK1310"))
        .stderr(predicate::str::contains("aborted"));

    // The veto fired BEFORE any copy — the User Library is untouched (last-good kept).
    assert!(
        !ul.path().join("Extensions/clip-renamer").exists(),
        "a vetoed deploy must not copy anything into the User Library"
    );
}

#[test]
fn pre_deploy_timeout_aborts_fast_with_rk1311() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();
    // A 200ms timeout override against a 30s sleep — the hung gate must be reaped fast.
    let proj = project_with_hooks(
        work.path(),
        "clip-renamer",
        "[hooks]\npre_deploy = \".rackabel/hooks/pd\"\n[hooks.timeouts]\npre_deploy = 200",
    );
    write_hook(&proj, ".rackabel/hooks/pd", "#!/bin/sh\nsleep 30\n");

    let start = std::time::Instant::now();
    deploy(home.path(), &proj, &live, &ul)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("RK1311"))
        .stderr(predicate::str::contains("timed out"));
    assert!(
        start.elapsed() < Duration::from_secs(20),
        "a hung pre_deploy must be reaped by the timeout, not block for the 30s sleep"
    );
    assert!(
        !ul.path().join("Extensions/clip-renamer").exists(),
        "a timed-out gate aborts the deploy — nothing copied"
    );
}

#[test]
fn pre_deploy_receives_the_env_contract_and_payload() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();
    let proj = project_with_hooks(
        work.path(),
        "clip-renamer",
        "[hooks]\npre_deploy = \".rackabel/hooks/pd\"",
    );
    let dump = proj.join("hook-dump.txt");
    // The gate dumps RACKABEL_HOOK_API + its stdin payload, then ALLOWS the deploy.
    write_hook(
        &proj,
        ".rackabel/hooks/pd",
        &format!(
            "#!/bin/sh\nprintf 'API=%s\\n' \"$RACKABEL_HOOK_API\" > {dump}\n\
             printf 'PROJECT=%s\\n' \"$RACKABEL_PROJECT_DIR\" >> {dump}\n\
             printf 'STDIN=' >> {dump}\ncat >> {dump}\nexit 0\n",
            dump = dump.display()
        ),
    );

    deploy(home.path(), &proj, &live, &ul).assert().success();

    let text = fs::read_to_string(&dump).unwrap();
    assert!(text.contains("API=1"), "RACKABEL_HOOK_API present: {text}");
    assert!(
        text.contains("PROJECT="),
        "RACKABEL_PROJECT_DIR present (in a project)"
    );
    let stdin_line = text
        .lines()
        .find(|l| l.starts_with("STDIN="))
        .unwrap()
        .trim_start_matches("STDIN=");
    let v: serde_json::Value = serde_json::from_str(stdin_line).unwrap();
    // The exact §5.3 pre_deploy field set.
    assert!(
        v["manifest_toml"].is_object(),
        "manifest_toml is the parsed object"
    );
    assert_eq!(v["slug"], "clip-renamer");
    assert!(
        v["bundle_path"]
            .as_str()
            .unwrap()
            .ends_with("dist/extension.js")
    );
    assert!(v["user_library"].as_str().unwrap().contains("User Library"));
}

#[test]
fn pre_deploy_first_veto_in_order_wins_and_stops_the_rest() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();
    // Only one project-local pre_deploy is supported per project, so prove the SINGLE gate
    // both vetoes AND leaves a side-effect marker proving it ran (the copy never happens).
    let proj = project_with_hooks(
        work.path(),
        "clip-renamer",
        "[hooks]\npre_deploy = \".rackabel/hooks/pd\"",
    );
    let marker = proj.join("ran.txt");
    write_hook(
        &proj,
        ".rackabel/hooks/pd",
        &format!(
            "#!/bin/sh\ncat >/dev/null\ntouch {}\nexit 2\n",
            marker.display()
        ),
    );

    deploy(home.path(), &proj, &live, &ul)
        .assert()
        .failure()
        .code(1);
    assert!(marker.is_file(), "the gate ran");
    assert!(
        !ul.path().join("Extensions/clip-renamer").exists(),
        "and its veto stopped the copy"
    );
}

#[test]
fn missing_pre_deploy_command_aborts_the_deploy() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();
    // A [hooks] entry pointing at a script that does not exist — a broken gate is a
    // refusal, never a silent pass.
    let proj = project_with_hooks(
        work.path(),
        "clip-renamer",
        "[hooks]\npre_deploy = \".rackabel/hooks/does-not-exist\"",
    );

    deploy(home.path(), &proj, &live, &ul)
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("RK1310"));
    assert!(
        !ul.path().join("Extensions/clip-renamer").exists(),
        "a broken gate aborts; nothing is copied"
    );
}

#[test]
fn no_hooks_table_deploys_normally() {
    // A project WITHOUT a [hooks] table deploys exactly as before — hooks are opt-in.
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();
    let proj = project_with_hooks(work.path(), "clip-renamer", "");

    deploy(home.path(), &proj, &live, &ul)
        .assert()
        .success()
        .stdout(predicate::str::contains("deployed clip-renamer"));
}

// ===========================================================================================
// Part 2 (HOOK-VERBS): the doctor_check + new_template enumerate hooks, the `plugin enable`
// consent gate, the pin-change auto-disable, and the `plugin migrate` surface.
// ===========================================================================================

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
