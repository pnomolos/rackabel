//! `rackabel build` (Extension) integration tests.
//!
//! Two tiers, per the build assignment:
//!   (a) everything up to the esbuild exec — `--print-config`, `--dry-run`, the
//!       no-entry / no-node error paths — runs unconditionally (no node/esbuild
//!       needed: `--print-config`/`--dry-run` return before node resolution);
//!   (b) a real end-to-end build is gated behind detecting a usable node + esbuild
//!       and self-skips with a clear message otherwise.

use crate::common::*;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

/// `build --print-config` dumps the resolved esbuild config and exits 0, baking the
/// banner + `define global=globalThis` + cjs/node, with no externals for the fixture.
/// Needs no node (it returns before node resolution).
#[test]
fn print_config_dumps_resolved_config() {
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("ext-fixture");

    rackabel_cmd(home.path(), &proj)
        .args(["build", "--print-config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"format\": \"cjs\""))
        .stdout(predicate::str::contains("\"platform\": \"node\""))
        .stdout(predicate::str::contains("\"global\": \"globalThis\""))
        // The polyfill banner is baked in (a distinctive fragment).
        .stdout(predicate::str::contains(
            "runInThisContext(\\\"Request\\\")",
        ))
        .stdout(predicate::str::contains("\"sourcesContent\": false"))
        // dev build => sourcemap on, minify off.
        .stdout(predicate::str::contains("\"sourcemap\": true"))
        .stdout(predicate::str::contains("\"minify\": false"));
}

/// `--release --print-config` flips minify on / sourcemap off and turns typecheck on.
#[test]
fn print_config_release_flips_minify_sourcemap() {
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("ext-fixture");

    rackabel_cmd(home.path(), &proj)
        .args(["build", "--release", "--print-config"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"minify\": true"))
        .stdout(predicate::str::contains("\"sourcemap\": false"))
        .stdout(predicate::str::contains("\"typecheck\": true"));
}

/// `--dry-run` prints the plan and mutates nothing (no dist/, no manifest.json).
#[test]
fn dry_run_mutates_nothing() {
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("ext-fixture");

    rackabel_cmd(home.path(), &proj)
        .args(["build", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("planned build steps"))
        .stdout(predicate::str::contains("nothing was changed"));

    assert!(!proj.join("dist").exists(), "dry-run created dist/");
    assert!(
        !proj.join("manifest.json").exists(),
        "dry-run wrote manifest.json"
    );
}

/// `--dry-run --json` is machine-readable and still mutates nothing.
#[test]
fn dry_run_json() {
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("ext-fixture");

    rackabel_cmd(home.path(), &proj)
        .args(["build", "--dry-run", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"dry_run\": true"));
    assert!(!proj.join("manifest.json").exists());
}

/// A project whose entry source is missing is a build error (exit 1) that names the
/// missing file — not a raw esbuild trace. Gated on node so we reach the entry check.
#[test]
fn missing_entry_is_build_error() {
    let node = match which_node() {
        Some(n) => n,
        None => {
            eprintln!("skipping missing_entry_is_build_error: no node on PATH");
            return;
        }
    };
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("ext-fixture");
    std::fs::remove_file(proj.join("src/extension.ts")).unwrap();

    rackabel_cmd(home.path(), &proj)
        .env("ABLETON_EH_NODE", &node)
        .arg("build")
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("entry source was not found"));
}

/// With no usable node anywhere — the override node fails `--version`, the (overridden)
/// Live's bundled node also fails, and PATH has no node — build is an environment
/// error (exit 3) with the "install Live / Node" remedy, never a raw "node not found".
///
/// Cannot run on Unix only because it relies on an empty-PATH `which` miss and shell
/// stubs; skips on non-unix.
#[cfg(unix)]
#[test]
fn no_usable_node_is_environment_error() {
    use std::os::unix::fs::PermissionsExt;

    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("ext-fixture");

    // A fake Live whose *bundled* node errors on --version (so node::resolve rejects it).
    let live_root = TempDir::new().unwrap();
    let host = live_root
        .path()
        .join("Broken.app/Contents/Helpers/ExtensionHost");
    std::fs::create_dir_all(&host).unwrap();
    std::fs::write(host.join("ExtensionHostNodeModule.node"), b"").unwrap();
    let broken_node = host.join("node");
    std::fs::write(&broken_node, "#!/bin/sh\nexit 1\n").unwrap();
    let mut p = std::fs::metadata(&broken_node).unwrap().permissions();
    p.set_mode(0o755);
    std::fs::set_permissions(&broken_node, p).unwrap();

    // A separate broken override node (resolve() tries the override first).
    let fake = proj.join("not-node");
    std::fs::write(&fake, "#!/bin/sh\nexit 1\n").unwrap();
    let mut p2 = std::fs::metadata(&fake).unwrap().permissions();
    p2.set_mode(0o755);
    std::fs::set_permissions(&fake, p2).unwrap();

    rackabel_cmd(home.path(), &proj)
        .env("ABLETON_APP", live_root.path().join("Broken.app"))
        .env("ABLETON_EH_NODE", &fake)
        // Empty PATH so the `which node` fallback finds nothing.
        .env("PATH", "/nonexistent-dir-for-rackabel-test")
        .arg("build")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0305"))
        .stderr(predicate::str::contains("Node runtime"));
}

/// REAL end-to-end build: resolves node + vendors a reachable esbuild into the temp
/// project, then builds. Verifies the bundle is emitted, the banner is present in the
/// output, `manifest.json` is generated with the five fields + dist entry, and the
/// success line carries a build hash. Self-skips if node+esbuild aren't available.
#[test]
fn end_to_end_real_build() {
    // Find a node that can resolve esbuild from *somewhere* on this machine. We probe
    // the current crate dir's ancestors and the home dir as source roots.
    let probe_roots = esbuild_source_roots();
    let (node, source_root) = match probe_roots
        .iter()
        .find_map(|r| usable_node_with_esbuild(r).map(|n| (n, r.clone())))
    {
        Some(pair) => pair,
        None => {
            eprintln!(
                "skipping end_to_end_real_build: no node+esbuild found (set up a node \
                 with esbuild installed to run the real build path)"
            );
            return;
        }
    };

    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("ext-fixture");
    assert!(
        vendor_esbuild_into(&node, &source_root, &proj),
        "could not vendor esbuild into the temp project"
    );

    rackabel_cmd(home.path(), &proj)
        .env("ABLETON_EH_NODE", &node)
        .arg("build")
        .assert()
        .success()
        .stdout(predicate::str::contains("rebuilt in"));

    // Bundle emitted.
    let bundle = proj.join("dist/extension.js");
    assert!(bundle.is_file(), "dist/extension.js missing");
    let js = std::fs::read_to_string(&bundle).unwrap();
    // The polyfill banner is baked in unconditionally.
    assert!(
        js.contains("_ehVm.runInThisContext(\"Request\")"),
        "banner not present in output bundle"
    );

    // manifest.json generated with the five SDK fields + dist entry.
    let manifest = proj.join("manifest.json");
    assert!(manifest.is_file(), "manifest.json not generated");
    let m: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    assert_eq!(m["name"], "Clip Renamer");
    assert_eq!(m["entry"], "dist/extension.js");
    assert_eq!(m["version"], "0.1.0"); // inferred default
    assert_eq!(m["minimumApiVersion"], "1.0.0"); // inferred default

    // Build hash persisted to state.
    let state = proj.join(".rackabel/state.toml");
    assert!(state.is_file(), "state.toml not written");
    assert!(
        std::fs::read_to_string(&state)
            .unwrap()
            .contains("build_hash")
    );
}

/// Candidate source roots from which esbuild might be resolvable on this machine.
fn esbuild_source_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    // The crate's own ancestors (a sibling project may carry node_modules).
    let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    for a in here.ancestors() {
        roots.push(a.to_path_buf());
    }
    if let Some(home) = home::home_dir() {
        roots.push(home);
    }
    roots
}
