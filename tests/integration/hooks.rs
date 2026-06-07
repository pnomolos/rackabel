//! Integration tests for the lifecycle-hook engine + wiring (DESIGN §5.3, §5.5).
//!
//! These exercise the `pre_deploy` veto end-to-end through the real `rackabel deploy`
//! binary, using PROJECT-LOCAL `[hooks]` (§5.5 — no manifest, no enable step, implicit
//! trust) so no plugin install ceremony is needed. Hermetic as always: a temp
//! `HOME`/`RACKABEL_HOME`, a fake Live `.app`, a fake User Library, and shell fixture
//! hook scripts in the project dir. A prebuilt-fresh bundle makes `deploy` skip its build
//! step so nothing shells to node/esbuild — the deploy reaches the hook + copy stages
//! offline. Unix-only (gated where declared in `main.rs`); fixtures are shell scripts.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

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
