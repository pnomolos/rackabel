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
