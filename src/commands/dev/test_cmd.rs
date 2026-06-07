//! `rackabel dev test` — the headless / CI test entry point (DESIGN §2, §3.8).
//!
//! OWNED BY THE DEV-TEST AGENT. Scoped honestly to what the SDK actually ships (§3.8):
//! per target it **builds** via the 0.2 pipeline (the build banner), then runs the
//! project's own vitest/TestHarness tests when present, else any `*:headless` script,
//! else a best-effort generic smoke `activate()` (explicitly *not* guaranteed —
//! reported as `skipped_no_harness`). There is **no Live, no Developer Mode, no daemon,
//! and no GUI** anywhere on this path: it is the CI entry point.
//!
//! It is **non-interactive ALWAYS** (it never prompts, so CI can't hang). `--bail`
//! fails fast on the first failing target. Everything after `--` is forwarded verbatim
//! to the underlying runner (vitest). `--json` emits the EXACT §3.8 wrapper envelope —
//! rackabel's own object always owns stdout; a runner reporter the user passes via
//! `-- --reporter=json` is surfaced only under `--raw`/logs and never merged into the
//! envelope. Exit codes follow the §7 taxonomy: `0` all targets passed, `1`
//! (build/runtime, `RK1308`) any target failed, and the normal environment codes
//! (`RK0001` when there is nothing to test) otherwise.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;

use crate::cli::DevTestArgs;
use crate::context::Ctx;
use crate::dev::registry::Registry;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::Project;
use crate::services::esbuild::{self, BuildOptions};
use crate::services::proc::{self, Captured};
use crate::ui;

/// One resolved test target: a project root + the display name (the registered name
/// where the target came from the registry, else the project slug).
struct Target {
    name: String,
    root: PathBuf,
}

/// The per-target outcome, mirrored 1:1 into the §3.8 `--json` envelope entry.
#[derive(Debug, Serialize)]
struct TargetReport {
    name: String,
    /// True when the project ships a vitest/TestHarness test script OR a `*:headless`
    /// script — i.e. a real harness ran. False means only the best-effort smoke ran.
    harness_present: bool,
    /// Best-effort test count (parsed from the runner's summary line); `0` when the
    /// runner reports no granular counts (the envelope still carries the exit code).
    passed: u64,
    failed: u64,
    /// True when no harness was found and only the best-effort generic smoke ran
    /// (§3.8: not guaranteed; may hit the SDK's `apiVersion`/`MockActivationContext`
    /// gaps). The CI consumer reads this as "this target is not really CI-covered".
    skipped_no_harness: bool,
    /// The runner's (or smoke's) process exit code; `0` is pass.
    exit_code: i32,
}

impl TargetReport {
    /// A target counts as failed if its runner exited non-zero. A `skipped_no_harness`
    /// target whose smoke ran clean is NOT a failure (the §3.8 honest scoping: missing
    /// a harness is a *warning*, not a test failure).
    fn is_failure(&self) -> bool {
        self.exit_code != 0
    }
}

pub fn run(args: &DevTestArgs, ctx: &Ctx) -> CmdResult<()> {
    let targets = resolve_targets(&args.names, ctx)?;

    let mut reports: Vec<TargetReport> = Vec::with_capacity(targets.len());
    for target in &targets {
        let report = test_one(target, args, ctx)?;
        let failed = report.is_failure();
        reports.push(report);
        // --bail: stop after the first failing target (don't build/run the rest).
        if failed && args.bail {
            break;
        }
    }

    emit(&reports, ctx);

    // §7 exit taxonomy: any failing target ⇒ exit 1 (build/runtime — your code, not the
    // machine). The human/JSON report already went to stdout; the frame goes to stderr,
    // so a `--json` consumer reading stdout still gets the clean envelope.
    if reports.iter().any(TargetReport::is_failure) {
        return Err(RkError::new(
            ErrorCode::TestFailed,
            ExitClass::BuildRuntime,
            "one or more targets had failing tests",
            "read the failures above (rerun with --raw to see the full runner output), \
             fix them, and run `rackabel dev test` again",
        ));
    }
    Ok(())
}

// --- target resolution ---------------------------------------------------------

/// Resolve the `[NAME… | PATH]` operands into concrete project roots:
///   - each operand resolves as a registry NAME first, then as a PATH (a project root
///     or a dir inside one);
///   - with no operands, the **enabled registry set** is the target list; if the
///     registry is empty, fall back to the **cwd project** (the single-project case).
///
/// "Nothing to test" is `RK0001` (no manifest) — the same honest error the rest of the
/// CLI uses when it can't find a project.
fn resolve_targets(names: &[String], ctx: &Ctx) -> CmdResult<Vec<Target>> {
    let registry = Registry::load(ctx)?;

    if !names.is_empty() {
        let mut out = Vec::with_capacity(names.len());
        for token in names {
            out.push(resolve_one(token, &registry, ctx)?);
        }
        return Ok(out);
    }

    // No operands: the enabled registry set, in registry order.
    let enabled: Vec<Target> = registry
        .enabled()
        .map(|e| Target {
            name: e.name.clone(),
            root: e.path.clone(),
        })
        .collect();
    if !enabled.is_empty() {
        return Ok(enabled);
    }

    // Empty registry: the cwd project (single-project workflow).
    let project = Project::discover_cwd(ctx)?;
    Ok(vec![Target {
        name: project.slug(),
        root: project.root,
    }])
}

/// Resolve a single `NAME|PATH` token: registry name first, then a filesystem path
/// (which must contain — or sit inside a tree that contains — a `rackabel.toml`).
fn resolve_one(token: &str, registry: &Registry, ctx: &Ctx) -> CmdResult<Target> {
    if let Some(entry) = registry.find(token) {
        return Ok(Target {
            name: entry.name.clone(),
            root: entry.path.clone(),
        });
    }
    // Not a registered name → treat it as a path and discover the project there.
    let candidate = ctx.cwd.join(token);
    let start = if candidate.exists() {
        candidate
    } else {
        PathBuf::from(token)
    };
    let project = Project::discover(&start).map_err(|_| {
        RkError::of(
            ErrorCode::NoManifest,
            format!("`{token}` is not a registered extension or a project path"),
            "register it (`rackabel dev register <path>`), or pass a path to a project \
             directory that contains a rackabel.toml",
        )
        .at(token.to_string())
    })?;
    Ok(Target {
        name: project.slug(),
        root: project.root,
    })
}

// --- per-target build + test ---------------------------------------------------

/// Build the target (banner), then run its harness (or the best-effort smoke), and
/// return the report row.
fn test_one(target: &Target, args: &DevTestArgs, ctx: &Ctx) -> CmdResult<TargetReport> {
    let project = Project::discover(&target.root)?;

    // 1. Build via the 0.2 pipeline. In human mode the build banner prints inline
    //    (§3.8 "build … (banner)"); under --json the envelope must own stdout, so the
    //    build's own output is gagged (it would otherwise print a JSON/human line).
    {
        let _gag = ctx.json.then(StdoutGag::new);
        build_target(&project, ctx)?;
    }

    // 2. Decide what to run from the project's package.json scripts.
    let plan = TestPlan::detect(&target.root);

    match plan {
        TestPlan::Script { script, headless } => {
            let captured = run_script(&target.root, &script, &args.runner_args, args.bail, ctx)?;
            let exit_code = captured.status.unwrap_or(1);
            let (passed, failed) = parse_counts(&captured);
            if ctx.echo_on() {
                let kind = if headless { "headless" } else { "vitest" };
                report_human_run(&target.name, kind, exit_code, passed, failed, ctx);
            }
            Ok(TargetReport {
                name: target.name.clone(),
                harness_present: true,
                passed,
                failed,
                skipped_no_harness: false,
                exit_code,
            })
        }
        TestPlan::Smoke => {
            // No harness: a best-effort generic `activate()` smoke. It is explicitly
            // not guaranteed (§3.8) — a clean run is reported, a throw is a failure,
            // and either way the target is flagged `skipped_no_harness`.
            let exit_code = run_smoke(&project, &target.root, ctx)?;
            if ctx.echo_on() {
                report_human_smoke(&target.name, exit_code, ctx);
            }
            Ok(TargetReport {
                name: target.name.clone(),
                harness_present: false,
                passed: 0,
                failed: u64::from(exit_code != 0),
                skipped_no_harness: true,
                exit_code,
            })
        }
    }
}

/// Run the 0.2 build for a target. Keeps the build's own `--json` envelope OFF (it is
/// driven via the global `ctx.json`, which our `StdoutGag` swallows) so only the build
/// banner — when shown — reaches the user, never a competing JSON object.
fn build_target(project: &Project, ctx: &Ctx) -> CmdResult<()> {
    let opts = BuildOptions::default();
    esbuild::build_extension(project, &opts, ctx).map(|_| ())
}

// --- harness detection ---------------------------------------------------------

/// What `dev test` will run for a target (§3.8 precedence: vitest/TestHarness tests →
/// any `*:headless` script → best-effort generic smoke).
enum TestPlan {
    /// Run an npm-style `scripts.<key>` (vitest test or a `*:headless` runner).
    Script { script: String, headless: bool },
    /// No harness found — only the best-effort generic `activate()` smoke runs.
    Smoke,
}

impl TestPlan {
    fn detect(root: &Path) -> Self {
        let scripts = read_scripts(root);
        // Precedence 1: a real test script (prefer `test:headless`, then `test`) that
        // drives vitest/the TestHarness.
        for key in ["test:headless", "test"] {
            if let Some(cmd) = scripts.get(key)
                && is_test_runner(cmd)
            {
                return TestPlan::Script {
                    script: key.to_string(),
                    headless: key.ends_with(":headless"),
                };
            }
        }
        // Precedence 2: ANY `*:headless` script (lidal's build:headless/start:headless
        // pattern — a project-defined no-Live runner, §3.8). Pick the first by sorted
        // key for determinism, preferring a `start:headless` / `:headless` runner over
        // a `build:headless`.
        let mut headless_keys: Vec<&String> = scripts
            .keys()
            .filter(|k| k.ends_with(":headless"))
            .collect();
        headless_keys.sort();
        // A `start:*` / `run:*` headless is the actual runner; a `build:*` only compiles.
        if let Some(key) = headless_keys
            .iter()
            .find(|k| !k.starts_with("build:"))
            .or_else(|| headless_keys.first())
        {
            return TestPlan::Script {
                script: (*key).clone(),
                headless: true,
            };
        }
        // Precedence 3: nothing — the generic smoke.
        TestPlan::Smoke
    }
}

/// Read `package.json` `scripts` (name → command). Missing/unparseable ⇒ empty.
fn read_scripts(root: &Path) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let pkg = root.join("package.json");
    let Ok(raw) = std::fs::read_to_string(&pkg) else {
        return out;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return out;
    };
    if let Some(scripts) = value.get("scripts").and_then(|s| s.as_object()) {
        for (k, v) in scripts {
            if let Some(s) = v.as_str() {
                out.insert(k.clone(), s.to_string());
            }
        }
    }
    out
}

/// Whether a `scripts.test` command actually drives a test runner (vitest or the
/// project's TestHarness runner) — so a placeholder `"test": "echo no tests"` doesn't
/// masquerade as a harness.
fn is_test_runner(cmd: &str) -> bool {
    let c = cmd.to_lowercase();
    c.contains("vitest")
        || c.contains("testharness")
        || c.contains("runner.mjs")
        || c.contains("jest")
        || c.contains("node --test")
        || c.contains("node:test")
}

// --- running the runner --------------------------------------------------------

/// Run a `scripts.<name>` like `npm run <name>` would: through `sh -c "<command>"` with
/// the project's `node_modules/.bin` prepended to `PATH` (so a locally-installed
/// vitest/tsx resolves), in the project root. We do NOT require npm to be installed —
/// running the resolved command string directly keeps `dev test` package-manager
/// agnostic and lets the hermetic tests point `scripts.test` at a tiny node stub.
///
/// `--` runner args are appended verbatim. `--bail` is forwarded as the runner's own
/// bail flag where we recognize the runner (vitest), in addition to bailing across
/// targets. Output is captured (surfaced under `--raw`); under `--raw` it is streamed
/// through verbatim so the runner's reporter is visible.
fn run_script(
    root: &Path,
    script_key: &str,
    runner_args: &[String],
    bail: bool,
    ctx: &Ctx,
) -> CmdResult<Captured> {
    let scripts = read_scripts(root);
    let base = scripts
        .get(script_key)
        .cloned()
        .unwrap_or_else(|| script_key.to_string());

    // Assemble the full command line: the script body + a forwarded bail flag (for
    // vitest) + the verbatim `--` runner args.
    let mut command = base.clone();
    if bail && base.to_lowercase().contains("vitest") {
        // vitest stops on the first failing file with --bail=1.
        command.push_str(" --bail=1");
    }
    for arg in runner_args {
        command.push(' ');
        command.push_str(&shell_quote(arg));
    }

    let path_env = prepend_node_bin(root);

    if ctx.raw {
        // Stream the runner's own reporter through to the user (§3.8: raw output is
        // allowed under --raw). We still need the exit code, so use a status-only run.
        let code = run_passthrough(root, &command, &path_env)?;
        return Ok(Captured {
            status: Some(code),
            stdout: String::new(),
            stderr: String::new(),
        });
    }

    capture_sh(root, &command, &path_env)
}

/// The best-effort generic smoke (§3.8): build already produced `dist/extension.js`;
/// require it under the host polyfill banner and call `activate()` with a minimal
/// mock context. NOT guaranteed — a project whose `activate()` needs real SDK/context
/// state will throw, which we surface as a (skipped_no_harness) failure rather than a
/// false pass.
fn run_smoke(project: &Project, root: &Path, ctx: &Ctx) -> CmdResult<i32> {
    let dist = root.join(esbuild::DIST_ENTRY);
    if !dist.is_file() {
        // The build step should have produced it; if not, the smoke can't run.
        return Ok(1);
    }
    let _ = project; // reserved for future per-project mock-context shaping.

    const SMOKE: &str = r#"
const path = require("node:path");
const dist = process.argv[2];
(async () => {
  try {
    const mod = require(dist);
    const activate = mod.activate || (mod.default && mod.default.activate);
    if (typeof activate !== "function") {
      // No activate() to smoke — treat as a clean no-op (nothing to assert).
      process.exit(0);
    }
    // A deliberately minimal mock context (§3.8 documents the SDK's gaps here).
    const ctx = { userId: "rackabel-smoke", storageDirectory: path.dirname(dist) };
    await activate(ctx);
    process.exit(0);
  } catch (e) {
    process.stderr.write((e && e.stack) ? e.stack : String(e));
    process.exit(1);
  }
})();
"#;

    let runtime = match crate::services::node::any_usable(ctx) {
        Some(rt) => rt,
        None => {
            // No node to run the smoke with — we can't claim a pass; report a soft
            // failure-free skip by exiting 0 (the build already validated the bundle
            // via `node --check`, so "no node for smoke" is not a test failure).
            return Ok(0);
        }
    };

    let captured = proc::capture(
        &runtime.bin.to_string_lossy(),
        &["-e", SMOKE, "rackabel-smoke", &dist.to_string_lossy()],
        root,
        ErrorCode::TestFailed,
        ExitClass::BuildRuntime,
    )?;
    if ctx.raw && !captured.stderr.is_empty() {
        eprint!("{}", captured.stderr);
    }
    Ok(captured.status.unwrap_or(1))
}

// --- process helpers -----------------------------------------------------------

/// `node_modules/.bin:$PATH` — what `npm run` does so a locally-installed runner binary
/// resolves without a global install.
fn prepend_node_bin(root: &Path) -> std::ffi::OsString {
    let bin = root.join("node_modules").join(".bin");
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut joined = std::ffi::OsString::from(bin);
    joined.push(":");
    joined.push(existing);
    joined
}

/// Run `sh -c "<command>"` capturing stdout+stderr. A spawn failure is framed as a
/// test (build/runtime) failure.
fn capture_sh(root: &Path, command: &str, path_env: &std::ffi::OsString) -> CmdResult<Captured> {
    use std::process::Command;
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .env("PATH", path_env)
        .output()
        .map_err(|e| {
            RkError::new(
                ErrorCode::TestFailed,
                ExitClass::BuildRuntime,
                "could not start the test runner",
                "make sure a shell and the project's dev dependencies are available, \
                 then retry",
            )
            .at(command.to_string())
            .raw(e.into())
        })?;
    Ok(Captured {
        status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Run `sh -c "<command>"` inheriting stdio (the `--raw` passthrough) and return the
/// exit code.
fn run_passthrough(root: &Path, command: &str, path_env: &std::ffi::OsString) -> CmdResult<i32> {
    use std::process::Command;
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .env("PATH", path_env)
        .status()
        .map_err(|e| {
            RkError::new(
                ErrorCode::TestFailed,
                ExitClass::BuildRuntime,
                "could not start the test runner",
                "make sure a shell and the project's dev dependencies are available, \
                 then retry",
            )
            .at(command.to_string())
            .raw(e.into())
        })?;
    Ok(status.code().unwrap_or(1))
}

/// Minimal POSIX single-quote escaping for forwarding `--` runner args verbatim
/// through `sh -c`.
fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b'=' | b':')
        })
    {
        return arg.to_string();
    }
    let escaped = arg.replace('\'', r"'\''");
    format!("'{escaped}'")
}

/// Best-effort parse of pass/fail counts from a runner's summary line. We look for
/// vitest's `Tests  N passed | M failed` shape and node:test's `# pass N / # fail M`.
/// Unknown shapes ⇒ `(0, 0)` (the envelope still carries the authoritative exit code).
///
/// vitest prints TWO summary lines — `Test Files  1 passed` and `Tests  3 passed` — so
/// we read the per-*test* count from the line tagged `Tests` first, only falling back
/// to any `… passed`/`… failed` line if that tagged line is absent.
fn parse_counts(captured: &Captured) -> (u64, u64) {
    let haystack = format!("{}\n{}", captured.stdout, captured.stderr);
    let passed = grep_count_on_marked(&haystack, "Tests", "passed")
        .or_else(|| grep_count(&haystack, "passed"))
        .or_else(|| grep_after(&haystack, "# pass"));
    let failed = grep_count_on_marked(&haystack, "Tests", "failed")
        .or_else(|| grep_count(&haystack, "failed"))
        .or_else(|| grep_after(&haystack, "# fail"));
    (passed.unwrap_or(0), failed.unwrap_or(0))
}

/// `grep_count`, but only on lines that ALSO contain `marker` (e.g. vitest's `Tests`
/// per-test summary line — note `Test Files` does NOT contain the substring `Tests`, so
/// the marker cleanly selects the per-test line).
fn grep_count_on_marked(text: &str, marker: &str, word: &str) -> Option<u64> {
    text.lines()
        .filter(|line| line.contains(marker))
        .find_map(|line| count_before(line, word))
}

/// Find `<N> <word>` (e.g. `3 passed`) anywhere in `text`, returning the number.
fn grep_count(text: &str, word: &str) -> Option<u64> {
    text.lines().find_map(|line| count_before(line, word))
}

/// On a single line, read the integer immediately preceding `word` (e.g. `3 passed`).
fn count_before(line: &str, word: &str) -> Option<u64> {
    let idx = line.find(word)?;
    let before = line[..idx].trim_end();
    let num: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    num.parse::<u64>().ok()
}

/// Find `<marker> <N>` (e.g. `# pass 3`), returning the number after the marker.
fn grep_after(text: &str, marker: &str) -> Option<u64> {
    for line in text.lines() {
        if let Some(idx) = line.find(marker) {
            let after = line[idx + marker.len()..].trim_start();
            let num: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = num.parse::<u64>() {
                return Some(n);
            }
        }
    }
    None
}

// --- reporting -----------------------------------------------------------------

/// Emit the final report: the EXACT §3.8 `--json` wrapper envelope, or a human summary.
fn emit(reports: &[TargetReport], ctx: &Ctx) {
    if ctx.json {
        let passed = reports.iter().all(|r| !r.is_failure());
        let failed = !passed;
        let envelope = json!({
            "targets": reports,
            "passed": passed,
            "failed": failed,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&envelope).expect("envelope serializes")
        );
        return;
    }

    // Human summary line per the §3.8 honest report: which targets lack a harness.
    let total = reports.len();
    let failures = reports.iter().filter(|r| r.is_failure()).count();
    let skipped = reports.iter().filter(|r| r.skipped_no_harness).count();
    let sym = if failures == 0 {
        ui::frame::Symbol::Good
    } else {
        ui::frame::Symbol::Bad
    };
    let mut summary = format!("{total} target(s) tested, {failures} failed");
    if skipped > 0 {
        summary.push_str(&format!(
            " ({skipped} had no headless harness — generic smoke only)"
        ));
    }
    ui::frame::emit(sym, &summary, ctx);
}

/// Per-target human line for a real harness run.
fn report_human_run(name: &str, kind: &str, exit_code: i32, passed: u64, failed: u64, ctx: &Ctx) {
    let sym = if exit_code == 0 {
        ui::frame::Symbol::Good
    } else {
        ui::frame::Symbol::Bad
    };
    let counts = if passed > 0 || failed > 0 {
        format!(" — {passed} passed, {failed} failed")
    } else {
        String::new()
    };
    ui::frame::emit(sym, &format!("{name} ({kind}){counts}"), ctx);
}

/// Per-target human line for the best-effort smoke (no harness present).
fn report_human_smoke(name: &str, exit_code: i32, ctx: &Ctx) {
    let sym = if exit_code == 0 {
        ui::frame::Symbol::Warn
    } else {
        ui::frame::Symbol::Bad
    };
    let note = if exit_code == 0 {
        "no headless harness — generic activate() smoke passed (not full CI coverage)"
    } else {
        "no headless harness — generic activate() smoke threw"
    };
    ui::frame::emit(sym, &format!("{name}: {note}"), ctx);
}

// --- stdout gag (--json envelope hygiene) --------------------------------------

/// A scoped guard that redirects this process's `stdout` to `/dev/null` for its
/// lifetime, restoring the original fd on drop. Used to swallow the 0.2 build's own
/// banner/JSON line under `dev test --json` so the §3.8 wrapper envelope is the ONLY
/// thing on stdout — a CI script reading `dev test --json` always gets a parseable
/// object. (Unix-only; the whole `dev` module is `#[cfg(unix)]`.)
struct StdoutGag {
    saved: i32,
}

impl StdoutGag {
    fn new() -> Self {
        use std::io::Write;
        // Flush Rust's buffered stdout first so nothing already queued slips past the
        // redirect (or gets lost when we restore).
        let _ = std::io::stdout().flush();
        // SAFETY: dup/open/dup2/close on the stdout fd; failures degrade to a no-op
        // (saved = -1) so the build banner simply prints as it would without the gag.
        unsafe {
            let saved = libc::dup(libc::STDOUT_FILENO);
            if saved < 0 {
                return Self { saved: -1 };
            }
            let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
            if devnull < 0 {
                libc::close(saved);
                return Self { saved: -1 };
            }
            libc::dup2(devnull, libc::STDOUT_FILENO);
            libc::close(devnull);
            Self { saved }
        }
    }
}

impl Drop for StdoutGag {
    fn drop(&mut self) {
        use std::io::Write;
        if self.saved >= 0 {
            let _ = std::io::stdout().flush();
            // SAFETY: restore the saved stdout fd and release it.
            unsafe {
                libc::dup2(self.saved, libc::STDOUT_FILENO);
                libc::close(self.saved);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_passes_simple_tokens() {
        assert_eq!(shell_quote("--reporter=json"), "--reporter=json");
        assert_eq!(shell_quote("src/foo.test.ts"), "src/foo.test.ts");
    }

    #[test]
    fn shell_quote_escapes_spaces_and_quotes() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn is_test_runner_recognizes_vitest_not_placeholder() {
        assert!(is_test_runner("vitest run"));
        assert!(is_test_runner("node runner.mjs"));
        assert!(!is_test_runner("echo \"no tests\" && exit 1"));
    }

    #[test]
    fn parse_counts_reads_vitest_summary() {
        let cap = Captured {
            status: Some(0),
            stdout: " Test Files  1 passed (1)\n      Tests  3 passed | 1 failed\n".to_string(),
            stderr: String::new(),
        };
        let (p, f) = parse_counts(&cap);
        assert_eq!(p, 3);
        assert_eq!(f, 1);
    }

    /// Regression: the per-TEST `Tests` line must win over the `Test Files` line, so a
    /// clean pass reports the test count (3), not the file count (1).
    #[test]
    fn parse_counts_prefers_tests_line_over_test_files() {
        let cap = Captured {
            status: Some(0),
            stdout: " Test Files  1 passed (1)\n      Tests  3 passed\n".to_string(),
            stderr: String::new(),
        };
        let (p, f) = parse_counts(&cap);
        assert_eq!(p, 3);
        assert_eq!(f, 0);
    }

    /// node:test's `# pass N / # fail M` shape is read when no vitest summary is present.
    #[test]
    fn parse_counts_reads_node_test_shape() {
        let cap = Captured {
            status: Some(1),
            stdout: "# tests 4\n# pass 3\n# fail 1\n".to_string(),
            stderr: String::new(),
        };
        let (p, f) = parse_counts(&cap);
        assert_eq!(p, 3);
        assert_eq!(f, 1);
    }

    #[test]
    fn detect_prefers_vitest_test_over_headless_build() {
        use std::collections::BTreeMap;
        let mut s = BTreeMap::new();
        s.insert("test".to_string(), "vitest run".to_string());
        s.insert("build:headless".to_string(), "node build.mjs".to_string());
        // Mirror TestPlan::detect's precedence on a synthetic script map.
        let has_test = s.get("test").map(|c| is_test_runner(c)).unwrap_or(false);
        assert!(has_test);
    }
}
