//! The watch loop (DESIGN §3.3, §4.4).
//!
//! OWNED BY THE WATCH-LOOP AGENT. Implements the flagship dev loop: a `notify`-based
//! watcher over globs DERIVED from each extension's build config (entry + bundle-graph
//! inputs) EXTENDED to the `src` tree of every internal `workspace:*` library it depends
//! on (§4.4); a debounced (`[dev].debounce_ms`, default 200 ms) atomic chain
//! `build library? → build extension → deploy → reload`; working-set scoping; reload-ms
//! reporting + the one-time scope hint; `--no-auto-reload` manual mode; and the TTY
//! hotkeys (`[r]` reload, `[l]` logs, `[q]` quit).
//!
//! The host itself lives in the detached daemon (DESIGN §3.1); this loop is an ephemeral
//! foreground CLIENT of it ([`super::ipc::Client`]). The atomic ordering — build, then
//! deploy, then *and only then* a `reload` IPC — is owned here in code (§3.3): a reload
//! request can NEVER precede a successful deploy, which is the deploy-before-reload trap
//! the whole milestone exists to close. On a failed build we keep the last good deployed
//! artifact, print a framed error, and do NOT reload.

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::manifest::{Kind, Project};
use crate::services::esbuild;
use crate::ui;

use super::ipc::{Client, Request, Response};
use super::{Inspect, RegistryEntry};

/// The default debounce window (DESIGN §3.3). Overridable via `[dev].debounce_ms`.
pub const DEFAULT_DEBOUNCE_MS: u64 = 200;

/// The enabled-set size above which bare `dev` prints the one-time scope hint (§3.3).
const SCOPE_HINT_ENABLED_THRESHOLD: usize = 4;

/// The reload-p50 budget (ms) above which the scope hint fires (§3.3, default ~750 ms).
const SCOPE_HINT_P50_BUDGET_MS: u64 = 750;

/// The source extensions a `workspace:*` library / extension edit can carry. We watch
/// the source tree and rebuild on a change to any of these (the bundle graph terminates
/// at a library's compiled `dist/`, so a naive graph-only derivation would miss `src/`,
/// §4.4 — we watch the `src` of every workspace dep explicitly).
const SOURCE_EXTS: &[&str] = &["ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "css"];

/// One internal `workspace:*` library a watched extension depends on (§4.4): its root
/// (the `node_modules` symlink target), its npm name (for the `pnpm --filter` build), and
/// its `src` dir (the tree we watch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceLib {
    /// The npm package name (e.g. `@arclight/core`) — used to build it with the right filter.
    pub name: String,
    /// The library project root (the symlink target under `node_modules`).
    pub root: PathBuf,
    /// The library's source dir (`<root>/src`), the tree we watch.
    pub src: PathBuf,
}

/// The derived watch set: the roots to register with `notify` and a path predicate.
pub struct WatchPlan {
    /// The directories handed to the recursive watcher.
    pub roots: Vec<PathBuf>,
    /// The glob filter applied to every event path (source files only, never `dist/`
    /// or `node_modules/`).
    pub globs: globset::GlobSet,
    /// Per-entry: the extension's project root + the workspace libs it depends on. The
    /// chain uses this to attribute a changed path to an extension and to its libs.
    entries: Vec<EntryPlan>,
}

/// The watch facts for one working-set entry.
struct EntryPlan {
    /// The registry name (the addressable handle + reload `only` token).
    name: String,
    /// The extension project root.
    root: PathBuf,
    /// The kind stored on the registry entry at `register --type` time, if any. Carried
    /// into the chain so a manifestless registered project resolves without re-guessing
    /// (Phase 5): a registered, package.json-only extension/device never has to infer its
    /// kind at build/deploy time. `None` => fall back to manifest/synthesized resolution.
    kind: Option<Kind>,
    /// The internal `workspace:*` libraries this extension consumes.
    libs: Vec<WorkspaceLib>,
}

/// Options for the watch loop (bare `dev` / `dev watch`).
pub struct WatchOpts {
    /// Auto-reload on a debounced change (default). `false` = manual `[r]`/`dev reload`.
    pub auto_reload: bool,
    /// The debounce window in ms.
    pub debounce_ms: u64,
    /// Show unfiltered host output in the inline tail.
    pub raw: bool,
    /// The node inspector endpoint, if `--inspect` was passed.
    pub inspect: Option<Inspect>,
    /// Whether `--emit-launch-config` already wrote (and we should mention) launch.json.
    pub emit_launch_config: bool,
}

impl Default for WatchOpts {
    fn default() -> Self {
        Self {
            auto_reload: true,
            debounce_ms: DEFAULT_DEBOUNCE_MS,
            raw: false,
            inspect: None,
            emit_launch_config: false,
        }
    }
}

/// A debounced set of changed paths, classified into library edits (which need their
/// library rebuilt first) vs plain extension-source edits (§3.3 step 2). It carries the
/// project root for each affected extension so the chain (the free
/// [`build_deploy_reload`]) can build/deploy without re-deriving the plan.
#[derive(Debug, Default, Clone)]
pub struct ChangeSet {
    /// All changed paths in this debounce window (for the trigger summary).
    pub paths: Vec<PathBuf>,
    /// Library roots whose `src` changed → rebuild these libraries first, in walk order.
    libs: Vec<WorkspaceLib>,
    /// Extension `name → project root` whose own source OR a dependent library changed →
    /// rebuild + deploy + reload these (the fan-out of a library edit to all its
    /// dependents). A `BTreeMap` so the order is stable for deterministic output/tests.
    affected: BTreeMap<String, PathBuf>,
    /// Per-affected-extension registered kind (`register --type`), keyed by name. Plumbed
    /// from the registry entry so the chain discovers each affected project WITH its kind
    /// (`Project::discover_with_kind`) — a manifestless registered project never guesses.
    /// Absent / `None` => discover resolves the kind itself (manifest or synthesized default).
    kinds: BTreeMap<String, Option<Kind>>,
}

impl ChangeSet {
    /// The names of the extensions this change touches (the reload `only` scope).
    pub fn affected_names(&self) -> Vec<String> {
        self.affected.keys().cloned().collect()
    }

    /// Whether anything actionable changed.
    pub fn is_empty(&self) -> bool {
        self.affected.is_empty() && self.libs.is_empty()
    }
}

// --- plan derivation -----------------------------------------------------------

/// Derive watch roots+globs for a working set, expanding each extension's `workspace:*`
/// library sources (§4.4). Every extension contributes its own project root; each of its
/// internal `workspace:*` deps contributes its library `src` tree, so editing shared
/// library code rebuilds the library then its dependents.
pub fn plan(working_set: &[RegistryEntry], ctx: &Ctx) -> CmdResult<WatchPlan> {
    let mut entries = Vec::new();
    let mut roots: BTreeSet<PathBuf> = BTreeSet::new();

    for entry in working_set {
        let root = entry.path.clone();
        roots.insert(root.clone());
        let libs = workspace_libs(&root, ctx);
        for lib in &libs {
            // Watch the library's src tree (not its dist/, which esbuild bundles from).
            if lib.src.is_dir() {
                roots.insert(lib.src.clone());
            } else {
                roots.insert(lib.root.clone());
            }
        }
        entries.push(EntryPlan {
            name: entry.name.clone(),
            root,
            kind: entry.kind,
            libs,
        });
    }

    let globs = source_globset()?;
    Ok(WatchPlan {
        roots: roots.into_iter().collect(),
        globs,
        entries,
    })
}

/// The source-file glob set: the watched source extensions, anywhere, but never under
/// `dist/` or `node_modules/` (a dist write is the BUILD output, not an input — reloading
/// on it would loop; node_modules churn isn't ours).
fn source_globset() -> CmdResult<globset::GlobSet> {
    let mut builder = globset::GlobSetBuilder::new();
    for ext in SOURCE_EXTS {
        builder.add(glob(&format!("**/*.{ext}"))?);
    }
    builder.build().map_err(glob_build_err)
}

fn glob(pat: &str) -> CmdResult<globset::Glob> {
    globset::Glob::new(pat).map_err(|e| {
        RkError::of(
            ErrorCode::HostLaunchFailed,
            format!("could not compile the watch glob `{pat}`"),
            "this is a bug in rackabel's watch-glob derivation; please report it",
        )
        .raw(e.into())
    })
}

fn glob_build_err(e: globset::Error) -> RkError {
    RkError::of(
        ErrorCode::HostLaunchFailed,
        "could not build the watch glob set",
        "this is a bug in rackabel's watch-glob derivation; please report it",
    )
    .raw(e.into())
}

/// Whether a changed path is an input we should rebuild on: it matches a source glob and
/// is not under a `dist/` or `node_modules/` segment (the build output / dep churn), and
/// is not a rackabel-GENERATED artifact at a project root.
///
/// `manifest.json` is the trap here (finding #2): the build step writes it at the project
/// root from `rackabel.toml`, and it matches the `**/*.json` source glob and lives OUTSIDE
/// `dist/`. Treating it as an input means every chain run's own manifest write re-triggers
/// the chain — a self-write feedback loop that fires the whole build→deploy→reload twice
/// for one save. It is a build OUTPUT (rackabel.toml is the source of truth), so exclude it.
fn is_source_input(path: &Path, globs: &globset::GlobSet) -> bool {
    if path
        .components()
        .any(|c| matches!(c.as_os_str().to_str(), Some("dist") | Some("node_modules")))
    {
        return false;
    }
    // The generated manifest is an output, not an input (rackabel.toml drives it).
    if path.file_name().and_then(|s| s.to_str()) == Some("manifest.json") {
        return false;
    }
    globs.is_match(path)
}

/// Discover the internal `workspace:*` libraries an extension at `root` depends on, by
/// reading its `package.json` `dependencies`/`devDependencies` for `workspace:` specs and
/// resolving each through the `node_modules/<name>` symlink to the library root (§4.4).
/// A dep that doesn't resolve to a real on-disk dir (no symlink, e.g. deps not installed)
/// is skipped — we can only watch what we can find.
pub fn workspace_libs(root: &Path, ctx: &Ctx) -> Vec<WorkspaceLib> {
    let _ = ctx; // reserved for a future workspace-root reconciliation hook.
    let pkg_path = root.join("package.json");
    let Ok(text) = std::fs::read_to_string(&pkg_path) else {
        return Vec::new();
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };

    let mut libs = Vec::new();
    for table in ["dependencies", "devDependencies"] {
        let Some(deps) = json.get(table).and_then(|v| v.as_object()) else {
            continue;
        };
        for (name, spec) in deps {
            let Some(spec) = spec.as_str() else { continue };
            if !spec.starts_with("workspace:") {
                continue;
            }
            // Resolve node_modules/<name> (handles scoped `@scope/pkg`) to its target.
            let link = root.join("node_modules").join(name);
            let lib_root = std::fs::canonicalize(&link).unwrap_or(link);
            if !lib_root.is_dir() {
                continue;
            }
            let src = lib_root.join("src");
            let lib = WorkspaceLib {
                name: name.clone(),
                root: lib_root,
                src,
            };
            if !libs.contains(&lib) {
                libs.push(lib);
            }
        }
    }
    libs
}

// --- change classification -----------------------------------------------------

impl WatchPlan {
    /// Classify a batch of changed paths into a [`ChangeSet`]: a path under an
    /// extension's own root marks that extension affected; a path under a workspace
    /// library's tree marks the library for rebuild AND every dependent extension
    /// affected (the library edit fans out, §4.4).
    fn classify(&self, changed: &[PathBuf]) -> ChangeSet {
        let mut cs = ChangeSet {
            paths: changed.to_vec(),
            ..Default::default()
        };
        let mut lib_set: BTreeMap<PathBuf, WorkspaceLib> = BTreeMap::new();

        for path in changed {
            if !is_source_input(path, &self.globs) {
                continue;
            }
            for entry in &self.entries {
                // The extension's own source (under root/, excluding its libs which live
                // under node_modules and are filtered out above).
                if path_under(path, &entry.root) {
                    cs.affected.insert(entry.name.clone(), entry.root.clone());
                    cs.kinds.insert(entry.name.clone(), entry.kind);
                }
                // A dependent library's source.
                for lib in &entry.libs {
                    let in_lib = path_under(path, &lib.src) || path_under(path, &lib.root);
                    if in_lib {
                        cs.affected.insert(entry.name.clone(), entry.root.clone());
                        cs.kinds.insert(entry.name.clone(), entry.kind);
                        lib_set.insert(lib.root.clone(), lib.clone());
                    }
                }
            }
        }
        cs.libs = lib_set.into_values().collect();
        cs
    }
}

/// Whether `path` is `base` or lives beneath it, robust to symlinked path prefixes (e.g.
/// macOS `/tmp` → `/private/tmp`, firmlinked `/Users`). The OS file-watcher (FSEvents on
/// macOS) reports the canonical real path of a changed file, while the registry stores the
/// path the user typed — a plain `starts_with` then silently never matches and no reload
/// ever fires. Canonicalize both sides before comparing; fall back to the lexical
/// `starts_with` if either side can't be canonicalized (e.g. a deleted file).
fn path_under(path: &Path, base: &Path) -> bool {
    if path.starts_with(base) {
        return true;
    }
    match (std::fs::canonicalize(path), std::fs::canonicalize(base)) {
        (Ok(p), Ok(b)) => p.starts_with(&b),
        _ => false,
    }
}

// --- the atomic build → deploy → reload chain ----------------------------------

/// The atomic chain run on a debounced change (§3.3): rebuild each changed `workspace:*`
/// library FIRST (so its `dist/` is fresh), then the affected extensions' esbuild, then
/// deploy the fresh bundle to the User Library, and ONLY on a clean build issue the
/// `reload` IPC. On a failed build we keep the last good deploy, print the framed error,
/// and do NOT reload (never reload against a half-built bundle).
///
/// The ordering is the contract: a `reload` request is sent strictly after a successful
/// deploy. This is the trap-closing invariant the watcher test asserts.
pub fn build_deploy_reload(changed: &ChangeSet, client: &mut Client, ctx: &Ctx) -> CmdResult<()> {
    if changed.is_empty() {
        return Ok(());
    }

    // 1. Rebuild changed libraries first (deps before dependents, §4.4).
    for lib in &changed.libs {
        build_library(lib, ctx)?;
    }

    if changed.affected.is_empty() {
        return Ok(());
    }

    // 2+3. For each affected extension: build (esbuild) then deploy (the 0.2 copy set).
    let mut total_build = Duration::ZERO;
    let mut total_deploy = Duration::ZERO;
    let mut deployed: Vec<String> = Vec::new();

    // Build/deploy run against a project-rooted, quieted context so the chain owns the
    // single `reloaded …` line instead of build/deploy each emitting their own frames.
    for (name, root) in &changed.affected {
        // Discover WITH the registered kind (Phase 5): a manifestless registered project
        // carries its `register --type` kind so it never has to guess at build/deploy time.
        // When the entry has no stored kind (`None`) or a real manifest is present, this is
        // identical to a plain discover — `discover_with_kind(None)` sets no override and a
        // present manifest always wins.
        let kind = changed.kinds.get(name).copied().flatten();
        let project = Project::discover_with_kind(root, kind)?;
        let quiet = quiet_project_ctx(ctx, &project.root);

        // BUILD — esbuild::build_extension is the shared 0.2 service (reused, not
        // reimplemented). A build failure here is the gate: the `?` returns WITHOUT a
        // reload, so the last good deployed artifact stays loaded (§3.3 step 5). We skip
        // the esbuild exec when the bundle is already fresh (the change was outside the
        // build inputs, or a prior build in this burst covered it) — same staleness
        // notion `deploy`'s build-if-stale uses, so a no-op save costs nothing.
        if bundle_is_stale(&project) {
            let build_started = Instant::now();
            let outcome =
                esbuild::build_extension(&project, &esbuild::BuildOptions::default(), &quiet)?;
            if !outcome.skipped {
                total_build += build_started.elapsed();
            }
        }

        // DEPLOY — only after a clean build. Reuses the `deploy` command's copy set
        // verbatim (DESIGN §8 "the watch chain calls deploy; do NOT reimplement the copy
        // set").
        let deploy_started = Instant::now();
        deploy_project(&project, &quiet)?;
        total_deploy += deploy_started.elapsed();
        deployed.push(name.clone());
    }

    if deployed.is_empty() {
        return Ok(());
    }

    // 4. ONLY after deploy completes: trigger the host reload over IPC. This ordering is
    // the trap-closing invariant — a reload request is never sent before a successful
    // deploy. The daemon re-scans the (now freshly deployed) registry and respawns the
    // host (SPEC H §2).
    let trigger = deployed.join(", ");
    let reload_resp = client.call(Request::Reload {
        only: Some(deployed.clone()),
        strict: false,
    })?;

    report_chain(
        &trigger,
        total_build.as_millis() as u64,
        total_deploy.as_millis() as u64,
        &reload_resp,
        ctx,
    );

    // on_reload lifecycle hooks (DESIGN §5.3) — fire AFTER the reload completes, once per
    // reloaded extension, informational only (logged + skipped, never aborts the loop). The
    // engine routes hook logs to stderr so the §6.2 chain line above stays the only stdout.
    fire_on_reload_hooks(&changed.affected, &reload_resp, ctx);

    Ok(())
}

/// Resolve + run the `on_reload` hooks for each reloaded extension (§5.3): payload
/// `{project_dir, manifest_toml, name, reload_ms, ok}`. The reload's overall `ok`/`reload_ms`
/// (from the host's `ReloadResult`) are attributed to each affected extension. Best-effort:
/// a manifest-present project whose file can't be parsed is skipped; a manifestless
/// (package.json-anchored) project renders its in-memory manifest so the hook still fires.
fn fire_on_reload_hooks(affected: &BTreeMap<String, PathBuf>, reload: &Response, ctx: &Ctx) {
    use crate::hooks::lifecycle;
    use crate::hooks::payload::{HookPayload, OnReloadPayload, manifest_toml_object};

    let (ok, reload_ms) = match reload {
        Response::ReloadResult { ok, reload_ms, .. } => (*ok, *reload_ms),
        // A reload that errored out at the IPC layer never "completed" — no on_reload fires.
        _ => return,
    };

    for (name, root) in affected {
        let Ok(project) = Project::discover(root) else {
            continue;
        };
        // `manifest_toml`: the on-disk rackabel.toml when the project has one; for a
        // SYNTHESIZED (manifestless, package.json-anchored) project there is no file, so
        // render the in-memory manifest instead. Reading-and-skipping would mean the hook
        // NEVER fires for exactly the manifestless projects this milestone enables (#6).
        let manifest_toml = match &project.manifest_path {
            Some(path) => match std::fs::read_to_string(path)
                .ok()
                .and_then(|text| manifest_toml_object(&text).ok())
            {
                Some(obj) => obj,
                None => continue,
            },
            None => serde_json::to_value(&project.raw)
                .expect("ManifestRaw always renders as JSON"),
        };
        let payload = HookPayload::OnReload(OnReloadPayload {
            project_dir: project.root.display().to_string(),
            manifest_toml,
            name: name.clone(),
            reload_ms,
            ok,
        });
        lifecycle::on_reload(ctx, &project, &payload);
    }
}

/// Whether an extension's built bundle is stale and needs an esbuild run: missing
/// `dist/extension.js`/`manifest.json`, or a source file newer than the bundle. Mirrors
/// `deploy`'s build-if-stale notion (§3.3) so a save that didn't touch a build input is a
/// cheap no-op rather than a redundant rebuild.
fn bundle_is_stale(project: &Project) -> bool {
    let bundle = project.root.join(esbuild::DIST_ENTRY);
    let manifest = project.root.join("manifest.json");
    if !bundle.is_file() || !manifest.is_file() {
        return true;
    }
    let Some(bundle_m) = mtime(&bundle) else {
        return true;
    };
    // rackabel.toml drives the generated manifest.json; a change there rebuilds.
    if let Some(m) = mtime(&project.root.join(crate::manifest::MANIFEST_NAME))
        && m > bundle_m
    {
        return true;
    }
    newest_mtime_under(&project.root.join("src")).is_some_and(|m| m > bundle_m)
}

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

fn newest_mtime_under(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    let rd = std::fs::read_dir(dir).ok()?;
    for entry in rd.flatten() {
        let path = entry.path();
        let m = if path.is_dir() {
            newest_mtime_under(&path)
        } else {
            mtime(&path)
        };
        if let Some(m) = m {
            newest = Some(newest.map_or(m, |cur| cur.max(m)));
        }
    }
    newest
}

/// Rebuild a `workspace:*` library so its `dist/` is fresh before any dependent esbuild
/// (§4.4). Prefers a workspace-aware `pnpm --filter <name> build` (matching the reference
/// `predeploy` scripts), falling back to the library's own `build` script via the package
/// manager, then to `tsc --build` if neither is available.
fn build_library(lib: &WorkspaceLib, ctx: &Ctx) -> CmdResult<()> {
    use std::process::Command;

    if ctx.echo_on() {
        ui::frame::emit(
            ui::Symbol::Warn,
            &format!("rebuilding library {}…", lib.name),
            ctx,
        );
    }

    // Try `pnpm --filter <name> build` from the library root's workspace (the reference
    // monorepo path). If pnpm isn't present or the filter fails, fall back to `tsc -b`.
    let pnpm = Command::new("pnpm")
        .arg("--filter")
        .arg(&lib.name)
        .arg("build")
        .current_dir(&lib.root)
        .status();

    let ok = match pnpm {
        Ok(s) if s.success() => true,
        _ => {
            // Fallback: the library's local `tsc --build` (matches @arclight/core).
            Command::new("npx")
                .args(["tsc", "--build"])
                .current_dir(&lib.root)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
    };

    if ok {
        Ok(())
    } else {
        Err(RkError::of(
            ErrorCode::BuildFailed,
            format!("the workspace library {} failed to build", lib.name),
            "fix the library's compile errors, then save again — the dependent \
             extensions rebuild automatically",
        )
        .at(lib.root.display().to_string()))
    }
}

/// Deploy a freshly built project to the User Library by reusing the `deploy` command's
/// copy set verbatim (DESIGN §8). Expects the already-`quiet`-rooted context.
fn deploy_project(project: &Project, ctx: &Ctx) -> CmdResult<()> {
    let _ = project; // ctx.cwd is already the project root (quiet_project_ctx).
    let args = crate::cli::DeployArgs {
        release: false,
        undo: false,
        fix: false,
        dry_run: false,
    };
    crate::commands::deploy::run(&args, ctx)
}

/// A project-rooted clone of `ctx` with the dedicated `quiet` flag set so the reused
/// 0.2 build/deploy services emit NOTHING — neither their human frames nor a JSON
/// envelope — leaving the chain's single `rebuilt … → reloaded` line as the only
/// output a musician sees (DESIGN §6.2, DEVIATIONS D-66). `json` is left at the
/// caller's value (normally `false`); `quiet` suppresses both paths.
fn quiet_project_ctx(ctx: &Ctx, root: &Path) -> Ctx {
    let mut c = ctx.clone();
    c.cwd = root.to_path_buf();
    c.quiet = true;
    c
}

/// Print the §6.2 chain line: `✓ rebuilt (NNms) → updated in Live (NNms) → reloaded <name>`
/// (and surface any failed/skipped extensions). Mirrors the transcript shape exactly.
fn report_chain(trigger: &str, build_ms: u64, deploy_ms: u64, reload: &Response, ctx: &Ctx) {
    if !ctx.echo_on() {
        return;
    }
    match reload {
        Response::ReloadResult {
            ok,
            reloaded,
            failed,
            skipped,
            ..
        } => {
            let names = if reloaded.is_empty() {
                trigger.to_string()
            } else {
                reloaded.join(", ")
            };
            let sym = if *ok {
                ui::Symbol::Good
            } else {
                ui::Symbol::Bad
            };
            ui::frame::emit(
                sym,
                &format!(
                    "rebuilt ({build_ms}ms) → updated in Live ({deploy_ms}ms) → reloaded {names}"
                ),
                ctx,
            );
            for f in failed {
                ui::frame::emit(
                    ui::Symbol::Bad,
                    &format!("  {} failed in activate(): {}", f.name, f.error),
                    ctx,
                );
            }
            for s in skipped {
                println!("  skipped {} ({})", s.name, s.reason);
            }
        }
        Response::Error { code, msg } => {
            ui::frame::emit(
                ui::Symbol::Bad,
                &format!("reload failed [{code}]: {msg}"),
                ctx,
            );
        }
        _ => {}
    }
}

// --- the blocking watch loop ---------------------------------------------------

/// The blocking watch loop used by bare `dev` and `dev watch`. Loads the working set into
/// the host (via `set_working_set` → implicit reload), prints the §6.2 legend/liveness
/// lines, registers the `notify` watcher over the derived plan, then debounces changes
/// into the atomic chain. On a TTY it also handles the `[r]`/`[l]`/`[q]` hotkeys.
pub fn run(
    mut client: Client,
    working_set: Vec<RegistryEntry>,
    opts: WatchOpts,
    ctx: &Ctx,
) -> CmdResult<()> {
    Loop::new(working_set, opts, ctx)?.run(&mut client)
}

/// The watch loop's mutable run state (plan + name→root index + scope-hint latch).
struct Loop {
    working_set: Vec<RegistryEntry>,
    plan: WatchPlan,
    opts: WatchOpts,
    ctx: Ctx,
    /// One-time scope-hint latch (§3.3).
    hinted: bool,
}

impl Loop {
    fn new(working_set: Vec<RegistryEntry>, opts: WatchOpts, ctx: &Ctx) -> CmdResult<Self> {
        let plan = plan(&working_set, ctx)?;
        Ok(Self {
            working_set,
            plan,
            opts,
            ctx: ctx.clone(),
            hinted: false,
        })
    }

    /// Whether this loop is attached to an interactive terminal (hotkeys + block-and-wait).
    fn is_tty(&self) -> bool {
        !self.ctx.no_input && std::io::stdin().is_terminal()
    }

    fn run(&mut self, client: &mut Client) -> CmdResult<()> {
        // 1. Load the working set into the host (transient scope, not a registry edit).
        //    set_working_set triggers an implicit reload server-side.
        let names: Vec<String> = self.working_set.iter().map(|e| e.name.clone()).collect();
        let scope = if names.is_empty() { None } else { Some(names) };
        let resp = client.call(Request::SetWorkingSet { names: scope })?;
        self.print_initial(&resp, client)?;

        // 2. The scope hint (one-time): too many enabled or a slow p50 (§3.3).
        self.maybe_scope_hint(client);

        // 3. Set up the file watcher + a debounce channel.
        let (tx, rx) = std::sync::mpsc::channel::<PathBuf>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                for path in ev.paths {
                    let _ = tx.send(path);
                }
            }
        })
        .map_err(watcher_err)?;
        for root in &self.plan.roots {
            // A missing root is not fatal — watch what exists.
            let _ = watcher.watch(root, RecursiveMode::Recursive);
        }

        // 4. The legend (TTY) / liveness lines.
        self.print_legend();

        // 5. The event loop: debounce file events; on a TTY also poll stdin for hotkeys.
        self.event_loop(client, &rx)
    }

    /// The §6.2 connected + installed + liveness banner. The per-extension liveness comes
    /// from the daemon's Status snapshot after the working-set reload.
    fn print_initial(&self, _set_resp: &Response, client: &mut Client) -> CmdResult<()> {
        if !self.ctx.echo_on() {
            return Ok(());
        }
        ui::frame::emit(ui::Symbol::Good, "connected to Live", &self.ctx);
        // Pull a fresh status to print per-extension liveness + the loaded set.
        if let Ok(Response::Status {
            extensions, host, ..
        }) = client.call(Request::Status)
        {
            for e in &extensions {
                match e.lifecycle {
                    crate::dev::Lifecycle::Loaded => {
                        println!("  ● live  {}  build OK  watching your source files", e.name);
                    }
                    crate::dev::Lifecycle::Skipped => {
                        println!(
                            "  ○ skipped  {}  ({})",
                            e.name,
                            e.skip_reason.as_deref().unwrap_or("incompatible")
                        );
                    }
                    crate::dev::Lifecycle::Failed => {
                        ui::frame::emit(
                            ui::Symbol::Bad,
                            &format!(
                                "  {} failed in activate(): {}",
                                e.name,
                                e.error.as_deref().unwrap_or("activate threw")
                            ),
                            &self.ctx,
                        );
                    }
                    _ => {
                        println!("  · {}  {}", e.name, lifecycle_word(e.lifecycle));
                    }
                }
            }
            if let crate::dev::HostState::Running { api_version, .. } = &host
                && self.ctx.verbose
            {
                println!("  host API {api_version}");
            }
        }
        if self.opts.inspect.is_some()
            && let Some(ins) = &self.opts.inspect
        {
            println!("  inspector on {}:{}", ins.host, ins.port);
        }
        if self.opts.emit_launch_config {
            println!("  wrote .vscode/launch.json (attach the debugger to the host)");
        }
        Ok(())
    }

    /// The §6.2 hotkey legend (TTY only) or the non-TTY liveness note.
    fn print_legend(&self) {
        if !self.ctx.echo_on() {
            return;
        }
        if self.is_tty() {
            if self.opts.auto_reload {
                println!("  keys: [l] logs   [q] quit");
                println!(
                    "        [r] force a reload now   (reloads happen automatically when you save)"
                );
            } else {
                println!("  keys: [r] reload   [l] logs   [q] quit");
                println!("        (manual reload mode — saves do NOT auto-reload)");
            }
        } else {
            // Non-interactive: no hotkeys; state the mode so logs are self-describing.
            if self.opts.auto_reload {
                println!("  watching for changes (auto-reload on save; ctrl-c to stop)");
            } else {
                println!("  watching for changes (manual reload — `rackabel dev reload`)");
            }
        }
    }

    /// Fire the one-time scope hint when the enabled set is large or the measured reload
    /// p50 exceeds the budget (§3.3).
    fn maybe_scope_hint(&mut self, client: &mut Client) {
        if self.hinted || !self.ctx.echo_on() {
            return;
        }
        let enabled = self.working_set.len();
        let p50 = match client.call(Request::Status) {
            Ok(Response::Status { reload_ms_p50, .. }) => reload_ms_p50,
            _ => None,
        };
        let over_count = enabled > SCOPE_HINT_ENABLED_THRESHOLD;
        let over_budget = p50.is_some_and(|v| v > SCOPE_HINT_P50_BUDGET_MS);
        if over_count || over_budget {
            println!(
                "  {enabled} extensions loaded — reloads re-run all of them; scope with \
                 `rackabel dev --only <name…>` for faster saves"
            );
            self.hinted = true;
        }
    }

    /// The debounce + dispatch loop. Collects file events for `debounce_ms` of quiet,
    /// classifies them, and runs the chain (or, in manual mode, just notes the change).
    /// On a TTY a parallel stdin reader feeds hotkeys through the same channel.
    fn event_loop(&mut self, client: &mut Client, rx: &Receiver<PathBuf>) -> CmdResult<()> {
        let hotkeys = self.spawn_hotkey_reader();
        let debounce = Duration::from_millis(self.opts.debounce_ms.max(1));
        let mut pending: Vec<PathBuf> = Vec::new();

        loop {
            // Drain any hotkey first (non-blocking).
            if let Some(hk) = &hotkeys {
                while let Ok(key) = hk.try_recv() {
                    match key {
                        Hotkey::Quit => {
                            self.print_quit();
                            return Ok(());
                        }
                        Hotkey::Reload => self.manual_reload(client),
                        Hotkey::Logs => self.show_logs_hint(),
                    }
                }
            }

            // Wait for the next file event (bounded so hotkeys stay responsive).
            match rx.recv_timeout(Duration::from_millis(120)) {
                Ok(path) => {
                    pending.push(path);
                    // Coalesce the burst: keep draining until `debounce` of quiet.
                    loop {
                        match rx.recv_timeout(debounce) {
                            Ok(p) => pending.push(p),
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => {
                                return Ok(());
                            }
                        }
                    }
                    self.handle_changes(client, std::mem::take(&mut pending));
                }
                Err(RecvTimeoutError::Timeout) => {
                    // No file event — loop back to poll hotkeys / re-check quit.
                    if let Some(hk) = &hotkeys
                        && hk.try_recv() == Ok(Hotkey::Quit)
                    {
                        self.print_quit();
                        return Ok(());
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
    }

    /// Process a debounced batch: classify, then (auto mode) run the chain or (manual
    /// mode) just report which extension changed so `[r]` is an informed choice.
    fn handle_changes(&mut self, client: &mut Client, paths: Vec<PathBuf>) {
        let cs = self.plan.classify(&paths);
        if cs.is_empty() {
            return;
        }
        if !self.opts.auto_reload {
            if self.ctx.echo_on() {
                let names = cs.affected_names().join(", ");
                println!("  changed: {names} — press [r] to reload (manual mode)");
            }
            return;
        }
        // The atomic chain. On a build failure it returns Err WITHOUT having issued a
        // reload, so the last good deploy stays loaded; we print the frame (§3.3 step 5).
        if let Err(e) = build_deploy_reload(&cs, client, &self.ctx) {
            ui::print_error(&e, &self.ctx);
        }
    }

    /// `[r]`: force a whole-host reload now (manual mode + the always-available hotkey).
    fn manual_reload(&self, client: &mut Client) {
        match client.call(Request::Reload {
            only: None,
            strict: false,
        }) {
            Ok(resp) => report_chain("manual", 0, 0, &resp, &self.ctx),
            Err(e) => ui::print_error(&e, &self.ctx),
        }
    }

    /// `[l]`: point the user at `rackabel dev logs -f` (a full inline tail is the LOGS
    /// agent's surface; the hotkey nudges there rather than duplicating it).
    fn show_logs_hint(&self) {
        if self.ctx.echo_on() {
            println!("  logs: run `rackabel dev logs --follow` in another terminal");
        }
    }

    /// `[q]` / ctrl-c: the loop exits but leaves the daemon (and host) running, so other
    /// terminals' `dev logs`/`dev status` keep working (DESIGN §3.1).
    fn print_quit(&self) {
        if self.ctx.echo_on() {
            println!(
                "  stopped watching (the dev host keeps running — `rackabel dev stop` to end it)"
            );
        }
    }

    /// Spawn a background stdin reader that maps `r`/`l`/`q` keystrokes to [`Hotkey`]s,
    /// but ONLY on an interactive TTY. In non-TTY mode (CI, piped) there are no hotkeys —
    /// the loop is driven purely by file events + ctrl-c (§7 `--no-input`).
    fn spawn_hotkey_reader(&self) -> Option<Receiver<Hotkey>> {
        if !self.is_tty() {
            return None;
        }
        let (tx, rx) = std::sync::mpsc::channel::<Hotkey>();
        std::thread::spawn(move || {
            use std::io::BufRead;
            let stdin = std::io::stdin();
            let lock = stdin.lock();
            for line in lock.lines() {
                let Ok(line) = line else { break };
                let key = match line.trim() {
                    "r" | "R" => Hotkey::Reload,
                    "l" | "L" => Hotkey::Logs,
                    "q" | "Q" => Hotkey::Quit,
                    _ => continue,
                };
                let quit = key == Hotkey::Quit;
                if tx.send(key).is_err() || quit {
                    break;
                }
            }
        });
        Some(rx)
    }
}

/// A parsed TTY hotkey (§6.2 legend).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hotkey {
    Reload,
    Logs,
    Quit,
}

/// A human word for a non-loaded lifecycle stage (status echo).
fn lifecycle_word(l: crate::dev::Lifecycle) -> &'static str {
    use crate::dev::Lifecycle;
    match l {
        Lifecycle::Registered => "registered",
        Lifecycle::Deployed => "deployed",
        Lifecycle::Loaded => "loaded",
        Lifecycle::Failed => "failed",
        Lifecycle::Skipped => "skipped",
    }
}

fn watcher_err(e: notify::Error) -> RkError {
    RkError::of(
        ErrorCode::HostLaunchFailed,
        "could not start the file watcher",
        "this is unexpected; run with --raw for details, or use `--no-auto-reload` \
         and reload manually with `rackabel dev reload`",
    )
    .raw(e.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, path: &Path) -> RegistryEntry {
        RegistryEntry {
            name: name.to_string(),
            path: path.to_path_buf(),
            source: crate::dev::Source::Dist,
            enabled: true,
            kind: None,
        }
    }

    fn entry_kind(name: &str, path: &Path, kind: Option<Kind>) -> RegistryEntry {
        RegistryEntry {
            kind,
            ..entry(name, path)
        }
    }

    fn ctx_for(home: &Path) -> Ctx {
        Ctx {
            no_input: true,
            json: false,
            quiet: false,
            verbose: false,
            raw: false,
            color: crate::ui::color::ColorMode::Never,
            color_err: crate::ui::color::ColorMode::Never,
            cwd: home.to_path_buf(),
            rackabel_home: home.join(".rackabel"),
            home: home.to_path_buf(),
            ableton_app: None,
            ableton_user_library: None,
            ableton_eh_mod: None,
            ableton_eh_node: None,
            ableton_extensions_dir: None,
            ableton_storage_base: None,
            rackabel_host_cmd: None,
        }
    }

    fn touch(path: &Path) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn source_globset_matches_ts_not_dist() {
        let globs = source_globset().unwrap();
        assert!(is_source_input(Path::new("/proj/src/extension.ts"), &globs));
        assert!(is_source_input(Path::new("/proj/src/a/b.tsx"), &globs));
        // dist/ and node_modules/ are outputs / churn, never inputs.
        assert!(!is_source_input(
            Path::new("/proj/dist/extension.js"),
            &globs
        ));
        assert!(!is_source_input(
            Path::new("/proj/node_modules/x/index.js"),
            &globs
        ));
        // A non-source file under src/ is ignored.
        assert!(!is_source_input(Path::new("/proj/src/README.md"), &globs));
    }

    #[test]
    fn plan_includes_extension_root() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("clip-renamer");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        let ctx = ctx_for(tmp.path());
        let p = plan(&[entry("clip-renamer", &proj)], &ctx).unwrap();
        assert!(p.roots.contains(&proj));
    }

    #[test]
    fn workspace_libs_resolved_through_node_modules_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        // A shared library with a src tree.
        let lib_root = tmp.path().join("arclight-core");
        std::fs::create_dir_all(lib_root.join("src")).unwrap();
        touch(&lib_root.join("src/index.ts"));
        // An extension depending on it via workspace:* + the node_modules symlink.
        let ext = tmp.path().join("harmonic-lens");
        std::fs::create_dir_all(ext.join("node_modules/@arclight")).unwrap();
        std::fs::write(
            ext.join("package.json"),
            r#"{"name":"harmonic-lens","dependencies":{"@arclight/core":"workspace:*"}}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&lib_root, ext.join("node_modules/@arclight/core")).unwrap();

        let ctx = ctx_for(tmp.path());
        let libs = workspace_libs(&ext, &ctx);
        assert_eq!(libs.len(), 1, "should find the one workspace lib");
        assert_eq!(libs[0].name, "@arclight/core");
        assert_eq!(
            std::fs::canonicalize(&libs[0].root).unwrap(),
            std::fs::canonicalize(&lib_root).unwrap()
        );
    }

    #[test]
    fn plan_extends_to_workspace_lib_src() {
        let tmp = tempfile::tempdir().unwrap();
        let lib_root = tmp.path().join("arclight-core");
        std::fs::create_dir_all(lib_root.join("src")).unwrap();
        let ext = tmp.path().join("harmonic-lens");
        std::fs::create_dir_all(ext.join("node_modules/@arclight")).unwrap();
        std::fs::write(
            ext.join("package.json"),
            r#"{"name":"harmonic-lens","dependencies":{"@arclight/core":"workspace:*"}}"#,
        )
        .unwrap();
        std::os::unix::fs::symlink(&lib_root, ext.join("node_modules/@arclight/core")).unwrap();

        let ctx = ctx_for(tmp.path());
        let p = plan(&[entry("harmonic-lens", &ext)], &ctx).unwrap();
        let lib_src = std::fs::canonicalize(lib_root.join("src")).unwrap();
        assert!(
            p.roots.iter().any(|r| r == &lib_src || r == &lib_root),
            "plan must watch the workspace lib's src tree, got {:?}",
            p.roots
        );
    }

    #[test]
    fn classify_extension_source_marks_affected() {
        let tmp = tempfile::tempdir().unwrap();
        let ext = tmp.path().join("clip-renamer");
        std::fs::create_dir_all(ext.join("src")).unwrap();
        let ctx = ctx_for(tmp.path());
        let p = plan(&[entry("clip-renamer", &ext)], &ctx).unwrap();
        let cs = p.classify(&[ext.join("src/extension.ts")]);
        assert_eq!(cs.affected_names(), vec!["clip-renamer".to_string()]);
        assert!(cs.libs.is_empty(), "no library edit");
    }

    #[test]
    fn classify_threads_registered_kind_into_changeset() {
        // #7: the registered kind (from `register --type`) must ride through
        // plan() -> EntryPlan.kind -> classify() -> ChangeSet.kinds, so a manifestless
        // registered project carries its kind into build_deploy_reload without re-guessing.
        let tmp = tempfile::tempdir().unwrap();
        let ext = tmp.path().join("my-device");
        std::fs::create_dir_all(ext.join("src")).unwrap();
        let ctx = ctx_for(tmp.path());
        let p = plan(&[entry_kind("my-device", &ext, Some(Kind::Device))], &ctx).unwrap();
        let cs = p.classify(&[ext.join("src/extension.ts")]);
        assert_eq!(cs.affected_names(), vec!["my-device".to_string()]);
        assert_eq!(
            cs.kinds.get("my-device").copied().flatten(),
            Some(Kind::Device),
            "registered kind threads into ChangeSet.kinds"
        );
    }

    #[test]
    fn classify_library_edit_fans_out_to_dependents_and_rebuilds_lib() {
        let tmp = tempfile::tempdir().unwrap();
        let lib_root = tmp.path().join("arclight-core");
        std::fs::create_dir_all(lib_root.join("src")).unwrap();
        // Two extensions depend on the same lib.
        for name in ["harmonic-lens", "groove-transplant"] {
            let ext = tmp.path().join(name);
            std::fs::create_dir_all(ext.join("node_modules/@arclight")).unwrap();
            std::fs::write(
                ext.join("package.json"),
                format!(r#"{{"name":"{name}","dependencies":{{"@arclight/core":"workspace:*"}}}}"#),
            )
            .unwrap();
            std::os::unix::fs::symlink(&lib_root, ext.join("node_modules/@arclight/core")).unwrap();
        }
        let ctx = ctx_for(tmp.path());
        let p = plan(
            &[
                entry("harmonic-lens", &tmp.path().join("harmonic-lens")),
                entry("groove-transplant", &tmp.path().join("groove-transplant")),
            ],
            &ctx,
        )
        .unwrap();

        // Editing the library's src fans out to BOTH dependents AND marks the lib for
        // rebuild (deps-before-dependents, §4.4).
        let lib_src_file = std::fs::canonicalize(&lib_root)
            .unwrap()
            .join("src/index.ts");
        let cs = p.classify(&[lib_src_file]);
        assert_eq!(
            cs.affected_names(),
            vec!["groove-transplant".to_string(), "harmonic-lens".to_string()]
        );
        assert_eq!(cs.libs.len(), 1, "the edited library is queued for rebuild");
        assert_eq!(cs.libs[0].name, "@arclight/core");
    }

    #[test]
    fn classify_matches_through_a_symlinked_path_prefix() {
        // REGRESSION: the OS file-watcher (FSEvents) reports the *canonical* real path of
        // a changed file, while the registry stores the path the user typed. When the
        // project lives under a symlinked prefix (the everyday macOS case: `/tmp` →
        // `/private/tmp`, firmlinked `/Users`), a lexical `starts_with` never matches and
        // NO reload ever fires. `path_under` must canonicalize both sides. Here we register
        // the extension under a symlinked alias of its real dir and feed classify the real
        // (canonical) source path — it must still mark the extension affected.
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(real.join("clip-renamer/src")).unwrap();
        std::fs::write(real.join("clip-renamer/src/extension.ts"), "// x").unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // Registry path goes through the symlink; the changed path is the canonical real one.
        let ext_via_link = link.join("clip-renamer");
        let ctx = ctx_for(tmp.path());
        let p = plan(&[entry("clip-renamer", &ext_via_link)], &ctx).unwrap();
        let changed = std::fs::canonicalize(real.join("clip-renamer/src/extension.ts")).unwrap();
        let cs = p.classify(&[changed]);
        assert_eq!(
            cs.affected_names(),
            vec!["clip-renamer".to_string()],
            "a canonical change path must match a symlinked registry root"
        );
    }

    #[test]
    fn classify_ignores_generated_manifest_write() {
        // REGRESSION (finding #2): the build step writes manifest.json at the project
        // root (it matches `**/*.json` and lives OUTSIDE dist/). If that counted as an
        // input, each chain run's own manifest write would re-trigger the chain — one
        // save → two whole-host reloads (a self-write feedback loop). It is an OUTPUT.
        let tmp = tempfile::tempdir().unwrap();
        let ext = tmp.path().join("clip-renamer");
        std::fs::create_dir_all(&ext).unwrap();
        let ctx = ctx_for(tmp.path());
        let p = plan(&[entry("clip-renamer", &ext)], &ctx).unwrap();
        let cs = p.classify(&[ext.join("manifest.json")]);
        assert!(
            cs.is_empty(),
            "a generated manifest.json write must not be actionable"
        );
        // A genuinely-edited source JSON (e.g. a data file under src/) still triggers.
        let cs2 = p.classify(&[ext.join("src/data.json")]);
        assert_eq!(cs2.affected_names(), vec!["clip-renamer".to_string()]);
    }

    #[test]
    fn classify_ignores_dist_writes() {
        // A dist/ write (the build output) must NOT trigger a rebuild — that would loop.
        let tmp = tempfile::tempdir().unwrap();
        let ext = tmp.path().join("clip-renamer");
        std::fs::create_dir_all(ext.join("dist")).unwrap();
        let ctx = ctx_for(tmp.path());
        let p = plan(&[entry("clip-renamer", &ext)], &ctx).unwrap();
        let cs = p.classify(&[ext.join("dist/extension.js")]);
        assert!(cs.is_empty(), "a dist write must not be actionable");
    }

    #[test]
    fn watch_opts_default_is_auto_reload_200ms() {
        let o = WatchOpts::default();
        assert!(o.auto_reload);
        assert_eq!(o.debounce_ms, DEFAULT_DEBOUNCE_MS);
        assert_eq!(DEFAULT_DEBOUNCE_MS, 200);
    }

    // --- the chain-ordering trap test (reload NEVER precedes deploy) ---------------

    use crate::dev::ipc::{Client, RequestEnvelope, Response, ResponseEnvelope};
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// Build a pre-built, pure-JS extension fixture under `root` whose `dist/` + manifest
    /// are NEWER than its sources, so the watch chain's build-if-stale guard SKIPS esbuild
    /// (the test stays hermetic — no node/esbuild needed). Returns the project root.
    fn prebuilt_fixture(root: &Path, slug: &str) -> PathBuf {
        let proj = root.join(slug);
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(
            proj.join("rackabel.toml"),
            format!(
                "[extension]\nname = \"{slug}\"\nauthor = \"t\"\nversion = \"0.1.0\"\n\
                 minimum_api_version = \"1.0.0\"\n"
            ),
        )
        .unwrap();
        std::fs::write(
            proj.join("src/extension.ts"),
            "export function activate(){}\n",
        )
        .unwrap();
        // The prebuilt outputs, written AFTER the sources so they're fresh.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::create_dir_all(proj.join("dist")).unwrap();
        std::fs::write(proj.join("dist/extension.js"), "module.exports={};\n").unwrap();
        std::fs::write(
            proj.join("manifest.json"),
            format!(
                "{{\"name\":\"{slug}\",\"version\":\"0.1.0\",\"entry\":\"dist/extension.js\",\
                 \"minimumApiVersion\":\"1.0.0\"}}\n"
            ),
        )
        .unwrap();
        proj
    }

    /// A fake daemon server that, on receiving a `Reload`, records whether the deployed
    /// `manifest.json` already exists in the User Library — proving deploy ran FIRST. It
    /// replies with a `reload_result` so the chain completes. Returns (socket path, the
    /// "deploy-was-present-at-reload" flag, the "reload-seen" flag).
    fn spawn_trap_server(
        sock: PathBuf,
        deployed_manifest: PathBuf,
    ) -> (Arc<AtomicBool>, Arc<AtomicBool>) {
        let deploy_before_reload = Arc::new(AtomicBool::new(false));
        let reload_seen = Arc::new(AtomicBool::new(false));
        let dbr = Arc::clone(&deploy_before_reload);
        let rs = Arc::clone(&reload_seen);
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            // One connection (the chain's Client) carries Reload (and maybe Status).
            if let Ok((stream, _)) = listener.accept() {
                let mut writer = stream.try_clone().unwrap();
                let reader = BufReader::new(stream);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if line.trim().is_empty() {
                        continue;
                    }
                    let env: RequestEnvelope = match serde_json::from_str(&line) {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    use crate::dev::ipc::Request;
                    let resp = match env.request {
                        Request::Reload { .. } => {
                            // The decisive assertion: at the moment the reload request
                            // arrives, the deployed bundle MUST already be on disk.
                            if deployed_manifest.is_file() {
                                dbr.store(true, Ordering::SeqCst);
                            }
                            rs.store(true, Ordering::SeqCst);
                            Response::ReloadResult {
                                ok: true,
                                reloaded: vec![],
                                failed: vec![],
                                skipped: vec![],
                                reload_ms: 1,
                                host_state: crate::dev::HostState::Running {
                                    pid: 1,
                                    since_ms: 0,
                                    api_version: "1.0.0".into(),
                                },
                            }
                        }
                        _ => Response::Ack {
                            working_set: None,
                            restarted: None,
                            inspector: None,
                        },
                    };
                    let out = serde_json::to_string(&ResponseEnvelope::new(resp)).unwrap();
                    if writer.write_all(out.as_bytes()).is_err() {
                        break;
                    }
                    let _ = writer.write_all(b"\n");
                    let _ = writer.flush();
                }
            }
        });
        (deploy_before_reload, reload_seen)
    }

    /// THE TRAP TEST (SPEC D §6): the watch chain must deploy the fresh bundle into the
    /// User Library BEFORE it ever issues the reload IPC. A fake daemon checks, at reload
    /// time, that the deployed manifest.json is already present — if reload ever preceded
    /// deploy this flips false and the test fails. This is the deploy-before-reload trap
    /// the whole milestone exists to close (§3.3).
    #[test]
    fn chain_deploys_before_it_reloads() {
        let tmp = tempfile::tempdir().unwrap();
        let ul = tmp.path().join("UserLibrary");
        std::fs::create_dir_all(ul.join("Extensions")).unwrap();
        let proj = prebuilt_fixture(tmp.path(), "clip-renamer");

        // A ctx whose User Library is our temp dir (deploy copies there).
        let mut ctx = ctx_for(tmp.path());
        ctx.ableton_user_library = Some(ul.clone());

        // The fake daemon socket + the path the deploy must create first.
        let sock = tmp.path().join("daemon.sock");
        let deployed_manifest = ul.join("Extensions/clip-renamer/manifest.json");
        let (deploy_before_reload, reload_seen) =
            spawn_trap_server(sock.clone(), deployed_manifest.clone());

        // Connect the chain's client to the fake daemon.
        // Brief retry: the server thread binds asynchronously.
        let mut client = None;
        for _ in 0..50 {
            if let Ok(c) = Client::connect(&sock) {
                client = Some(c);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut client = client.expect("connect to trap server");

        // Build the changeset by classifying a source edit, then run the chain.
        let plan = plan(&[entry("clip-renamer", &proj)], &ctx).unwrap();
        let cs = plan.classify(&[proj.join("src/extension.ts")]);
        assert_eq!(cs.affected_names(), vec!["clip-renamer".to_string()]);

        build_deploy_reload(&cs, &mut client, &ctx).expect("chain runs");

        assert!(
            reload_seen.load(Ordering::SeqCst),
            "a reload IPC must be issued"
        );
        assert!(
            deploy_before_reload.load(Ordering::SeqCst),
            "the deployed bundle MUST exist before the reload IPC (the trap)"
        );
        assert!(
            deployed_manifest.is_file(),
            "the bundle was deployed to the User Library"
        );
    }

    /// A FAILED build keeps the last good deploy and issues NO reload (§3.3 step 5). We
    /// simulate a build failure by removing the prebuilt dist AND the source entry so
    /// esbuild::build_extension errors before any deploy/reload.
    #[test]
    fn failed_build_does_not_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let ul = tmp.path().join("UserLibrary");
        std::fs::create_dir_all(ul.join("Extensions")).unwrap();
        let proj = prebuilt_fixture(tmp.path(), "clip-renamer");
        // Make the bundle stale (touch src newer) and remove the entry so the build fails
        // with "entry source not found" before deploy.
        std::fs::remove_file(proj.join("dist/extension.js")).unwrap();
        std::fs::remove_file(proj.join("src/extension.ts")).unwrap();

        let mut ctx = ctx_for(tmp.path());
        ctx.ableton_user_library = Some(ul.clone());

        let sock = tmp.path().join("daemon.sock");
        let deployed_manifest = ul.join("Extensions/clip-renamer/manifest.json");
        let (_dbr, reload_seen) = spawn_trap_server(sock.clone(), deployed_manifest);

        let mut client = None;
        for _ in 0..50 {
            if let Ok(c) = Client::connect(&sock) {
                client = Some(c);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut client = client.expect("connect");

        let plan = plan(&[entry("clip-renamer", &proj)], &ctx).unwrap();
        // Re-create a src dir so classify can attribute (the file is gone but the path is
        // under the root, which is what classify keys on).
        let cs = plan.classify(&[proj.join("src/extension.ts")]);
        let result = build_deploy_reload(&cs, &mut client, &ctx);
        assert!(result.is_err(), "a failed build must error");
        // Give any (erroneous) reload a moment to land, then assert none did.
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !reload_seen.load(Ordering::SeqCst),
            "NO reload may be issued on a failed build"
        );
    }

    /// THE WATCH-CHAIN HOOK INTEGRATION (§5.3): with project-local `[hooks]` declaring a
    /// `pre_deploy` (allowing) and an `on_reload`, one save through the chain must run the
    /// `pre_deploy` gate BEFORE the deploy copy and the `on_reload` hook AFTER the reload
    /// IPC completes. We prove ordering via marker files the fixture hooks touch + the
    /// deployed manifest's presence, against the same fake daemon as the trap test.
    #[test]
    fn watch_chain_fires_pre_deploy_then_on_reload() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let ul = tmp.path().join("UserLibrary");
        std::fs::create_dir_all(ul.join("Extensions")).unwrap();

        // A prebuilt fixture with a [hooks] table; the bundle is fresh so esbuild is
        // skipped (no node needed) — post_build does NOT fire (no build ran), but the
        // deploy's pre_deploy and the post-reload on_reload do.
        let proj = tmp.path().join("clip-renamer");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::create_dir_all(proj.join("hooks")).unwrap();
        let pd_marker = proj.join("pd-ran.txt");
        let or_marker = proj.join("or-ran.txt");
        let deployed_manifest = ul.join("Extensions/clip-renamer/manifest.json");
        std::fs::write(
            proj.join("rackabel.toml"),
            "[extension]\nname = \"clip-renamer\"\nauthor = \"t\"\nversion = \"0.1.0\"\n\
             minimum_api_version = \"1.0.0\"\n\
             [hooks]\npre_deploy = \"hooks/pd\"\non_reload = \"hooks/or\"\n",
        )
        .unwrap();
        std::fs::write(
            proj.join("src/extension.ts"),
            "export function activate(){}\n",
        )
        .unwrap();
        let write_exec = |path: &Path, body: &str| {
            std::fs::write(path, body).unwrap();
            let mut p = std::fs::metadata(path).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(path, p).unwrap();
        };
        // pre_deploy ALLOWS (exit 0) but records that the deploy had NOT yet happened when
        // it ran (the deployed manifest must not exist yet at pre_deploy time — ordering).
        write_exec(
            &proj.join("hooks/pd"),
            &format!(
                "#!/bin/sh\ncat >/dev/null\n\
                 if [ -f {dm} ]; then echo TOO_LATE > {pd}; else echo OK > {pd}; fi\nexit 0\n",
                dm = deployed_manifest.display(),
                pd = pd_marker.display()
            ),
        );
        write_exec(
            &proj.join("hooks/or"),
            &format!(
                "#!/bin/sh\ncat >/dev/null\ntouch {}\nexit 0\n",
                or_marker.display()
            ),
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::create_dir_all(proj.join("dist")).unwrap();
        std::fs::write(proj.join("dist/extension.js"), "module.exports={};\n").unwrap();
        std::fs::write(
            proj.join("manifest.json"),
            "{\"name\":\"clip-renamer\",\"version\":\"0.1.0\",\"entry\":\"dist/extension.js\",\
             \"minimumApiVersion\":\"1.0.0\"}\n",
        )
        .unwrap();

        let mut ctx = ctx_for(tmp.path());
        ctx.ableton_user_library = Some(ul.clone());

        let sock = tmp.path().join("daemon.sock");
        let (_dbr, reload_seen) = spawn_trap_server(sock.clone(), deployed_manifest.clone());

        let mut client = None;
        for _ in 0..50 {
            if let Ok(c) = Client::connect(&sock) {
                client = Some(c);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut client = client.expect("connect");

        let plan = plan(&[entry("clip-renamer", &proj)], &ctx).unwrap();
        let cs = plan.classify(&[proj.join("src/extension.ts")]);
        build_deploy_reload(&cs, &mut client, &ctx).expect("chain runs");

        assert!(reload_seen.load(Ordering::SeqCst), "the reload IPC fired");
        let pd = std::fs::read_to_string(&pd_marker).expect("pre_deploy ran");
        assert_eq!(
            pd.trim(),
            "OK",
            "pre_deploy must run before the deploy copy"
        );
        assert!(or_marker.is_file(), "on_reload must run after the reload");
        assert!(deployed_manifest.is_file(), "the bundle was deployed");
    }

    /// A `pre_deploy` veto in the watch chain ABORTS that extension's chain step (the
    /// deploy errors) so NO reload is issued — the host keeps its last-good artifact.
    #[test]
    fn watch_chain_pre_deploy_veto_blocks_reload() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let ul = tmp.path().join("UserLibrary");
        std::fs::create_dir_all(ul.join("Extensions")).unwrap();

        let proj = tmp.path().join("clip-renamer");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::create_dir_all(proj.join("hooks")).unwrap();
        std::fs::write(
            proj.join("rackabel.toml"),
            "[extension]\nname = \"clip-renamer\"\nauthor = \"t\"\nversion = \"0.1.0\"\n\
             minimum_api_version = \"1.0.0\"\n[hooks]\npre_deploy = \"hooks/pd\"\n",
        )
        .unwrap();
        std::fs::write(
            proj.join("src/extension.ts"),
            "export function activate(){}\n",
        )
        .unwrap();
        let pd = proj.join("hooks/pd");
        std::fs::write(&pd, "#!/bin/sh\ncat >/dev/null\nexit 1\n").unwrap();
        let mut perms = std::fs::metadata(&pd).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&pd, perms).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::create_dir_all(proj.join("dist")).unwrap();
        std::fs::write(proj.join("dist/extension.js"), "module.exports={};\n").unwrap();
        std::fs::write(
            proj.join("manifest.json"),
            "{\"name\":\"clip-renamer\",\"version\":\"0.1.0\",\"entry\":\"dist/extension.js\",\
             \"minimumApiVersion\":\"1.0.0\"}\n",
        )
        .unwrap();

        let mut ctx = ctx_for(tmp.path());
        ctx.ableton_user_library = Some(ul.clone());

        let sock = tmp.path().join("daemon.sock");
        let deployed_manifest = ul.join("Extensions/clip-renamer/manifest.json");
        let (_dbr, reload_seen) = spawn_trap_server(sock.clone(), deployed_manifest.clone());

        let mut client = None;
        for _ in 0..50 {
            if let Ok(c) = Client::connect(&sock) {
                client = Some(c);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let mut client = client.expect("connect");

        let plan = plan(&[entry("clip-renamer", &proj)], &ctx).unwrap();
        let cs = plan.classify(&[proj.join("src/extension.ts")]);
        let result = build_deploy_reload(&cs, &mut client, &ctx);
        assert!(
            result.is_err(),
            "a pre_deploy veto must error the chain step"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            !reload_seen.load(Ordering::SeqCst),
            "a vetoed deploy issues NO reload (host keeps last-good)"
        );
        assert!(!deployed_manifest.is_file(), "the veto blocked the copy");
    }
}
