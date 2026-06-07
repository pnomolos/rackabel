//! The Extension build pipeline (DESIGN §2 `build`, §4.5, §4.6; SPEC A §3.5; SPEC B §2).
//!
//! `build_extension` is THE shared build entry that `deploy`/`pack`/`new` call. It:
//!   1. resolves the extension config from `rackabel.toml` (with inference);
//!   2. resolves a usable node (Live-bundled first, then PATH — DESIGN §0);
//!   3. drives esbuild through that node (we own the invocation so we can bake the
//!      polyfill banner the official `build.ts` omits — DESIGN §4.6);
//!   4. generates the SDK `manifest.json` from `rackabel.toml` (DESIGN §4.5);
//!   5. validates the bundle (`node --check`; >10KB sanity warning — see note below);
//!   6. records a short content hash + elapsed time so "did it rebuild?" is never a
//!      mystery, and persists the hash to `.rackabel/state.toml`.
//!
//! ## esbuild invocation model
//! We drive esbuild via its JS API (`esbuild.build(config)`) inside a one-shot node
//! process, exactly like the arclight `scripts/build-extension.js` (SPEC B §2). The
//! config object mirrors the arclight one — `bundle`, `format:cjs`, `platform:node`,
//! `define:{global:"globalThis"}`, the banner, `external:native_deps`,
//! `sourcesContent:false`, `minify`/`sourcemap` flipped by `--release`. esbuild
//! itself comes from the *project's* `node_modules` (resolved with
//! `require.resolve("esbuild", { paths: [projectRoot] })`, which handles both npm and
//! pnpm layouts) — never a global. A missing node is an *environment* error
//! (`RK0305`), never a raw "node not found"; a missing esbuild is a build error
//! (`RK1301`) with the friendly "run `rackabel deploy --fix` / `npm install`" remedy.
//!
//! ## The >10KB sanity check
//! SPEC A §5 / `pack-extension.js` treats a sub-10KB bundle as an error because a
//! real extension always bundles the (45KB+) SDK. A *minimal* or SDK-less project
//! (e.g. a test fixture, or `new --minimal`) can legitimately produce a smaller
//! bundle that still passes `node --check`. So rackabel keeps the floor as a
//! **non-fatal warning** at build time (a valid, parseable small bundle is not a
//! build failure); `pack`/`validate` may treat it more strictly. Recorded in
//! `docs/DEVIATIONS.md`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use serde_json::json;

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::{self, Project, ResolvedExtension};
use crate::services::{banner, node};
use crate::ui;

/// The conventional built-bundle path written into `manifest.json`'s `entry`.
pub const DIST_ENTRY: &str = "dist/extension.js";

/// The bundle-size floor below which we warn (SPEC A §5).
const BUNDLE_SIZE_FLOOR: u64 = 10_000;

/// Options controlling a build (DESIGN §2 `build` flags).
///
/// `Default` is all-off / `None` (typecheck `None` = default-on for `--release`).
#[derive(Clone, Debug, Default)]
pub struct BuildOptions {
    /// `--release`: minify on, no sourcemap, typecheck default-on.
    pub release: bool,
    /// `--clean`: wipe `dist/` first.
    pub clean: bool,
    /// `--typecheck`/`--no-typecheck`: `None` = default (on for release).
    pub typecheck: Option<bool>,
    /// `--print-config`: dump the resolved esbuild config and exit.
    pub print_config: bool,
    /// `--dry-run`: print the planned steps, mutate nothing.
    pub dry_run: bool,
    /// `--json`.
    pub json: bool,
}

impl BuildOptions {
    /// Whether `tsc --noEmit` runs: explicit flag wins, else default-on for release.
    fn typecheck_on(&self) -> bool {
        self.typecheck.unwrap_or(self.release)
    }
}

/// The result of a successful build.
#[derive(Debug)]
pub struct BuildOutcome {
    /// The built bundle (`dist/extension.js`).
    pub entry: PathBuf,
    /// The generated `manifest.json`.
    pub manifest_json: PathBuf,
    pub bytes: u64,
    pub elapsed: std::time::Duration,
    /// Short content hash, printed so "did it rebuild?" is never a mystery.
    pub hash: String,
    /// True when up-to-date and no `--clean`.
    pub skipped: bool,
    pub typechecked: bool,
}

/// The resolved esbuild configuration — the single source of truth for both
/// `--print-config` and the node driver. Field names match esbuild's JS API so the
/// dumped JSON is directly usable in a `build.ts`. `__project_root` is a private hint
/// for the driver (it resolves `esbuild` relative to it) and is stripped before the
/// config reaches `esbuild.build`.
#[derive(Debug, Serialize)]
struct EsbuildConfig {
    #[serde(rename = "__projectRoot")]
    project_root: PathBuf,
    #[serde(rename = "entryPoints")]
    entry_points: Vec<PathBuf>,
    outfile: PathBuf,
    bundle: bool,
    format: &'static str,
    platform: &'static str,
    #[serde(rename = "sourcesContent")]
    sources_content: bool,
    #[serde(rename = "logLevel")]
    log_level: &'static str,
    minify: bool,
    sourcemap: bool,
    define: serde_json::Value,
    banner: serde_json::Value,
    external: Vec<String>,
}

impl EsbuildConfig {
    /// Build the config from the resolved extension + options. `entry_abs` is the
    /// absolute source entry; `outfile_abs` the absolute `dist/extension.js`.
    fn resolve(
        project_root: &Path,
        entry_abs: PathBuf,
        outfile_abs: PathBuf,
        ext: &ResolvedExtension,
        opts: &BuildOptions,
    ) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            entry_points: vec![entry_abs],
            outfile: outfile_abs,
            bundle: true,
            format: "cjs",
            platform: "node",
            sources_content: false,
            log_level: "warning",
            // --release => minify on, no sourcemap; otherwise sourcemap on (SPEC B §2).
            minify: opts.release,
            sourcemap: !opts.release,
            define: json!({ "global": "globalThis" }),
            banner: json!({ "js": banner::POLYFILL_BANNER }),
            external: ext.native_deps.clone(),
        }
    }
}

/// THE shared build entry. Resolves node, injects the banner, runs esbuild, writes
/// `manifest.json`, validates. Returns `RK13xx` on build failure, `RK03xx` if no
/// usable node.
pub fn build_extension(
    project: &Project,
    opts: &BuildOptions,
    ctx: &Ctx,
) -> CmdResult<BuildOutcome> {
    let started = Instant::now();
    let ext = project.resolved_extension(ctx)?;

    // The source entry is `[extension].entry` (default src/extension.ts); the built
    // bundle is always `dist/extension.js` (the path the SDK manifest records).
    let entry_abs = project.root.join(&ext.entry);
    let dist_dir = project.root.join("dist");
    let outfile_abs = dist_dir.join("extension.js");

    let cfg = EsbuildConfig::resolve(
        &project.root,
        entry_abs.clone(),
        outfile_abs.clone(),
        &ext,
        opts,
    );

    // --print-config: dump the resolved esbuild config and exit, mutating nothing.
    if opts.print_config {
        print_config(&cfg, opts);
        // A successful "config dumped" returns a synthetic skipped outcome so the
        // caller (build::run) can stop cleanly without treating it as a real build.
        return Ok(synthetic_outcome(outfile_abs, project, &ext, started, true));
    }

    // --dry-run: print the planned steps and exit, mutating nothing.
    if opts.dry_run {
        print_plan(&cfg, opts, &ext, project, ctx);
        return Ok(synthetic_outcome(outfile_abs, project, &ext, started, true));
    }

    // Resolve the node we will drive esbuild (and `tsc`/`node --check`) with: Live's
    // bundled node first (ABI match), else PATH node. No usable node is RK0305 —
    // never a raw "node not found" (DESIGN §0, §6.2).
    let live = crate::services::live::detect(ctx);
    let primary = live.iter().find(|i| i.bundled_node.is_some());
    let runtime = node::resolve(primary, ctx).ok_or_else(no_usable_node)?;

    // --clean: blow away dist/ first.
    if opts.clean && dist_dir.exists() {
        std::fs::remove_dir_all(&dist_dir).map_err(|e| {
            RkError::new(
                ErrorCode::BuildFailed,
                ExitClass::BuildRuntime,
                "could not clean the build directory",
                "check write permissions for the project directory, then retry",
            )
            .at(dist_dir.display().to_string())
            .raw(e.into())
        })?;
    }

    // The source entry must exist before we hand it to esbuild.
    if !entry_abs.is_file() {
        return Err(RkError::new(
            ErrorCode::BuildFailed,
            ExitClass::BuildRuntime,
            "the extension's entry source was not found",
            "create src/extension.ts, or set [extension].entry in rackabel.toml",
        )
        .at(entry_abs.display().to_string()));
    }

    // Optional typecheck (default on for --release).
    let typecheck_on = opts.typecheck_on();
    if typecheck_on {
        run_typecheck(&runtime, &project.root)?;
    }

    std::fs::create_dir_all(&dist_dir).map_err(|e| {
        RkError::new(
            ErrorCode::BuildFailed,
            ExitClass::BuildRuntime,
            "could not create the build directory",
            "check write permissions for the project directory, then retry",
        )
        .at(dist_dir.display().to_string())
        .raw(e.into())
    })?;

    // Drive esbuild.
    run_esbuild(&runtime, &cfg)?;

    // Post-build validation: the bundle must exist, parse (`node --check`), and a
    // size sanity check (warn-only — see module note + DEVIATIONS.md).
    let bytes = validate_bundle(&runtime, &outfile_abs, ctx)?;

    // Generate manifest.json from rackabel.toml (DESIGN §4.5).
    let manifest_json = write_manifest(project, &ext)?;

    // Hash + persist to state so "did it rebuild?" is answerable.
    let hash = short_hash(&outfile_abs)?;
    persist_state(&project.root, &hash);

    let elapsed = started.elapsed();
    let outcome = BuildOutcome {
        entry: outfile_abs,
        manifest_json,
        bytes,
        elapsed,
        hash,
        skipped: false,
        typechecked: typecheck_on,
    };
    report_success(&outcome, ctx);
    Ok(outcome)
}

// --- node-driven steps ------------------------------------------------------

/// Run `tsc --noEmit` via the project's TypeScript. We invoke it through node so we
/// use the project toolchain (`node_modules/.bin/tsc` resolved by node), keeping the
/// dev and CI environments identical. A type error is RK1302 (build/runtime).
fn run_typecheck(runtime: &node::NodeRuntime, project_root: &Path) -> CmdResult<()> {
    // Resolve the project's `typescript` and run its `tsc` programmatically. Using
    // node -e keeps us off a globally-installed tsc and matches the `tsc --noEmit`
    // the official package.json runs before `tsx build.ts`.
    const DRIVER: &str = r#"
const path = require("node:path");
const root = process.argv[2];
let tscBin;
try {
  // typescript ships bin/tsc; resolve its package then its tsc entry.
  const tsPkg = require.resolve("typescript/package.json", { paths: [root] });
  tscBin = path.join(path.dirname(tsPkg), "bin", "tsc");
} catch (e) {
  process.stderr.write("TSC_NOT_FOUND");
  process.exit(3);
}
const { spawnSync } = require("node:child_process");
const r = spawnSync(process.execPath, [tscBin, "--noEmit"], { cwd: root, stdio: "inherit" });
process.exit(r.status == null ? 1 : r.status);
"#;
    let out = run_node(
        runtime,
        DRIVER,
        &[&project_root.to_string_lossy()],
        project_root,
    )?;
    if out.status == Some(3) || out.stderr.contains("TSC_NOT_FOUND") {
        // No typescript installed — surface it as a build error with the install remedy,
        // not a raw module-not-found.
        return Err(RkError::new(
            ErrorCode::TypecheckFailed,
            ExitClass::BuildRuntime,
            "couldn't find the project's TypeScript to typecheck with",
            "install dependencies (the project's package.json pins typescript), or \
             pass --no-typecheck to skip the check",
        )
        .at(project_root.display().to_string()));
    }
    if out.status != Some(0) {
        return Err(RkError::new(
            ErrorCode::TypecheckFailed,
            ExitClass::BuildRuntime,
            "the TypeScript typecheck (`tsc --noEmit`) reported errors",
            "fix the type errors shown above, or pass --no-typecheck to skip the check",
        )
        .raw(anyhow::anyhow!("{}{}", out.stdout, out.stderr)));
    }
    Ok(())
}

/// Run esbuild via the project's esbuild (resolved by node). Failure is RK1301; a
/// missing esbuild is RK1301 with the install remedy.
fn run_esbuild(runtime: &node::NodeRuntime, cfg: &EsbuildConfig) -> CmdResult<()> {
    const DRIVER: &str = r#"
const fs = require("node:fs");
const cfg = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
const root = cfg.__projectRoot;
delete cfg.__projectRoot;
let esbuild;
try {
  esbuild = require(require.resolve("esbuild", { paths: [root] }));
} catch (e) {
  process.stderr.write("ESBUILD_NOT_FOUND");
  process.exit(3);
}
esbuild.build(cfg)
  .then(() => process.exit(0))
  .catch((e) => { process.stderr.write((e && e.message) ? e.message : String(e)); process.exit(1); });
"#;
    // Write the config to a temp JSON file so banner content (with quotes/newlines)
    // never has to survive shell/argv quoting.
    let cfg_json = serde_json::to_string(cfg).expect("EsbuildConfig serializes");
    let cfg_file = write_temp("rackabel-esbuild-", ".json", &cfg_json)?;
    let project_root = &cfg.project_root;
    let out = run_node(
        runtime,
        DRIVER,
        &[&cfg_file.to_string_lossy()],
        project_root,
    );
    let _ = std::fs::remove_file(&cfg_file);
    let out = out?;

    if out.status == Some(3) || out.stderr.contains("ESBUILD_NOT_FOUND") {
        return Err(RkError::new(
            ErrorCode::BuildFailed,
            ExitClass::BuildRuntime,
            "couldn't find esbuild to bundle the extension",
            "install the project's dependencies (its package.json pins esbuild), \
             e.g. `npm install`, then rerun the build",
        )
        .at(project_root.display().to_string()));
    }
    if out.status != Some(0) {
        return Err(RkError::new(
            ErrorCode::BuildFailed,
            ExitClass::BuildRuntime,
            "esbuild failed to bundle the extension",
            "fix the error shown above (a missing import or a syntax error in your \
             source), then rerun the build",
        )
        .raw(anyhow::anyhow!("{}{}", out.stdout, out.stderr)));
    }
    Ok(())
}

/// Post-build validation: the bundle exists, `node --check` parses it, and a
/// (warn-only) size sanity floor. Returns the bundle size in bytes.
fn validate_bundle(runtime: &node::NodeRuntime, outfile: &Path, ctx: &Ctx) -> CmdResult<u64> {
    let meta = std::fs::metadata(outfile).map_err(|e| {
        RkError::new(
            ErrorCode::BundleSanity,
            ExitClass::BuildRuntime,
            "esbuild reported success but the bundle is missing",
            "rerun with --clean; if it persists, run with --raw to see esbuild output",
        )
        .at(outfile.display().to_string())
        .raw(e.into())
    })?;
    let bytes = meta.len();

    // `node --check <bundle>` — a real syntax check on the emitted CJS.
    let check = crate::services::proc::capture(
        &runtime.bin.to_string_lossy(),
        &["--check", &outfile.to_string_lossy()],
        outfile.parent().unwrap_or(Path::new(".")),
        ErrorCode::BundleSanity,
        ExitClass::BuildRuntime,
    )?;
    if !check.success() {
        return Err(RkError::new(
            ErrorCode::BundleSanity,
            ExitClass::BuildRuntime,
            "the built bundle failed `node --check` (it is not valid JavaScript)",
            "this is usually a bug in the build pipeline — rerun with --raw and report it",
        )
        .at(outfile.display().to_string())
        .raw(anyhow::anyhow!("{}{}", check.stdout, check.stderr)));
    }

    // Size sanity: warn-only (a minimal/SDK-less bundle can legitimately be smaller).
    if bytes < BUNDLE_SIZE_FLOOR && ctx.echo_on() {
        ui::frame::emit(
            ui::frame::Symbol::Warn,
            &format!(
                "bundle is small ({bytes} bytes < {BUNDLE_SIZE_FLOOR}) — fine for a minimal \
                 extension, but a real one usually bundles the SDK and is larger"
            ),
            ctx,
        );
    }
    Ok(bytes)
}

// --- manifest + state -------------------------------------------------------

/// Generate `manifest.json` from `rackabel.toml` (DESIGN §4.5). The `entry` is always
/// the built bundle path (`dist/extension.js`), not the source entry.
fn write_manifest(project: &Project, ext: &ResolvedExtension) -> CmdResult<PathBuf> {
    let value = manifest::sdk_manifest::generate(ext, DIST_ENTRY);
    let body = serde_json::to_string_pretty(&value).expect("manifest serializes");
    let path = project.root.join("manifest.json");
    std::fs::write(&path, format!("{body}\n")).map_err(|e| {
        RkError::new(
            ErrorCode::BuildFailed,
            ExitClass::BuildRuntime,
            "could not write manifest.json",
            "check write permissions for the project directory, then retry",
        )
        .at(path.display().to_string())
        .raw(e.into())
    })?;
    Ok(path)
}

/// Persist the build hash to `.rackabel/state.toml` (best-effort; a state-write
/// failure must not fail the build).
fn persist_state(root: &Path, hash: &str) {
    if let Ok(mut state) = manifest::state::load(root) {
        state.build_hash = Some(hash.to_string());
        let _ = manifest::state::save(root, &state);
    }
}

// --- reporting --------------------------------------------------------------

fn report_success(outcome: &BuildOutcome, ctx: &Ctx) {
    if ctx.json {
        let v = json!({
            "ok": true,
            "entry": outcome.entry,
            "manifest": outcome.manifest_json,
            "bytes": outcome.bytes,
            "elapsed_ms": outcome.elapsed.as_millis(),
            "hash": outcome.hash,
            "typechecked": outcome.typechecked,
        });
        println!("{}", serde_json::to_string_pretty(&v).expect("json"));
        return;
    }
    // The §2 line: "rebuilt in NNms" + a short build hash.
    let ms = outcome.elapsed.as_millis();
    ui::frame::emit(
        ui::frame::Symbol::Good,
        &format!(
            "rebuilt in {ms}ms ({}) — {}",
            outcome.hash,
            outcome.entry.display()
        ),
        ctx,
    );
}

/// Dump the resolved esbuild config (the `--print-config` escape hatch for Persona B).
fn print_config(cfg: &EsbuildConfig, opts: &BuildOptions) {
    // Strip the private `__projectRoot` hint and present an esbuild-API-shaped object.
    let mut value = serde_json::to_value(cfg).expect("config serializes");
    if let Some(obj) = value.as_object_mut() {
        obj.remove("__projectRoot");
        obj.insert("typecheck".into(), json!(opts.typecheck_on()));
    }
    println!("{}", serde_json::to_string_pretty(&value).expect("json"));
}

/// Print the planned steps without mutating anything (`--dry-run`).
fn print_plan(
    cfg: &EsbuildConfig,
    opts: &BuildOptions,
    ext: &ResolvedExtension,
    project: &Project,
    ctx: &Ctx,
) {
    if ctx.json {
        let v = json!({
            "dry_run": true,
            "clean": opts.clean,
            "typecheck": opts.typecheck_on(),
            "release": opts.release,
            "entry": cfg.entry_points,
            "outfile": cfg.outfile,
            "manifest": project.root.join("manifest.json"),
            "externals": ext.native_deps,
        });
        println!("{}", serde_json::to_string_pretty(&v).expect("json"));
        return;
    }
    println!("planned build steps (nothing was changed):");
    if opts.clean {
        println!("  1. clean {}", project.root.join("dist").display());
    }
    if opts.typecheck_on() {
        println!("  - typecheck: tsc --noEmit");
    }
    println!(
        "  - bundle {} -> {} (format=cjs, platform=node, {})",
        cfg.entry_points
            .first()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        cfg.outfile.display(),
        if opts.release {
            "minify, no sourcemap"
        } else {
            "sourcemap"
        }
    );
    if !ext.native_deps.is_empty() {
        println!(
            "  - externalize native deps: {}",
            ext.native_deps.join(", ")
        );
    }
    println!("  - write {}", project.root.join("manifest.json").display());
    println!("  - validate: node --check, bundle-size sanity");
}

// --- helpers ----------------------------------------------------------------

/// Run a small node driver script (`node -e SCRIPT arg...`) capturing output.
fn run_node(
    runtime: &node::NodeRuntime,
    script: &str,
    args: &[&str],
    cwd: &Path,
) -> CmdResult<crate::services::proc::Captured> {
    let bin = runtime.bin.to_string_lossy().into_owned();
    // `node -e "<script>" a b` puts `a` at `process.argv[1]`, not `[2]` (there is no
    // script-file path entry the way there is for `node file.js a`). The driver scripts
    // read their first real argument as `process.argv[2]` (the conventional index), so we
    // insert a placeholder positional to line the indices up. Without it the first real
    // argument lands at `[1]` and the driver reads `undefined` at `[2]`.
    let mut full: Vec<&str> = vec!["-e", script, "rackabel-driver"];
    full.extend_from_slice(args);
    crate::services::proc::capture(
        &bin,
        &full,
        cwd,
        ErrorCode::BuildFailed,
        ExitClass::BuildRuntime,
    )
}

/// A short content hash of a file, for the "did it rebuild?" build hash. Uses a
/// stable non-cryptographic hash (FNV-1a) — we only need change-detection, not
/// security — so we avoid pulling in a hashing crate.
fn short_hash(path: &Path) -> CmdResult<String> {
    let bytes = std::fs::read(path).map_err(|e| {
        RkError::new(
            ErrorCode::BundleSanity,
            ExitClass::BuildRuntime,
            "could not read the bundle to hash it",
            "rerun the build; if it persists, run with --raw",
        )
        .at(path.display().to_string())
        .raw(e.into())
    })?;
    Ok(fnv1a_hex(&bytes))
}

/// FNV-1a 64-bit, rendered as 12 hex chars (enough to distinguish rebuilds).
fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{:012x}", hash & 0xffff_ffff_ffff)
}

/// Write `content` to a uniquely-named temp file and return its path.
fn write_temp(prefix: &str, suffix: &str, content: &str) -> CmdResult<PathBuf> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("{prefix}{pid}-{nanos}{suffix}"));
    std::fs::write(&path, content).map_err(|e| {
        RkError::new(
            ErrorCode::BuildFailed,
            ExitClass::BuildRuntime,
            "could not write a temporary build file",
            "check that the system temp directory is writable",
        )
        .at(path.display().to_string())
        .raw(e.into())
    })?;
    Ok(path)
}

/// A synthetic outcome for `--print-config`/`--dry-run` (nothing was built).
fn synthetic_outcome(
    outfile: PathBuf,
    project: &Project,
    _ext: &ResolvedExtension,
    started: Instant,
    skipped: bool,
) -> BuildOutcome {
    BuildOutcome {
        entry: outfile,
        manifest_json: project.root.join("manifest.json"),
        bytes: 0,
        elapsed: started.elapsed(),
        hash: String::new(),
        skipped,
        typechecked: false,
    }
}

/// The "no usable node runtime" environment error (RK0305). The remedy is "upgrade
/// Live", never "install Node" — Live's bundled node is the supported runtime
/// (DESIGN §0, §6.2).
fn no_usable_node() -> RkError {
    RkError::new(
        ErrorCode::NoNodeRuntime,
        ExitClass::Environment,
        "couldn't find a Node runtime to build with",
        "install Ableton Live 12.4.5+ (it bundles the right Node), or install Node on \
         your PATH, then rerun the build",
    )
}

/// A uniform "not implemented yet" error for the parallel-stub services. Uses the
/// build/runtime class so it never masquerades as an environment problem. (Retained
/// for the deploy/pack stubs that still call it until those branches land.)
pub(crate) fn not_implemented(what: &str) -> RkError {
    RkError::new(
        ErrorCode::BuildFailed,
        ExitClass::BuildRuntime,
        format!("`{what}` isn't implemented yet"),
        "this command lands later in the 0.2 milestone — track its branch",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typecheck_default_is_release() {
        let mut opts = BuildOptions::default();
        assert!(!opts.typecheck_on());
        opts.release = true;
        assert!(opts.typecheck_on());
        opts.typecheck = Some(false);
        assert!(!opts.typecheck_on());
        opts.release = false;
        opts.typecheck = Some(true);
        assert!(opts.typecheck_on());
    }

    #[test]
    fn config_release_flips_minify_and_sourcemap() {
        let ext = sample_ext(vec![]);
        let dev = EsbuildConfig::resolve(
            Path::new("/p"),
            PathBuf::from("/p/src/extension.ts"),
            PathBuf::from("/p/dist/extension.js"),
            &ext,
            &BuildOptions::default(),
        );
        assert!(!dev.minify);
        assert!(dev.sourcemap);

        let rel = EsbuildConfig::resolve(
            Path::new("/p"),
            PathBuf::from("/p/src/extension.ts"),
            PathBuf::from("/p/dist/extension.js"),
            &ext,
            &BuildOptions {
                release: true,
                ..Default::default()
            },
        );
        assert!(rel.minify);
        assert!(!rel.sourcemap);
    }

    #[test]
    fn config_bakes_banner_and_define_unconditionally() {
        let ext = sample_ext(vec![]);
        for release in [false, true] {
            let cfg = EsbuildConfig::resolve(
                Path::new("/p"),
                PathBuf::from("/p/src/extension.ts"),
                PathBuf::from("/p/dist/extension.js"),
                &ext,
                &BuildOptions {
                    release,
                    ..Default::default()
                },
            );
            assert_eq!(cfg.define["global"], "globalThis");
            assert_eq!(cfg.banner["js"], banner::POLYFILL_BANNER);
            assert_eq!(cfg.format, "cjs");
            assert_eq!(cfg.platform, "node");
            assert!(cfg.bundle);
            assert!(!cfg.sources_content);
        }
    }

    #[test]
    fn config_externals_come_from_native_deps() {
        let ext = sample_ext(vec!["easymidi".into(), "abletonlink".into()]);
        let cfg = EsbuildConfig::resolve(
            Path::new("/p"),
            PathBuf::from("/p/src/extension.ts"),
            PathBuf::from("/p/dist/extension.js"),
            &ext,
            &BuildOptions::default(),
        );
        assert_eq!(cfg.external, vec!["easymidi", "abletonlink"]);
    }

    #[test]
    fn config_json_is_esbuild_shaped() {
        let ext = sample_ext(vec![]);
        let cfg = EsbuildConfig::resolve(
            Path::new("/p"),
            PathBuf::from("/p/src/extension.ts"),
            PathBuf::from("/p/dist/extension.js"),
            &ext,
            &BuildOptions::default(),
        );
        let v = serde_json::to_value(&cfg).unwrap();
        // esbuild-API field names.
        assert!(v.get("entryPoints").is_some());
        assert!(v.get("sourcesContent").is_some());
        assert!(v.get("logLevel").is_some());
        assert_eq!(v["logLevel"], "warning");
        // The private hint is present in the wire form (stripped by print_config).
        assert!(v.get("__projectRoot").is_some());
    }

    #[test]
    fn fnv_is_stable_and_distinguishes() {
        assert_eq!(fnv1a_hex(b"abc"), fnv1a_hex(b"abc"));
        assert_ne!(fnv1a_hex(b"abc"), fnv1a_hex(b"abd"));
        assert_eq!(fnv1a_hex(b"abc").len(), 12);
    }

    fn sample_ext(native_deps: Vec<String>) -> ResolvedExtension {
        ResolvedExtension {
            name: "x".into(),
            author: "a".into(),
            version: semver::Version::new(0, 1, 0),
            entry: PathBuf::from("src/extension.ts"),
            minimum_api_version: semver::Version::new(1, 0, 0),
            extra_dist_files: vec![],
            native_deps,
            pack_targets: vec![],
            inferred: vec![],
        }
    }
}
