//! `--json` envelope audit (DESIGN §7).
//!
//! Section 7 lists the commands that must support `--json` for scripting. The contract
//! finalized in 0.5:
//!   - **Success** prints a single stable-keyed JSON object/array on stdout.
//!   - **Failure** prints a single JSON object on stdout — never a human three-part
//!     frame on stderr with an empty stdout. A *setup/environment* failure (no manifest,
//!     bad toml, …) is rendered by `main` as the error envelope
//!     `{ok:false, code, exit, problem, location, help}`; a *domain-shaped* failure
//!     whose own envelope already encodes the outcome (validate's checklist, `dev test`'s
//!     targets, `dev reload`'s result) keeps that single envelope and is NOT double-printed.
//!
//! These tests are hermetic (no node/Live/network): they exercise the envelope SHAPE
//! and the round-trip, not a full build. Every assertion parses stdout as JSON so a
//! stray human line on stdout fails the test loudly.

use crate::common::*;
use serde_json::Value;
use tempfile::TempDir;

/// Run a command and parse its stdout as a single JSON value, asserting the exit code.
fn run_json(home: &std::path::Path, cwd: &std::path::Path, args: &[&str], code: i32) -> Value {
    let mut cmd = rackabel_cmd(home, cwd);
    cmd.env("ABLETON_APP", "/nonexistent-ableton-live.app");
    cmd.args(args);
    let out = cmd.output().expect("spawn");
    assert_eq!(
        out.status.code(),
        Some(code),
        "exit code mismatch for {args:?}; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    serde_json::from_str(&stdout).unwrap_or_else(|e| {
        panic!("stdout is not valid JSON for {args:?}: {e}\n--- stdout ---\n{stdout}")
    })
}

const COMPLETE: &str = "[extension]\nname=\"Cool\"\nauthor=\"Jane\"\nversion=\"1.2.0\"\nentry=\"src/extension.ts\"\nminimum_api_version=\"1.0.0\"\n";

/// A SETUP failure under `--json` (no manifest) emits the error envelope as a single
/// JSON object on stdout — code/exit/problem/help present — not a human frame on stderr.
#[test]
fn setup_failure_emits_error_envelope_on_stdout() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let v = run_json(home.path(), work.path(), &["--json", "validate"], 3);
    assert_eq!(v["ok"], Value::Bool(false));
    assert_eq!(v["code"], "RK0001");
    assert_eq!(v["exit"], 3);
    assert!(v["problem"].is_string(), "problem must be a string");
    assert!(v["help"].is_string(), "help must be a string");
    // `location` is present (null or string) so a consumer can rely on the key existing.
    assert!(v.get("location").is_some(), "location key must be present");
}

/// The error envelope is the SAME shape regardless of which command produced it: a
/// build with no manifest yields the identical key set, exit 3, RK0001.
#[test]
fn error_envelope_shape_is_uniform_across_commands() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let v = run_json(home.path(), work.path(), &["--json", "build"], 3);
    for key in ["ok", "code", "exit", "problem", "location", "help"] {
        assert!(
            v.get(key).is_some(),
            "missing key `{key}` in error envelope"
        );
    }
    assert_eq!(v["code"], "RK0001");
}

/// validate SUCCESS under `--json`: a single object with the stable keys
/// (`ok`/`failed`/`warnings`/`strict`/`checks[]`), `ok:true`, exit 0.
#[test]
fn validate_success_json_keys() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    std::fs::write(work.path().join("rackabel.toml"), COMPLETE).unwrap();
    std::fs::write(work.path().join("CHANGELOG.md"), "## 1.2.0\n").unwrap();
    let v = run_json(home.path(), work.path(), &["--json", "validate"], 0);
    assert_eq!(v["ok"], Value::Bool(true));
    assert_eq!(v["failed"], 0);
    assert!(v["checks"].is_array(), "checks must be an array");
    let checks = v["checks"].as_array().unwrap();
    assert!(!checks.is_empty());
    for c in checks {
        for key in ["id", "status", "message"] {
            assert!(c.get(key).is_some(), "check missing `{key}`");
        }
    }
}

/// validate FAILURE under `--json` keeps its OWN checklist envelope (the domain object
/// carries `ok:false` + the failing check) and is NOT followed by a second error
/// object — stdout parses as exactly one JSON value, exit 4.
#[test]
fn validate_failure_keeps_single_domain_envelope() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    std::fs::write(work.path().join("rackabel.toml"), COMPLETE).unwrap();
    // No CHANGELOG → one failure.
    let v = run_json(home.path(), work.path(), &["--json", "validate"], 4);
    // It is the checklist envelope (has `checks`), NOT the generic error envelope
    // (which would have `problem`/`help` and no `checks`).
    assert_eq!(v["ok"], Value::Bool(false));
    assert_eq!(v["failed"], 1);
    assert!(v["checks"].is_array());
    assert!(
        v.get("checks").is_some() && v.get("problem").is_none(),
        "validate failure must be the single checklist envelope, not the error envelope"
    );
}

/// doctor SUCCESS-or-fail under `--json` is always a single parseable object on stdout
/// (doctor renders its diagnosis as JSON and exit-codes directly — no double frame).
/// Outside a project with no Live, doctor reports the no-Live failure (exit 3) as JSON.
#[test]
fn doctor_json_is_single_object() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    // No Live → doctor exits 3, but its JSON diagnosis is the single stdout object.
    let mut cmd = rackabel_cmd(home.path(), work.path());
    cmd.env("ABLETON_APP", "/nonexistent-ableton-live.app");
    cmd.args(["--json", "doctor"]);
    let out = cmd.output().expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("doctor --json stdout not JSON: {e}\n{stdout}"));
    assert!(
        v.is_object() || v.is_array(),
        "doctor --json must be a JSON value"
    );
}

/// `plugin which --json` for an unknown plugin prints ONLY its resolution object
/// (`resolution: not_found`) and NOT a second error envelope, even though it exits
/// non-zero (RK0401) — the domain object is `json_handled`. stdout parses as one value.
#[test]
fn plugin_which_not_found_keeps_single_resolution_object() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let v = run_json(
        home.path(),
        work.path(),
        &["--json", "plugin", "which", "definitely-not-installed"],
        3,
    );
    assert_eq!(v["resolution"], "not_found");
    assert_eq!(v["name"], "definitely-not-installed");
    // It is the resolution object, NOT the generic error envelope.
    assert!(
        v.get("resolution").is_some() && v.get("problem").is_none(),
        "plugin which failure must be the single resolution object"
    );
}

/// `validate --json` produces NO ANSI/human checklist glyphs on stdout (the JSON object
/// is the only thing on stdout — regression guard against a mixed frame).
#[test]
fn json_stdout_has_no_human_glyphs() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    std::fs::write(work.path().join("rackabel.toml"), COMPLETE).unwrap();
    let mut cmd = rackabel_cmd(home.path(), work.path());
    cmd.env("ABLETON_APP", "/nonexistent-ableton-live.app");
    cmd.args(["--json", "validate"]);
    let out = cmd.output().expect("spawn");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("[✗]"), "no human glyphs on JSON stdout");
    assert!(!stdout.contains("[✓]"), "no human glyphs on JSON stdout");
    // And it still parses cleanly.
    let _: Value = serde_json::from_str(&stdout).expect("clean JSON");
}
