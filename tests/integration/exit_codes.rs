//! Exit-code precedence matrix (DESIGN §7) — the ship-quality centerpiece.
//!
//! Section 7 fixes the taxonomy and its precedence:
//!   - `0` success; `1` build/runtime; `2` usage; `3` environment-not-ready;
//!     `4` validation.
//!   - **Precedence / cause attribution:** environment(3) > validation(4) >
//!     build/runtime(1). The environment subset runs first and short-circuits, so a
//!     command that auto-runs multiple gates returns the SINGLE highest-severity code
//!     — never a mix. Usage(2) is caught at parse time.
//!   - `--no-input` prompt classes: a usage/missing-answer prompt becomes exit 2; an
//!     *environment* prompt (an ambiguous Live/User-Library pick the machine can't
//!     resolve deterministically) becomes exit 3 — never a silent default-accept.
//!
//! This is one table-driven test plus a few targeted scenarios (the multi-gate
//! short-circuit and the `--no-input` classes) that don't fit a single-args row.
//! Every case is hermetic: temp `RACKABEL_HOME`/`HOME`, fake Live/User-Library seams,
//! no node, no network, no real Live.

use crate::common::*;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use tempfile::TempDir;

/// How a case prepares its working dir before the command runs.
enum Setup {
    /// Leave the working dir empty (no rackabel.toml).
    Empty,
    /// Write this `rackabel.toml` body into the working dir.
    Toml(&'static str),
    /// Write a `rackabel.toml` body AND a CHANGELOG.md AND a `.rackabel/state.toml`.
    Project {
        toml: &'static str,
        changelog: Option<&'static str>,
        state: Option<&'static str>,
    },
}

/// One row of the matrix: a name, a setup, the argv, the expected exit code, and a
/// stable substring that must appear in stderr (the framed RK code) proving the *cause*.
struct Row {
    name: &'static str,
    setup: Setup,
    args: &'static [&'static str],
    code: i32,
    stderr_has: &'static str,
}

const COMPLETE: &str = "[extension]\nname=\"Cool\"\nauthor=\"Jane\"\nversion=\"1.2.0\"\nentry=\"src/extension.ts\"\nminimum_api_version=\"1.0.0\"\n";

fn matrix() -> Vec<Row> {
    vec![
        // --- usage (exit 2), caught at parse time ---
        Row {
            name: "bad flag value is usage",
            setup: Setup::Toml(COMPLETE),
            // a non-existent global flag is a clap parse error (exit 2)
            args: &["build", "--nonsense-flag"],
            code: 2,
            stderr_has: "",
        },
        Row {
            name: "register --name with --recursive is usage (mutually exclusive)",
            setup: Setup::Empty,
            args: &["dev", "register", ".", "--name", "x", "--recursive"],
            code: 2,
            stderr_has: "",
        },
        // --- environment (exit 3) ---
        Row {
            name: "no manifest is environment (RK0001)",
            setup: Setup::Empty,
            args: &["build"],
            code: 3,
            stderr_has: "RK0001",
        },
        Row {
            name: "both kinds declared is environment (RK0002)",
            setup: Setup::Toml(
                "[extension]\n[device]\nname=\"d\"\nkind=\"audio-effect\"\nentry=\"x.maxpat\"\n",
            ),
            args: &["build"],
            code: 3,
            stderr_has: "RK0002",
        },
        Row {
            name: "malformed toml is environment parse error (RK0003)",
            setup: Setup::Toml("[extension]\nname = \"unterminated\n"),
            args: &["build"],
            code: 3,
            stderr_has: "RK0003",
        },
        Row {
            name: "validate outside a project is environment, not validation (RK0001)",
            setup: Setup::Empty,
            args: &["validate"],
            code: 3,
            stderr_has: "RK0001",
        },
        // --- validation (exit 4) ---
        Row {
            name: "missing changelog is validation (RK4001)",
            setup: Setup::Project {
                toml: COMPLETE,
                changelog: None,
                state: None,
            },
            args: &["validate"],
            code: 4,
            stderr_has: "RK4001",
        },
        Row {
            name: "stale version is validation (RK4003)",
            setup: Setup::Project {
                toml: COMPLETE,
                changelog: Some("## 1.2.0\n"),
                state: Some("last_packed_version = \"1.2.0\"\n"),
            },
            args: &["validate"],
            code: 4,
            stderr_has: "RK4003",
        },
        // --- build / runtime (exit 1) ---
        Row {
            name: "device build stub is build/runtime",
            setup: Setup::Project {
                toml: "[device]\nname=\"my-device\"\nkind=\"audio-effect\"\nentry=\"src/d.maxpat\"\n",
                changelog: None,
                state: None,
            },
            args: &["build"],
            code: 1,
            stderr_has: "isn't implemented yet",
        },
    ]
}

/// The table-driven exit-code matrix: each row exercises one taxonomy class and proves
/// both the exit code AND the framed cause (the RK code) match §7.
#[test]
fn exit_code_matrix() {
    for row in matrix() {
        let home = TempDir::new().unwrap();
        let work = TempDir::new().unwrap();
        let root = work.path();
        match &row.setup {
            Setup::Empty => {}
            Setup::Toml(body) => {
                std::fs::write(root.join("rackabel.toml"), body).unwrap();
            }
            Setup::Project {
                toml,
                changelog,
                state,
            } => {
                std::fs::write(root.join("rackabel.toml"), toml).unwrap();
                if let Some(c) = changelog {
                    std::fs::write(root.join("CHANGELOG.md"), c).unwrap();
                }
                if let Some(s) = state {
                    let rk = root.join(".rackabel");
                    std::fs::create_dir_all(&rk).unwrap();
                    std::fs::write(rk.join("state.toml"), s).unwrap();
                }
            }
        }
        // For the device-build-stub row, src/d.maxpat must exist + be valid JSON to
        // reach the assembly stub (a missing entry would short-circuit as a sanity
        // build error too, but we want the deterministic "not implemented" cause).
        if row.name.starts_with("device build stub") {
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(root.join("src/d.maxpat"), "{}").unwrap();
        }

        let mut cmd = rackabel_cmd(home.path(), root);
        // Keep the host-apiVersion rule off the real machine for validate rows.
        cmd.env("ABLETON_APP", "/nonexistent-ableton-live.app");
        cmd.args(row.args);
        let assert = cmd.assert().failure().code(row.code);
        if !row.stderr_has.is_empty() {
            assert.stderr(predicate::str::contains(row.stderr_has));
        }
    }
}

// ---------------------------------------------------------------------------
// The multi-gate short-circuit: environment(3) BEFORE validation(4).
// ---------------------------------------------------------------------------

/// `deploy --release` auto-runs BOTH the environment gate (User-Library resolution)
/// and `validate`. When BOTH would fail — no User Library to resolve AND a validation
/// failure (no CHANGELOG) — the environment check must short-circuit first and the
/// command must return `3`, never `4` (§7: "3 is returned before 4 is ever reached").
#[test]
fn deploy_release_environment_short_circuits_validation() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let proj = work.path().join("clip-renamer");
    std::fs::create_dir_all(&proj).unwrap();
    // Complete manifest but NO CHANGELOG.md → validate would fail (exit 4)…
    std::fs::write(proj.join("rackabel.toml"), COMPLETE).unwrap();

    // …and no resolvable User Library: `rackabel_cmd` pins HOME to a fresh temp dir and
    // clears ABLETON_USER_LIBRARY, so discovery finds no `~/Music/Ableton…/User Library`
    // → RK0302 (environment, exit 3). With BOTH gates failing, environment must win.
    rackabel_cmd(home.path(), &proj)
        .env("ABLETON_APP", "/nonexistent-ableton-live.app")
        .args(["deploy", "--release"])
        .assert()
        .failure()
        // Environment short-circuits validation: exit 3, not 4.
        .code(3)
        .stderr(predicate::str::contains("RK0302"));
}

/// A `deploy --release` whose ONLY failure is validation (the environment IS ready:
/// a fake Live + a real User Library) returns `4` — proving the precedence only
/// promotes to `3` when the environment actually fails, and otherwise validation wins.
#[test]
fn deploy_release_validation_only_is_exit_4() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let proj = work.path().join("clip-renamer");
    std::fs::create_dir_all(&proj).unwrap();
    // Complete manifest, NO CHANGELOG → validation failure.
    std::fs::write(proj.join("rackabel.toml"), COMPLETE).unwrap();

    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    let ul = FakeUserLibrary::new();

    rackabel_cmd(home.path(), &proj)
        .arg("deploy")
        .arg("--release")
        .arg("--live")
        .arg(live.app_path())
        .arg("--user-library")
        .arg(ul.path())
        .assert()
        .failure()
        // Environment is ready → validation is the single highest-severity cause → 4.
        .code(4)
        .stderr(predicate::str::contains("RK4001"));
}

// ---------------------------------------------------------------------------
// --no-input prompt classes (§7): usage(2) vs environment(3).
// ---------------------------------------------------------------------------

/// An unresolvable environment under `--no-input` is an ENVIRONMENT failure (exit 3),
/// never a silent default-accept (§7 `--no-input` semantics: "do not prompt, and do not
/// invent an answer"). Here deploy can't resolve a User Library and `--no-input` forbids
/// the interactive pick → exit 3, the environment class, not a usage(2).
#[test]
fn no_input_unresolvable_user_library_is_environment() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let proj = work.path().join("clip-renamer");
    std::fs::create_dir_all(&proj).unwrap();
    std::fs::write(proj.join("rackabel.toml"), COMPLETE).unwrap();
    std::fs::write(proj.join("CHANGELOG.md"), "## 1.2.0\n").unwrap();

    // No --user-library and a fresh temp HOME → discovery finds nothing → RK0302
    // (environment, exit 3). `--no-input` must surface this as an environment failure,
    // not a usage(2) and not a silent default.
    let live = FakeLive::new("12.4.5b3", FakeArch::Universal, FakeLayout::Helpers);
    rackabel_cmd(home.path(), &proj)
        .env("ABLETON_APP", live.app_path())
        .args(["deploy", "--no-input"])
        .assert()
        .failure()
        // Environment class (3), never a usage(2) and never a silent default.
        .code(3)
        .stderr(predicate::str::contains("RK0302"));
}

/// `new --template gh:…` under `--no-input` without `--yes` is the §5.7 consent gate:
/// a remote fetch needs an explicit decision the machine can't invent, so it refuses
/// as an ENVIRONMENT-class outcome (exit 3, RK0403) rather than silently fetching.
#[test]
fn no_input_remote_template_refuses_environment() {
    let home = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    rackabel_cmd(home.path(), work.path())
        .args([
            "new",
            "x",
            "--template",
            "gh:owner/repo",
            "--no-input",
            "--no-git",
        ])
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("RK0403"));
}
