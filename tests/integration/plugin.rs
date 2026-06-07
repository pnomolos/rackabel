//! The PATH-subcommand exec path + env contract + built-in precedence, end to end
//! (DESIGN §5.1/§5.2/§5.6). FOUNDATION-owned: these exercise the load-bearing runtime
//! surface (resolve → set the env contract → exec → passthrough) the three 0.4 feature
//! agents build on. Hermetic: a fixture plugin is a tiny shell script in a temp
//! RACKABEL_HOME's managed bin dir; no network, no Live.
//!
//! Gated `#[cfg(unix)]` at the `mod plugin;` site in `main.rs` (the fixture plugins are
//! shell scripts with exec bits), so no inner cfg is needed here.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

use super::common::rackabel_cmd;

/// Drop an executable `rackabel-<name>` into `<home>/.rackabel/plugins/bin`. The script
/// echoes the §5.2 env-contract vars so the test can assert presence/values.
fn install_managed(home: &Path, name: &str) {
    let bin = home.join(".rackabel/plugins/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = "#!/bin/sh\n\
         echo \"ARGS:$*\"\n\
         echo \"RACKABEL=$RACKABEL\"\n\
         echo \"RACKABEL_PLUGIN_API=$RACKABEL_PLUGIN_API\"\n\
         echo \"RACKABEL_VERSION=$RACKABEL_VERSION\"\n\
         echo \"RACKABEL_REGISTRY=$RACKABEL_REGISTRY\"\n\
         echo \"PROJECT_DIR=${RACKABEL_PROJECT_DIR:-UNSET}\"\n\
         echo \"MANIFEST=${RACKABEL_MANIFEST:-UNSET}\"\n\
         exit 0\n";
    let p = bin.join(format!("rackabel-{name}"));
    std::fs::write(&p, script).unwrap();
    let mut perms = std::fs::metadata(&p).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&p, perms).unwrap();
}

/// A bare `rackabel <foo>` with an installed managed plugin runs it, forwards args, and
/// sets the always-present env contract. Outside a project, the two project-only vars are
/// UNSET (not empty) — the §5.2 presence rule.
#[test]
fn external_runs_managed_plugin_with_env_contract_outside_project() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    install_managed(home.path(), "greet");

    rackabel_cmd(home.path(), work.path())
        .args(["greet", "alpha", "beta"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ARGS:alpha beta"))
        .stdout(predicate::str::contains("RACKABEL_PLUGIN_API=1"))
        .stdout(predicate::str::contains("RACKABEL_REGISTRY="))
        .stdout(predicate::str::contains("RACKABEL_VERSION="))
        // Unset-not-empty outside a project.
        .stdout(predicate::str::contains("PROJECT_DIR=UNSET"))
        .stdout(predicate::str::contains("MANIFEST=UNSET"));
}

/// Inside a project the two project-only vars are PRESENT (the manifest path + the root).
#[test]
fn external_sets_project_vars_inside_a_project() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let proj = work.path().join("clip-renamer");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("rackabel.toml"), "[extension]\nname = \"x\"\n").unwrap();
    install_managed(home.path(), "greet");

    rackabel_cmd(home.path(), &proj)
        .arg("greet")
        .assert()
        .success()
        .stdout(predicate::str::contains("PROJECT_DIR="))
        .stdout(predicate::str::contains("clip-renamer"))
        .stdout(predicate::str::contains("rackabel.toml"))
        .stdout(predicate::str::contains("PROJECT_DIR=UNSET").not());
}

/// A plugin's non-zero exit passes through as rackabel's exit (tier-2 passthrough, §7).
#[test]
fn external_plugin_exit_code_passes_through() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let bin = home.path().join(".rackabel/plugins/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let p = bin.join("rackabel-boom");
    std::fs::write(&p, "#!/bin/sh\nexit 7\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();

    rackabel_cmd(home.path(), work.path())
        .arg("boom")
        .assert()
        .failure()
        .code(7);
}

/// A built-in ALWAYS wins (§5.6): a `rackabel-build` plugin can never shadow the built-in
/// `build`. The token routes to the built-in (which errors RK0001 with no manifest), NOT
/// to the plugin — so the plugin's marker never prints.
#[test]
fn builtin_is_never_shadowed_by_a_plugin() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let bin = home.path().join(".rackabel/plugins/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let p = bin.join("rackabel-build");
    std::fs::write(&p, "#!/bin/sh\necho PLUGIN-BUILD-RAN\nexit 0\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();

    rackabel_cmd(home.path(), work.path())
        .arg("build")
        .assert()
        .failure()
        .code(3) // the built-in `build` with no manifest
        .stdout(predicate::str::contains("PLUGIN-BUILD-RAN").not())
        .stderr(predicate::str::contains("RK0001"));
}

/// `plugin run <name>` reaches a plugin EVEN when a built-in shadows the name (the §5.6
/// escape hatch).
#[test]
fn plugin_run_reaches_a_shadowed_plugin() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let bin = home.path().join(".rackabel/plugins/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let p = bin.join("rackabel-publish");
    std::fs::write(&p, "#!/bin/sh\necho SHADOWED-PUBLISH-RAN\nexit 0\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();

    // `publish` is reserved ahead of shipping; the bare token would be the (future)
    // built-in, but `plugin run` invokes the plugin anyway.
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "run", "publish"])
        .assert()
        .success()
        .stdout(predicate::str::contains("SHADOWED-PUBLISH-RAN"));

    // `plugin which publish` reports the shadow + points at `plugin run` (exit 2, RK0103).
    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "which", "publish"])
        .assert()
        .failure()
        .code(2)
        .stdout(predicate::str::contains("shadowed by built-in"))
        .stderr(predicate::str::contains("RK0103"));
}

/// `plugin which <managed>` reports the managed path (exit 0).
#[test]
fn plugin_which_reports_managed_path() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    install_managed(home.path(), "greet");

    rackabel_cmd(home.path(), work.path())
        .args(["plugin", "which", "greet"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rackabel-greet"))
        .stdout(predicate::str::contains("(managed)"));
}

/// Drop an executable `rackabel-<name>` on a controlled `$PATH` dir (NOT the managed bin).
/// The script dumps the full environment to `<dumpfile>` so the test can assert the §5.2
/// presence rules + that `RACKABEL` points at the binary under test, exactly.
fn install_on_path(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    // `env > $RK_DUMP` captures the exact environment the plugin received; we assert on
    // the file, not stdout, so presence/absence is unambiguous (unset != empty).
    let script = "#!/bin/sh\nenv > \"$RK_DUMP\"\nexit 0\n";
    let p = dir.join(format!("rackabel-{name}"));
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The env contract is asserted from INSIDE a fixture plugin: it dumps `env` to a file and
/// the test reads it back, asserting exact presence/absence and that `RACKABEL` is the
/// absolute path of the binary under test (§5.2, the cargo-#15099 lesson).
#[test]
fn env_contract_is_exact_from_inside_the_plugin() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let dump = work.path().join("env-dump.txt");
    install_managed_dumping(home.path(), "probe", &dump);

    let bin = assert_cmd::cargo::cargo_bin("rackabel");
    rackabel_cmd(home.path(), work.path())
        .env("RK_DUMP", &dump)
        .arg("probe")
        .assert()
        .success();

    let env = std::fs::read_to_string(&dump).unwrap();
    let get = |key: &str| -> Option<String> {
        env.lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")).map(str::to_string))
    };

    // Always-present vars (§5.2).
    assert_eq!(get("RACKABEL_PLUGIN_API").as_deref(), Some("1"));
    assert!(get("RACKABEL_VERSION").is_some());
    assert!(get("RACKABEL_REGISTRY").unwrap().ends_with("registry.toml"));
    // RACKABEL is the absolute path of the binary under test (canonicalized), NOT a stale
    // inherited value.
    let rk = get("RACKABEL").expect("RACKABEL present");
    assert!(Path::new(&rk).is_absolute(), "RACKABEL not absolute: {rk}");
    assert_eq!(
        std::fs::canonicalize(&rk).unwrap(),
        std::fs::canonicalize(&bin).unwrap(),
        "RACKABEL must point at the running binary"
    );
    // Project-only vars are UNSET (not empty) outside a project — assert true ABSENCE.
    assert!(
        get("RACKABEL_PROJECT_DIR").is_none(),
        "RACKABEL_PROJECT_DIR must be unset (not empty) outside a project"
    );
    assert!(get("RACKABEL_MANIFEST").is_none());
}

/// A managed `rackabel-<name>` that dumps `env` to `<dump>` (for exact env-contract
/// assertions). Distinct from `install_managed`, which echoes a fixed shape to stdout.
fn install_managed_dumping(home: &Path, name: &str, dump: &Path) {
    let bin = home.join(".rackabel/plugins/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = format!("#!/bin/sh\nenv > {:?}\nexit 0\n", dump.to_string_lossy());
    let p = bin.join(format!("rackabel-{name}"));
    std::fs::write(&p, script).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The managed bin is searched BEFORE `$PATH`: with the SAME name in both, the managed
/// one runs (its marker prints, the PATH one's does not), and the one-time both-locations
/// warning fires on stderr.
#[test]
fn managed_wins_over_path_with_warning() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let path_dir = work.path().join("pathbin");

    // Managed copy prints a managed marker; the PATH copy a different one.
    let mbin = home.path().join(".rackabel/plugins/bin");
    std::fs::create_dir_all(&mbin).unwrap();
    let mp = mbin.join("rackabel-dual");
    std::fs::write(&mp, "#!/bin/sh\necho MANAGED-RAN\nexit 0\n").unwrap();
    std::fs::set_permissions(&mp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(&path_dir).unwrap();
    let pp = path_dir.join("rackabel-dual");
    std::fs::write(&pp, "#!/bin/sh\necho PATH-RAN\nexit 0\n").unwrap();
    std::fs::set_permissions(&pp, std::fs::Permissions::from_mode(0o755)).unwrap();

    let path_env = prepend_path(&path_dir);
    rackabel_cmd(home.path(), work.path())
        .env("PATH", &path_env)
        .arg("dual")
        .assert()
        .success()
        .stdout(predicate::str::contains("MANAGED-RAN"))
        .stdout(predicate::str::contains("PATH-RAN").not())
        // The one-time both-locations warning goes to stderr (never pollutes plugin stdout).
        .stderr(predicate::str::contains("found in both"))
        .stderr(predicate::str::contains("using the managed one"));
}

/// The both-locations warning is ONE-TIME per name: shown on the first invocation, then
/// suppressed (state persisted under RACKABEL_HOME, §5.1).
#[test]
fn both_locations_warning_is_one_time() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let path_dir = work.path().join("pathbin");

    let mbin = home.path().join(".rackabel/plugins/bin");
    std::fs::create_dir_all(&mbin).unwrap();
    let mp = mbin.join("rackabel-dual");
    std::fs::write(&mp, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&mp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::create_dir_all(&path_dir).unwrap();
    let pp = path_dir.join("rackabel-dual");
    std::fs::write(&pp, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&pp, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path_env = prepend_path(&path_dir);

    // First run: warning shown.
    rackabel_cmd(home.path(), work.path())
        .env("PATH", &path_env)
        .arg("dual")
        .assert()
        .success()
        .stderr(predicate::str::contains("found in both"));

    // Second run (same RACKABEL_HOME): warning suppressed.
    rackabel_cmd(home.path(), work.path())
        .env("PATH", &path_env)
        .arg("dual")
        .assert()
        .success()
        .stderr(predicate::str::contains("found in both").not());
}

/// A plugin only on `$PATH` (no managed copy) runs WITHOUT the both-locations warning —
/// there is only one location, so nothing to disambiguate.
#[test]
fn path_only_plugin_runs_without_warning() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let path_dir = work.path().join("pathbin");
    let dump = work.path().join("d.txt");
    install_on_path(&path_dir, "lonely");
    let path_env = prepend_path(&path_dir);

    rackabel_cmd(home.path(), work.path())
        .env("PATH", &path_env)
        .env("RK_DUMP", &dump)
        .arg("lonely")
        .assert()
        .success()
        .stderr(predicate::str::contains("found in both").not());
    assert!(dump.is_file(), "the PATH plugin should have run");
}

/// `plugin which <path-only>` reports the `$PATH` source (exit 0).
#[test]
fn plugin_which_reports_path_source() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let path_dir = work.path().join("pathbin");
    install_on_path(&path_dir, "ponly");
    let path_env = prepend_path(&path_dir);

    rackabel_cmd(home.path(), work.path())
        .env("PATH", &path_env)
        .args(["plugin", "which", "ponly"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rackabel-ponly"))
        .stdout(predicate::str::contains("($PATH)"));
}

/// Prepend `dir` to the current process's `$PATH` so a controlled fixture dir is searched
/// first, without dropping the system PATH (the test still needs `/bin/sh`).
fn prepend_path(dir: &Path) -> std::ffi::OsString {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut parts = vec![dir.to_path_buf()];
    parts.extend(std::env::split_paths(&existing));
    std::env::join_paths(parts).unwrap()
}
