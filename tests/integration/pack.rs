//! `rackabel pack` (Extension) integration tests.
//!
//! Tiers, per the pack assignment:
//!   (a) include-guard errors + dry-run planning run hermetically (they happen before
//!       any build — no node needed);
//!   (b) a real end-to-end `--no-official-cli` pack is gated behind a usable
//!       node + esbuild and self-skips otherwise (the production build runs first).
//!
//! The own-packer's output naming + archive membership are exercised directly against
//! fixture trees in `services::packer`'s unit tests; here we verify the wired-up CLI
//! behavior (filenames on disk, the .ablx is a real zip with the expected members).

use crate::common::*;
use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::io::Read;
use tempfile::TempDir;

/// An absolute `--include` is a validation error (exit 4), emitted as a three-part
/// frame BEFORE any build runs — no node needed.
#[test]
fn include_absolute_is_validation_error() {
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("pack-fixture");

    rackabel_cmd(home.path(), &proj)
        .args(["pack", "-i", "/etc/hosts"])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("must be relative"))
        .stderr(predicate::str::contains("RK4004"))
        .stderr(predicate::str::contains("help:"));
}

/// An escaping `--include` (`../x`) is a validation error (exit 4).
#[test]
fn include_escape_is_validation_error() {
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("pack-fixture");

    rackabel_cmd(home.path(), &proj)
        .args(["pack", "-i", "../outside.txt"])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("inside the extension directory"))
        .stderr(predicate::str::contains("RK4004"));
}

/// A missing `--include` is a validation error (exit 4) that names the path.
#[test]
fn include_missing_is_validation_error() {
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("pack-fixture");

    rackabel_cmd(home.path(), &proj)
        .args(["pack", "-i", "does-not-exist.png"])
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("not found"))
        .stderr(predicate::str::contains("--include does-not-exist.png"));
}

/// `pack --dry-run` plans the official packer for a pure-JS extension and reports the
/// official `<name>-<version>.ablx` filename, mutating nothing. No node needed.
#[test]
fn dry_run_pure_js_plans_official_filename() {
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("pack-fixture");

    rackabel_cmd(home.path(), &proj)
        .args(["pack", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("planned pack steps"))
        .stdout(predicate::str::contains("official extensions-cli"))
        // pack-fixture is "Clip Renamer" 0.1.0.
        .stdout(predicate::str::contains("Clip-Renamer-0.1.0.ablx"));

    assert!(!proj.join("Clip-Renamer-0.1.0.ablx").exists());
}

/// `pack --no-official-cli --dry-run` plans rackabel's own packer. No node needed.
#[test]
fn dry_run_no_official_cli_plans_own_packer() {
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("pack-fixture");

    rackabel_cmd(home.path(), &proj)
        .args(["pack", "--no-official-cli", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rackabel (--no-official-cli)"));
}

/// `pack --dry-run --json` is machine-readable and mutates nothing.
#[test]
fn dry_run_json() {
    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("pack-fixture");

    rackabel_cmd(home.path(), &proj)
        .args(["pack", "--dry-run", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"dry_run\": true"))
        .stdout(predicate::str::contains("\"packer\": \"official-cli\""));
}

/// The official-CLI shell-out path: rackabel locates the vendored
/// `@ableton-extensions/cli`, builds, then drives `node <cli.mjs> package <dir> -o
/// <out> -i <inc>`. We stub `cli.mjs` (the real one needs `archiver` installed) so the
/// test is hermetic, and assert rackabel passed the right args + surfaced the official
/// `<name>-<version>.ablx` filename. Gated behind a usable node + esbuild.
#[test]
fn official_cli_shell_out() {
    let probe_roots = esbuild_source_roots();
    let (node, source_root) = match probe_roots
        .iter()
        .find_map(|r| usable_node_with_esbuild(r).map(|n| (n, r.clone())))
    {
        Some(pair) => pair,
        None => {
            eprintln!("skipping official_cli_shell_out: no node+esbuild found");
            return;
        }
    };

    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("pack-fixture");
    assert!(vendor_esbuild_into(&node, &source_root, &proj));

    // Stub the official CLI at the layout rackabel's `locate()` checks first. It
    // emulates `package <dir> -o <out> [-i …]`: writes the chosen output and prints it
    // (the official CLI's only success stdout is the output path, SPEC A §1.4).
    let cli_dist = proj.join("node_modules/@ableton-extensions/cli/dist");
    std::fs::create_dir_all(&cli_dist).unwrap();
    let stub = r#"
const fs = require("node:fs");
const args = process.argv.slice(2);
// args: ["package", "<dir>", "-o", "<out>", "-i", "<inc>"...]
const oi = args.indexOf("-o");
const out = args[oi + 1];
const includes = [];
for (let i = 0; i < args.length; i++) if (args[i] === "-i") includes.push(args[i + 1]);
fs.writeFileSync(out, "PK-STUB " + includes.join(","));
process.stdout.write(out + "\n");
"#;
    std::fs::write(cli_dist.join("cli.mjs"), stub).unwrap();

    std::fs::write(proj.join("icon.png"), b"icon").unwrap();

    rackabel_cmd(home.path(), &proj)
        .env("ABLETON_EH_NODE", &node)
        .args(["pack", "-i", "icon.png"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Clip-Renamer-0.1.0.ablx"))
        .stdout(predicate::str::contains("Live → Settings → Extensions"));

    // The stub wrote the chosen output at the official filename in the extension dir.
    let out = proj.join("Clip-Renamer-0.1.0.ablx");
    assert!(
        out.is_file(),
        "official .ablx not produced at expected path"
    );
    let body = std::fs::read_to_string(&out).unwrap();
    // rackabel forwarded the validated include to the official CLI.
    assert!(body.contains("icon.png"), "include not forwarded: {body}");
}

/// REAL end-to-end `--no-official-cli` pack: builds (release) then packs with
/// rackabel's own packer. Verifies the `.ablx` lands in `releases/` with the
/// `<slug>-v<version>-<os>-<arch>.ablx` name and is a real zip carrying manifest.json
/// + dist/extension.js. Self-skips without node+esbuild.
#[test]
fn end_to_end_own_packer() {
    let probe_roots = esbuild_source_roots();
    let (node, source_root) = match probe_roots
        .iter()
        .find_map(|r| usable_node_with_esbuild(r).map(|n| (n, r.clone())))
    {
        Some(pair) => pair,
        None => {
            eprintln!("skipping end_to_end_own_packer: no node+esbuild found");
            return;
        }
    };

    let home = TempDir::new().unwrap();
    let (_hold, proj) = fixture_project("pack-fixture");
    assert!(
        vendor_esbuild_into(&node, &source_root, &proj),
        "could not vendor esbuild into the temp project"
    );

    rackabel_cmd(home.path(), &proj)
        .env("ABLETON_EH_NODE", &node)
        .args(["pack", "--no-official-cli"])
        .assert()
        .success()
        .stdout(predicate::str::contains("packed"))
        .stdout(predicate::str::contains("Live → Settings → Extensions"));

    // The slug is the temp project dir basename ("ext-fixture"); version 0.1.0; host
    // target resolved at runtime — so locate the single .ablx in releases/.
    let releases = proj.join("releases");
    assert!(releases.is_dir(), "releases/ not created");
    let ablx: Vec<_> = std::fs::read_dir(&releases)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "ablx").unwrap_or(false))
        .collect();
    assert_eq!(ablx.len(), 1, "expected exactly one .ablx in releases/");
    let name = ablx[0].file_name().unwrap().to_string_lossy();
    assert!(
        name.starts_with("pack-fixture-v0.1.0-"),
        "unexpected .ablx name: {name}"
    );

    // It is a real zip with the expected members.
    let file = std::fs::File::open(&ablx[0]).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let members: Vec<String> = (0..zip.len())
        .map(|i| zip.by_index(i).unwrap().name().to_string())
        .collect();
    assert!(members.contains(&"manifest.json".to_string()));
    assert!(members.contains(&"dist/extension.js".to_string()));

    // manifest.json inside the archive matches the generated one.
    let mut m = zip.by_name("manifest.json").unwrap();
    let mut s = String::new();
    m.read_to_string(&mut s).unwrap();
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["name"], "Clip Renamer");
    assert_eq!(v["entry"], "dist/extension.js");
}

/// Candidate source roots from which esbuild might be resolvable on this machine.
fn esbuild_source_roots() -> Vec<std::path::PathBuf> {
    let mut roots = Vec::new();
    let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf();
    for a in here.ancestors() {
        roots.push(a.to_path_buf());
    }
    if let Some(home) = home::home_dir() {
        roots.push(home);
    }
    roots
}
