//! `rackabel dev test` (§3.8) integration tests — the no-Live CI entry point.
//!
//! Two tiers (mirroring build.rs):
//!   (a) the "nothing to test" / routing error paths run unconditionally (no node);
//!   (b) the real build + stub-runner paths gate on a usable node + esbuild and
//!       self-skip with a message otherwise. The TEST RUNNER itself is a tiny
//!       dependency-free node stub committed in each fixture, so these tests never need
//!       a real vitest network install — only a node that can bundle (esbuild) the
//!       trivial extension. The runner's pass/fail is driven by `RK_TEST_OUTCOME`.
//!
//! Every test pins `RACKABEL_HOME`/`HOME` to temp dirs; nothing touches real Live, the
//! real User Library, or real host processes (there are none on this path).

use std::path::{Path, PathBuf};

use crate::common::*;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

// --- tier (a): no node needed ---------------------------------------------------

/// With nothing registered and no project in the cwd, `dev test` has nothing to test →
/// `RK0001` (exit 3). Crucially it routes to the test runner, NOT the bare loop
/// (`RK0307`).
#[test]
fn nothing_to_test_is_no_manifest() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    rackabel_cmd(home.path(), work.path())
        .args(["dev", "test", "--no-input"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0001"))
        .stderr(predicate::str::contains("RK0307").not());
}

/// An operand that is neither a registered name nor a project path is `RK0001` with the
/// "register it, or pass a path" remedy — and never prompts (non-interactive always).
#[test]
fn unknown_target_is_no_manifest() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    rackabel_cmd(home.path(), work.path())
        .args(["dev", "test", "does-not-exist"])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0001"))
        .stderr(predicate::str::contains("not a registered extension"));
}

// --- tier (b): real build + stub runner -----------------------------------------

/// A vitest-harness target whose stub runner passes → exit 0; the human summary shows
/// the build banner and a passing target line.
#[test]
fn vitest_pass_human() {
    let Some((node, src)) = first_node_with_esbuild() else {
        eprintln!("skipping vitest_pass_human: no node+esbuild");
        return;
    };
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("dev-test-vitest");
    assert!(vendor_esbuild_into(&node, &src, &proj), "vendor esbuild");

    rackabel_cmd(home.path(), &proj)
        .env("ABLETON_EH_NODE", &node)
        .args(["dev", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rebuilt in")) // build banner
        .stdout(predicate::str::contains("3 passed"));
}

/// The same target with `RK_TEST_OUTCOME=fail` → the runner exits 1, so `dev test`
/// exits 1 (build/runtime, RK1308) — the CI gate fails deterministically.
#[test]
fn vitest_fail_is_exit_1() {
    let Some((node, src)) = first_node_with_esbuild() else {
        eprintln!("skipping vitest_fail_is_exit_1: no node+esbuild");
        return;
    };
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("dev-test-vitest");
    assert!(vendor_esbuild_into(&node, &src, &proj), "vendor esbuild");

    rackabel_cmd(home.path(), &proj)
        .env("ABLETON_EH_NODE", &node)
        .env("RK_TEST_OUTCOME", "fail")
        .args(["dev", "test"])
        .assert()
        .failure()
        .code(1)
        .stderr(predicate::str::contains("RK1308"));
}

/// `--json` emits the EXACT §3.8 wrapper envelope and OWNS stdout (no build banner, no
/// runner reporter leaks): one target, harness present, the parsed counts, top-level
/// passed/failed. The whole of stdout must parse as a single JSON object.
#[test]
fn json_envelope_shape_pass() {
    let Some((node, src)) = first_node_with_esbuild() else {
        eprintln!("skipping json_envelope_shape_pass: no node+esbuild");
        return;
    };
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("dev-test-vitest");
    assert!(vendor_esbuild_into(&node, &src, &proj), "vendor esbuild");

    let out = rackabel_cmd(home.path(), &proj)
        .env("ABLETON_EH_NODE", &node)
        .args(["dev", "test", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is a single JSON object");
    assert_eq!(v["passed"], true);
    assert_eq!(v["failed"], false);
    let targets = v["targets"].as_array().expect("targets array");
    assert_eq!(targets.len(), 1);
    let t = &targets[0];
    assert_eq!(t["name"], "dev-test-vitest"); // slug (single-project cwd path)
    assert_eq!(t["harness_present"], true);
    assert_eq!(t["skipped_no_harness"], false);
    assert_eq!(t["exit_code"], 0);
    assert_eq!(t["passed"], 3);
    assert_eq!(t["failed"], 0);
}

/// `--json` on a failing run: top-level `failed:true`, the target's `exit_code` is the
/// runner's, and the process exits 1 — while stdout stays a single parseable object.
#[test]
fn json_envelope_shape_fail() {
    let Some((node, src)) = first_node_with_esbuild() else {
        eprintln!("skipping json_envelope_shape_fail: no node+esbuild");
        return;
    };
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("dev-test-vitest");
    assert!(vendor_esbuild_into(&node, &src, &proj), "vendor esbuild");

    let out = rackabel_cmd(home.path(), &proj)
        .env("ABLETON_EH_NODE", &node)
        .env("RK_TEST_OUTCOME", "fail")
        .args(["dev", "test", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is a single JSON object");
    assert_eq!(v["passed"], false);
    assert_eq!(v["failed"], true);
    let t = &v["targets"].as_array().unwrap()[0];
    assert_eq!(t["exit_code"], 1);
    assert_eq!(t["failed"], 1);
}

/// `--` forwards args verbatim to the runner (and `--bail` adds vitest's `--bail=1`).
/// The stub echoes its argv as `RK_ARGS:[...]`; assert both the forwarded flag and the
/// bail flag arrive.
#[test]
fn forwards_runner_args_and_bail() {
    let Some((node, src)) = first_node_with_esbuild() else {
        eprintln!("skipping forwards_runner_args_and_bail: no node+esbuild");
        return;
    };
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("dev-test-vitest");
    assert!(vendor_esbuild_into(&node, &src, &proj), "vendor esbuild");

    // --raw so the runner's stdout (the RK_ARGS line) streams through. The global flag
    // is placed before the subcommand (clap routes trailing `last=true` args verbatim,
    // so a global flag after `--bail` would be swallowed into the runner args).
    rackabel_cmd(home.path(), &proj)
        .env("ABLETON_EH_NODE", &node)
        .args(["--raw", "dev", "test", "--bail", "--", "-t", "my case"])
        .assert()
        .success()
        .stdout(predicate::str::contains("RK_ARGS:"))
        .stdout(predicate::str::contains("--bail=1"))
        .stdout(predicate::str::contains("-t"))
        .stdout(predicate::str::contains("my case"));
}

/// A target with only a `*:headless` script (no vitest `test`) runs the headless runner
/// (§3.8 second precedence): harness present, passes.
#[test]
fn headless_script_runs() {
    let Some((node, src)) = first_node_with_esbuild() else {
        eprintln!("skipping headless_script_runs: no node+esbuild");
        return;
    };
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("dev-test-headless");
    assert!(vendor_esbuild_into(&node, &src, &proj), "vendor esbuild");

    let out = rackabel_cmd(home.path(), &proj)
        .env("ABLETON_EH_NODE", &node)
        .args(["dev", "test", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let t = &v["targets"].as_array().unwrap()[0];
    assert_eq!(t["harness_present"], true);
    assert_eq!(t["skipped_no_harness"], false);
    assert_eq!(t["exit_code"], 0);
}

/// A target with NO harness runs the best-effort generic activate() smoke (§3.8 third
/// precedence): `skipped_no_harness:true`, harness absent, the smoke passes (exit 0).
#[test]
fn no_harness_runs_smoke_skipped() {
    let Some((node, src)) = first_node_with_esbuild() else {
        eprintln!("skipping no_harness_runs_smoke_skipped: no node+esbuild");
        return;
    };
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("dev-test-nohraness");
    assert!(vendor_esbuild_into(&node, &src, &proj), "vendor esbuild");

    let out = rackabel_cmd(home.path(), &proj)
        .env("ABLETON_EH_NODE", &node)
        .args(["dev", "test", "--json"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["passed"], true);
    let t = &v["targets"].as_array().unwrap()[0];
    assert_eq!(t["harness_present"], false);
    assert_eq!(t["skipped_no_harness"], true);
    assert_eq!(t["exit_code"], 0);
}

/// `--bail` stops after the first failing target: pass two PATH operands (both fail),
/// assert only ONE target appears in the `--json` envelope and the process exits 1.
/// (Uses explicit path operands rather than the registry so the test does not depend on
/// the registry-agent `dev register` verb, which is a sibling-agent's file.)
#[test]
fn bail_stops_after_first_failing_target() {
    let Some((node, src)) = first_node_with_esbuild() else {
        eprintln!("skipping bail_stops_after_first_failing_target: no node+esbuild");
        return;
    };
    let home = TempDir::new().unwrap();
    // Two copies of the vitest fixture; both will fail. esbuild must be reachable from
    // each project for its build to succeed.
    let (_h1, proj_a) = fixture_project("dev-test-vitest");
    let (_h2, proj_b) = fixture_project("dev-test-vitest");
    assert!(
        vendor_esbuild_into(&node, &src, &proj_a),
        "vendor esbuild A"
    );
    assert!(
        vendor_esbuild_into(&node, &src, &proj_b),
        "vendor esbuild B"
    );

    // Run BOTH as path operands; the first fails, --bail stops after it (B never builds).
    let out = rackabel_cmd(home.path(), home.path())
        .env("ABLETON_EH_NODE", &node)
        .env("RK_TEST_OUTCOME", "fail")
        .args(["dev", "test", "--bail", "--json"])
        .arg(&proj_a)
        .arg(&proj_b)
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let targets = v["targets"].as_array().unwrap();
    assert_eq!(
        targets.len(),
        1,
        "--bail should stop after the first failing target"
    );
    assert_eq!(targets[0]["exit_code"], 1);
    assert_eq!(v["failed"], true);
}

// --- local helpers --------------------------------------------------------------

/// Find a node that can resolve esbuild from some root on this machine, returning the
/// node + the source root esbuild is reachable from (to vendor into the temp project).
fn first_node_with_esbuild() -> Option<(PathBuf, PathBuf)> {
    for root in esbuild_source_roots() {
        if let Some(node) = usable_node_with_esbuild(&root) {
            return Some((node, root));
        }
    }
    None
}

/// Candidate source roots from which esbuild might be resolvable on this machine.
fn esbuild_source_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    for a in here.ancestors() {
        roots.push(a.to_path_buf());
    }
    if let Some(home) = home::home_dir() {
        roots.push(home);
    }
    roots
}
