//! `rackabel deploy` (alias `install`) — build + copy into the Live User Library.
//!
//! OWNED BY THE DEPLOY AGENT for the Extension path (build-if-stale, copy
//! manifest+bundle+extra dist files into `<UserLibrary>/Extensions/<slug>`, native
//! deps, `--undo`, `--fix`, `--release`, `--dry-run`, `--json`). The foundation wires
//! dispatch and preserves the existing M4L `.amxd` install verbatim (the `[device]`
//! path).
//!
//! Behavior (Extension), per DESIGN §2 deploy + SPEC B §3 `deploy-extension.js`:
//!   1. `--undo` short-circuits to a safe removal of the deployed folder.
//!   2. `--release` runs `validate` first and fails the deploy on any validation
//!      error (exit 4) before touching anything.
//!   3. build-if-stale: rebuild only if the bundle/manifest is missing or any source
//!      file is newer than the built bundle (an mtime check).
//!   4. resolve the User Library (echoing the resolved path + how it was chosen).
//!   5. `slug` = the project *root directory basename* (launcher convention) — NOT
//!      the manifest name.
//!   6. copy `manifest.json` + `dist/extension.js` + `[extension.build].extra_dist_files`
//!      into `<UserLibrary>/Extensions/<slug>/` (dist files under `dist/`).
//!   7. native deps: audit the runtime graph; if a compiled `.node` is missing, emit
//!      the plain-English §3.7 error pointing at `deploy --fix` (never a bare pnpm
//!      command). `--fix` owns the native build under the hood first.

use std::path::{Path, PathBuf};

use serde_json::json;

use crate::cli::DeployArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::{self, DeviceProject, Kind, Project, ResolvedExtension};
use crate::max::paths;
use crate::services::esbuild::{self, BuildOptions};
use crate::services::native_dep;
use crate::services::user_library::{self, UserLibrary};
use crate::ui;

pub fn run(args: &DeployArgs, ctx: &Ctx) -> CmdResult<()> {
    let project = Project::discover_cwd(ctx)?;
    match project.kind()? {
        Kind::Extension => deploy_extension(&project, args, ctx),
        Kind::Device => install_device(ctx),
        Kind::Workspace => Err(RkError::new(
            ErrorCode::AmbiguousKind,
            ExitClass::Usage,
            "this is a workspace root, not a single project",
            "cd into a member directory to deploy it",
        )),
    }
}

/// The full Extension deploy path.
fn deploy_extension(project: &Project, args: &DeployArgs, ctx: &Ctx) -> CmdResult<()> {
    // --undo is a removal, not a deploy: handle it first and return.
    if args.undo {
        return undo_extension(project, args, ctx);
    }

    let ext = project.resolved_extension(ctx)?;
    let slug = project.slug();

    // --release: run validate first; any validation error fails the deploy (exit 4)
    // before we build or touch the User Library (DESIGN §2 deploy).
    if args.release {
        crate::commands::validate::run(&crate::cli::ValidateArgs { strict: false }, ctx)?;
    }

    // Resolve the deploy target up front so --dry-run can report it and a
    // User-Library-not-found error stops us before any build/copy.
    let ul = user_library::resolve(Some(project), ctx)?;
    let dest = user_library::extension_install_dir(&ul, &slug);

    // The copy set: manifest.json + dist/extension.js + extra dist files (under dist/).
    let manifest_src = project.root.join("manifest.json");
    let bundle_src = project.root.join(esbuild::DIST_ENTRY);
    let copy_set = CopySet::resolve(project, &ext, &dest);

    if args.dry_run {
        return print_plan(&ext, &slug, &ul, &dest, &copy_set, args, ctx);
    }

    // build-if-stale: rebuild only when the bundle is missing or its hash drifted.
    let built = build_if_stale(project, ctx)?;

    // Both artifacts must exist after the (possible) build.
    if !manifest_src.is_file() {
        return Err(RkError::new(
            ErrorCode::DeployCopyFailed,
            ExitClass::BuildRuntime,
            "manifest.json is missing — the build did not produce it",
            "run `rackabel build` and check it succeeds, then retry the deploy",
        )
        .at(manifest_src.display().to_string()));
    }
    if !bundle_src.is_file() {
        return Err(RkError::new(
            ErrorCode::DeployCopyFailed,
            ExitClass::BuildRuntime,
            "the built bundle is missing — the build did not produce it",
            "run `rackabel build` and check it succeeds, then retry the deploy",
        )
        .at(bundle_src.display().to_string()));
    }

    // Native deps: build them first if --fix, then audit; a missing .node is the
    // §3.7 plain-English error pointing at --fix (never a bare pnpm command).
    if args.fix {
        native_dep::fix(project, &ext, ctx)?;
    }
    let report = native_dep::audit(project, &ext)?;
    if !report.is_ok() {
        return Err(native_dep::missing_node_error(&report.missing_node));
    }

    // Create the destination and copy the set.
    std::fs::create_dir_all(&dest).map_err(copy_err(&dest))?;
    for (src, dst) in copy_set.pairs() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(copy_err(parent))?;
        }
        if !src.is_file() {
            // An extra-dist file that doesn't exist is a warning-and-skip (parity with
            // pack-extension.js), not a hard failure.
            if ctx.echo_on() {
                ui::frame::emit(
                    ui::frame::Symbol::Warn,
                    &format!("skipping missing extra dist file: {}", src.display()),
                    ctx,
                );
            }
            continue;
        }
        std::fs::copy(&src, &dst).map_err(copy_err(&dst))?;
    }

    // Native modules: recopy the collected graph into <dest>/node_modules (or clear
    // it for a pure-JS extension so a prior native deploy is cleaned up).
    native_dep::copy_native_modules(&report, &dest)?;

    // Record the deploy timestamp in state (best-effort).
    record_deploy(&project.root);

    report_success(&dest, &slug, built.rebuilt, &report, ctx);
    Ok(())
}

/// `--undo`: remove the deployed `<UserLibrary>/Extensions/<slug>` folder, but only
/// if it has the shape rackabel deploys (a `manifest.json`). Refuse with a framed
/// error if the folder exists but lacks that shape — never blindly `rm -rf` a folder
/// we didn't create (DESIGN §2 deploy "the discoverable cleanup path").
fn undo_extension(project: &Project, args: &DeployArgs, ctx: &Ctx) -> CmdResult<()> {
    let slug = project.slug();
    let ul = user_library::resolve(Some(project), ctx)?;
    let dest = user_library::extension_install_dir(&ul, &slug);

    if !dest.exists() {
        // Nothing to undo: report it as a clean no-op (not an error — the desired end
        // state, "not deployed", already holds).
        if ctx.json {
            print_json(&json!({ "undone": false, "reason": "not deployed", "dest": dest }));
        } else if ctx.echo_on() {
            ui::frame::emit(
                ui::frame::Symbol::Good,
                &format!("nothing to undo — {} is not deployed", slug),
                ctx,
            );
        }
        return Ok(());
    }

    // Safety: only remove a folder that looks rackabel-deployed.
    if !is_rackabel_deployed(&dest) {
        return Err(RkError::new(
            ErrorCode::DeployCopyFailed,
            ExitClass::BuildRuntime,
            "that Extensions folder doesn't look like a rackabel deploy — refusing to remove it",
            "if you're sure, delete it by hand. rackabel only removes folders that\n\
             contain a generated manifest.json (the shape `rackabel deploy` creates).",
        )
        .at(dest.display().to_string()));
    }

    if args.dry_run {
        if ctx.json {
            print_json(&json!({ "dry_run": true, "would_remove": dest }));
        } else {
            println!("planned undo (nothing was changed):");
            println!("  - remove {}", dest.display());
        }
        return Ok(());
    }

    std::fs::remove_dir_all(&dest).map_err(|e| {
        RkError::new(
            ErrorCode::DeployCopyFailed,
            ExitClass::BuildRuntime,
            "could not remove the deployed extension folder",
            "check write permissions for the User Library folder, then retry",
        )
        .at(dest.display().to_string())
        .raw(e.into())
    })?;

    if ctx.json {
        print_json(&json!({ "undone": true, "dest": dest }));
    } else if ctx.echo_on() {
        ui::frame::emit(
            ui::frame::Symbol::Good,
            &format!("removed {} from {}", slug, ul.path.display()),
            ctx,
        );
    }
    Ok(())
}

/// Whether a deployed folder has the shape rackabel creates: a `manifest.json` at its
/// root (the always-written member). We additionally accept the generated marker as a
/// strong signal but do not require it (a hand-built deploy via the SDK CLI would also
/// have a manifest.json — but it would NOT have our `_generated` marker; to stay safe
/// we require at minimum a manifest.json so we never `rm -rf` an unrelated folder).
fn is_rackabel_deployed(dest: &Path) -> bool {
    let manifest = dest.join("manifest.json");
    if !manifest.is_file() {
        return false;
    }
    // If it carries our generated marker, it's definitely ours. Otherwise, a bare
    // manifest.json is still the deploy shape we own (we created the folder named by
    // the slug under Extensions/), so we accept it — but a folder WITHOUT a
    // manifest.json (e.g. a user's own data folder that happens to share the slug) is
    // refused above.
    true
}

/// The set of (source, destination) file copies for a deploy. `manifest.json` and the
/// bundle go to `<dest>/...`; extra dist files go under `<dest>/dist/`.
struct CopySet {
    manifest: (PathBuf, PathBuf),
    bundle: (PathBuf, PathBuf),
    extra: Vec<(PathBuf, PathBuf)>,
}

impl CopySet {
    fn resolve(project: &Project, ext: &ResolvedExtension, dest: &Path) -> Self {
        let manifest = (
            project.root.join("manifest.json"),
            dest.join("manifest.json"),
        );
        let bundle = (
            project.root.join(esbuild::DIST_ENTRY),
            dest.join("dist").join("extension.js"),
        );
        let dist_dir = project.root.join("dist");
        let dest_dist = dest.join("dist");
        let extra = ext
            .extra_dist_files
            .iter()
            .map(|rel| (dist_dir.join(rel), dest_dist.join(rel)))
            .collect();
        Self {
            manifest,
            bundle,
            extra,
        }
    }

    /// All (source, destination) pairs in copy order.
    fn pairs(&self) -> Vec<(PathBuf, PathBuf)> {
        let mut out = vec![self.manifest.clone(), self.bundle.clone()];
        out.extend(self.extra.iter().cloned());
        out
    }
}

/// The outcome of the build-if-stale step.
struct StaleResult {
    rebuilt: bool,
}

/// Rebuild only when the bundle is missing, the generated `manifest.json` is missing,
/// or any source file is newer than the built bundle (the classic build-if-stale
/// mtime check). Returns whether a build ran. We deliberately use mtime rather than
/// the build's content-hash so deploy stays self-contained and never has to agree with
/// `build`'s internal hashing — a freshly-built bundle is always considered fresh.
fn build_if_stale(project: &Project, ctx: &Ctx) -> CmdResult<StaleResult> {
    let bundle = project.root.join(esbuild::DIST_ENTRY);
    let manifest_json = project.root.join("manifest.json");

    let fresh =
        bundle.is_file() && manifest_json.is_file() && !sources_newer_than(project, &bundle);
    if fresh {
        if ctx.echo_on() && !ctx.json {
            ui::frame::emit(
                ui::frame::Symbol::Good,
                &format!("up to date — {}", bundle.display()),
                ctx,
            );
        }
        return Ok(StaleResult { rebuilt: false });
    }

    let opts = BuildOptions {
        json: false, // the deploy reports its own success; keep build quiet under --json
        ..Default::default()
    };
    esbuild::build_extension(project, &opts, ctx)?;
    Ok(StaleResult { rebuilt: true })
}

/// Whether any source file under `src/` (or `rackabel.toml`, which feeds the generated
/// manifest) is newer than the built bundle. Missing mtimes are treated as "stale" so
/// we err on the side of rebuilding rather than shipping a stale bundle (the
/// deploy-before-reload trap, DESIGN §3).
fn sources_newer_than(project: &Project, bundle: &Path) -> bool {
    let Some(bundle_mtime) = mtime(bundle) else {
        return true;
    };
    // rackabel.toml drives the generated manifest.json; a change should rebuild.
    if let Some(m) = mtime(&project.root.join(crate::manifest::MANIFEST_NAME))
        && m > bundle_mtime
    {
        return true;
    }
    let src = project.root.join("src");
    newest_mtime_under(&src).is_some_and(|m| m > bundle_mtime)
}

/// The newest mtime of any file under `dir` (recursively), or `None` if `dir` is
/// empty/unreadable.
fn newest_mtime_under(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
    {
        if let Some(m) = entry.metadata().ok().and_then(|md| md.modified().ok()) {
            newest = Some(newest.map_or(m, |cur| cur.max(m)));
        }
    }
    newest
}

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Record the deploy timestamp in `.rackabel/state.toml` (best-effort).
fn record_deploy(root: &Path) {
    if let Ok(mut state) = manifest::state::load(root) {
        state.deployed_at = Some(now_epoch_marker());
        let _ = manifest::state::save(root, &state);
    }
}

/// A deterministic deploy timestamp without pulling in a time crate: an `@<epoch-seconds>`
/// marker. Stored only for human reference / drift tooling; epoch-seconds is sufficient
/// and avoids a date-formatting dependency. (This is intentionally NOT RFC3339 — the
/// `@` prefix makes the format unambiguous.)
fn now_epoch_marker() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("@{secs}")
}

// --- reporting --------------------------------------------------------------

fn report_success(
    dest: &Path,
    slug: &str,
    rebuilt: bool,
    report: &native_dep::NativeDepReport,
    ctx: &Ctx,
) {
    // The in-process dev-watch chain sets `quiet` so it owns the single chain line —
    // emit nothing here (neither the human frame nor the JSON envelope).
    if ctx.quiet {
        return;
    }
    if ctx.json {
        let v = json!({
            "ok": true,
            "slug": slug,
            "dest": dest,
            "rebuilt": rebuilt,
            "native_deps": report.deps.iter().map(|d| &d.name).collect::<Vec<_>>(),
        });
        print_json(&v);
        return;
    }
    ui::frame::emit(
        ui::frame::Symbol::Good,
        &format!("deployed {} → {}", slug, dest.display()),
        ctx,
    );
    if !report.deps.is_empty() && ctx.echo_on() {
        let names: Vec<&str> = report.deps.iter().map(|d| d.name.as_str()).collect();
        ui::frame::note(
            &format!(
                "copied {} native-dep package(s): {}",
                names.len(),
                names.join(", ")
            ),
            ctx,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn print_plan(
    ext: &ResolvedExtension,
    slug: &str,
    ul: &UserLibrary,
    dest: &Path,
    copy_set: &CopySet,
    args: &DeployArgs,
    ctx: &Ctx,
) -> CmdResult<()> {
    if ctx.json {
        let v = json!({
            "dry_run": true,
            "slug": slug,
            "user_library": ul.path,
            "dest": dest,
            "release": args.release,
            "fix": args.fix,
            "copies": copy_set.pairs()
                .iter()
                .map(|(s, d)| json!({ "from": s, "to": d }))
                .collect::<Vec<_>>(),
            "native_deps": ext.native_deps,
        });
        print_json(&v);
        return Ok(());
    }
    println!("planned deploy (nothing was changed):");
    println!("  - slug: {slug}");
    println!("  - target: {}", dest.display());
    if args.release {
        println!("  - validate first (--release)");
    }
    println!("  - build if stale, then copy:");
    for (src, dst) in copy_set.pairs() {
        println!("      {} -> {}", src.display(), dst.display());
    }
    if !ext.native_deps.is_empty() {
        println!(
            "  - native deps: {} (audit {} + recopy node_modules)",
            ext.native_deps.join(", "),
            if args.fix { "build" } else { "" }
        );
    }
    Ok(())
}

fn print_json(value: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(value).expect("json"));
}

/// Frame an IO error as a deploy-copy failure naming the offending path.
fn copy_err(path: &Path) -> impl Fn(std::io::Error) -> RkError {
    let path = path.to_path_buf();
    move |e| {
        RkError::new(
            ErrorCode::DeployCopyFailed,
            ExitClass::BuildRuntime,
            "could not copy the extension into the User Library",
            "check write permissions for the User Library folder, then retry",
        )
        .at(path.display().to_string())
        .raw(e.into())
    }
}

// --- the existing M4L `.amxd` install (preserved verbatim) -------------------

/// The existing M4L `.amxd` install, preserved verbatim (only re-framed errors).
fn install_device(ctx: &Ctx) -> CmdResult<()> {
    let project = DeviceProject::discover_cwd(ctx)?;
    let device_name = &project.device.name;

    let built = project
        .root
        .join("build")
        .join(format!("{device_name}.amxd"));
    if !built.is_file() {
        return Err(RkError::new(
            ErrorCode::DeployCopyFailed,
            ExitClass::BuildRuntime,
            "no built device found",
            "run `rackabel build` first",
        )
        .at(built.display().to_string()));
    }

    let Some(presets) = paths::m4l_presets_dir() else {
        return Err(RkError::of(
            ErrorCode::UserLibraryNotFound,
            "couldn't determine Ableton's User Library on this platform",
            "set [host].user_library in rackabel.toml or ABLETON_USER_LIBRARY",
        ));
    };
    if !presets.is_dir() {
        return Err(RkError::of(
            ErrorCode::UserLibraryNotFound,
            "Ableton User Library not found",
            "open Ableton Live once so it creates the User Library, then retry",
        )
        .at(presets.display().to_string()));
    }

    let dest = presets.join(format!("{device_name}.amxd"));
    std::fs::copy(&built, &dest).map_err(device_io_err(&dest))?;
    println!("Installed {}", dest.display());
    Ok(())
}

fn device_io_err(path: &Path) -> impl Fn(std::io::Error) -> RkError {
    let path = path.to_path_buf();
    move |e| {
        RkError::new(
            ErrorCode::DeployCopyFailed,
            ExitClass::BuildRuntime,
            "could not copy the device into the User Library",
            "check write permissions for the User Library folder",
        )
        .at(path.display().to_string())
        .raw(e.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn project_at(root: &Path) -> Project {
        Project {
            root: root.to_path_buf(),
            raw: crate::manifest::ManifestRaw::default(),
        }
    }

    fn ext_with(extra: Vec<&str>, native: Vec<&str>) -> ResolvedExtension {
        ResolvedExtension {
            name: "Clip Renamer".into(),
            author: "Jane".into(),
            version: semver::Version::new(0, 1, 0),
            entry: PathBuf::from("src/extension.ts"),
            minimum_api_version: semver::Version::new(1, 0, 0),
            extra_dist_files: extra.into_iter().map(String::from).collect(),
            native_deps: native.into_iter().map(String::from).collect(),
            pack_targets: vec![],
            inferred: vec![],
        }
    }

    #[test]
    fn copy_set_targets_manifest_bundle_and_extra() {
        let root = PathBuf::from("/proj/clip-renamer");
        let dest = PathBuf::from("/lib/Extensions/clip-renamer");
        let project = project_at(&root);
        let ext = ext_with(vec!["editor-client.js"], vec![]);
        let cs = CopySet::resolve(&project, &ext, &dest);
        let pairs = cs.pairs();
        // manifest.json at the root of dest.
        assert_eq!(pairs[0].0, root.join("manifest.json"));
        assert_eq!(pairs[0].1, dest.join("manifest.json"));
        // bundle goes under dist/.
        assert_eq!(pairs[1].0, root.join("dist/extension.js"));
        assert_eq!(pairs[1].1, dest.join("dist/extension.js"));
        // extra dist file is under dist/ on both sides.
        assert_eq!(pairs[2].0, root.join("dist/editor-client.js"));
        assert_eq!(pairs[2].1, dest.join("dist/editor-client.js"));
    }

    #[test]
    fn copy_set_without_extra_has_two_pairs() {
        let root = PathBuf::from("/proj/e");
        let dest = PathBuf::from("/lib/Extensions/e");
        let project = project_at(&root);
        let ext = ext_with(vec![], vec![]);
        let cs = CopySet::resolve(&project, &ext, &dest);
        assert_eq!(cs.pairs().len(), 2);
    }

    #[test]
    fn undo_safety_requires_manifest_shape() {
        let tmp = tempdir().unwrap();
        let dest = tmp.path().join("clip-renamer");
        fs::create_dir_all(&dest).unwrap();
        // A folder without a manifest.json is NOT rackabel-deployed → refuse.
        assert!(!is_rackabel_deployed(&dest));
        // Add a manifest.json → it now looks like our deploy shape.
        fs::write(dest.join("manifest.json"), "{}").unwrap();
        assert!(is_rackabel_deployed(&dest));
    }

    #[test]
    fn sources_newer_than_detects_a_changed_source() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let project = project_at(root);
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        let dist = root.join("dist");
        fs::create_dir_all(&dist).unwrap();
        let bundle = dist.join("extension.js");

        // Write the bundle first, then a newer source file.
        fs::write(&bundle, "built").unwrap();
        // Ensure a measurable mtime gap.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(src.join("extension.ts"), "newer source").unwrap();
        assert!(sources_newer_than(&project, &bundle));

        // Rewrite the bundle to be newest → no longer stale.
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&bundle, "rebuilt").unwrap();
        assert!(!sources_newer_than(&project, &bundle));
    }

    #[test]
    fn sources_newer_than_treats_missing_bundle_as_stale() {
        let tmp = tempdir().unwrap();
        let project = project_at(tmp.path());
        let bundle = tmp.path().join("dist/extension.js");
        assert!(sources_newer_than(&project, &bundle));
    }
}
