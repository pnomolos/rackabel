//! Native-dependency service (DESIGN §3.7; SPEC B §3; SPEC C §3.8).
//!
//! Extensions that declare `[extension.build].native_deps` ship one or more compiled
//! `.node` binaries that esbuild externalizes (they can't be bundled — they're loaded
//! at runtime via `process.dlopen`). For these to work in the deployed copy, the
//! whole runtime graph of each declared dep must be copied into
//! `<install>/node_modules`, and each native dep must actually have a compiled
//! `.node` on disk (pnpm blocks build scripts until approved, so a fresh install
//! often has the package but no binary — the `abletonlink` footgun in BUILDING.md).
//!
//! This module implements:
//!   * [`audit`] — read-only graph walk over `dependencies` + `optionalDependencies`
//!     (NOT `peerDependencies`), following pnpm's `realpath`-first symlink layout,
//!     and an assertion that every gyp/native package has a `.node`;
//!   * [`copy_native_modules`] — recopy the collected subtrees into the deploy target;
//!   * [`fix`] — own the native build under the hood (locate pnpm, `approve-builds`
//!     + `rebuild`) so the user never types a `pnpm` command (DESIGN §3.7).
//!
//! The Persona-A-facing failure for a missing `.node` is the plain-English §3.7
//! message that points at `rackabel deploy --fix` — never a bare `pnpm` command.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::{Project, ResolvedExtension};
use crate::services::proc;
use crate::ui;

/// The result of auditing declared native deps.
#[derive(Debug, Default)]
pub struct NativeDepReport {
    /// Every package collected from the runtime graph (declared deps + their
    /// transitive deps+optionalDeps), in stable (name-sorted) order.
    pub deps: Vec<NativeDep>,
    /// Declared native deps whose compiled `.node` could not be found — these drive
    /// the §3.7 "run `rackabel deploy --fix`" error.
    pub missing_node: Vec<String>,
}

impl NativeDepReport {
    /// Whether the audit found everything it needs to deploy a working native bundle.
    pub fn is_ok(&self) -> bool {
        self.missing_node.is_empty()
    }
}

/// One resolved package in the native-dep graph.
#[derive(Debug, Clone)]
pub struct NativeDep {
    pub name: String,
    /// The directory that *contains* this package's `node_modules/<name>` (i.e. the
    /// `from` dir; the package itself lives at `dir/node_modules/<name>`).
    pub dir: PathBuf,
    /// The package's own root (`dir/node_modules/<name>`), realpath-resolved.
    pub pkg_root: PathBuf,
    /// Whether this package is "native" (has a `binding.gyp` or `gypfile: true`).
    pub native: bool,
    /// A compiled `.node` binary found under the package root, if any.
    pub dot_node: Option<PathBuf>,
}

/// Walk the runtime graph (deps + optionalDeps, NOT peer), follow pnpm symlinks,
/// assert each declared native dep has a compiled `.node`. Read-only.
///
/// Mirrors `deploy-extension.js`'s `collectDepTree`/`assertNativeBinariesPresent`
/// (SPEC B §3): `findPackageDir` realpaths first (so pnpm's
/// `.pnpm/<pkg>@<ver>/node_modules/<sub>` sibling-symlink layout resolves), then
/// climbs `node_modules` parents. A package with a `binding.gyp`/`gypfile:true` but
/// no `.node` is recorded in `missing_node`.
pub fn audit(project: &Project, ext: &ResolvedExtension) -> CmdResult<NativeDepReport> {
    let mut report = NativeDepReport::default();
    if ext.native_deps.is_empty() {
        return Ok(report);
    }

    let mut collected: BTreeMap<String, PathBuf> = BTreeMap::new();
    for dep in &ext.native_deps {
        collect_dep_tree(dep, &project.root, &mut collected, true)?;
    }

    for (name, dir) in &collected {
        let pkg_root = real(&dir.join("node_modules").join(name));
        let native = is_native_package(&pkg_root);
        let dot_node = find_dot_node(&pkg_root);
        // A package that declares a native build (binding.gyp/gypfile) but has no
        // compiled `.node` is the footgun: pnpm blocked its build script. Only the
        // *declared* native deps drive the user-facing error (a sub-dep without its
        // binary is warned during the walk, not fatal — SPEC B §3).
        if native && dot_node.is_none() && ext.native_deps.iter().any(|d| d == name) {
            report.missing_node.push(name.clone());
        }
        report.deps.push(NativeDep {
            name: name.clone(),
            dir: dir.clone(),
            pkg_root,
            native,
            dot_node,
        });
    }
    Ok(report)
}

/// Recursively collect a dependency and its runtime sub-graph into `collected`.
///
/// `is_declared` distinguishes the top-level declared deps (a missing one is a hard
/// error — RK0304) from transitive sub-deps (a missing one is a warning we skip,
/// matching `deploy-extension.js`).
fn collect_dep_tree(
    name: &str,
    from: &Path,
    collected: &mut BTreeMap<String, PathBuf>,
    is_declared: bool,
) -> CmdResult<()> {
    if collected.contains_key(name) {
        return Ok(());
    }
    let Some(dir) = find_package_dir(name, from) else {
        if is_declared {
            return Err(RkError::of(
                ErrorCode::NativeDepNotCompiled,
                format!("native dependency `{name}` is not installed"),
                "this extension uses a compiled component that isn't installed yet —\n\
                 run `rackabel deploy --fix` to install and build it.",
            )
            .at(format!("searched from {}", from.display())));
        }
        // A transitive sub-dep not present on disk is non-fatal (it may be an
        // optionalDep that this platform doesn't need). Skip it silently.
        return Ok(());
    };
    collected.insert(name.to_string(), dir.clone());

    // Recurse over dependencies + optionalDependencies (NOT peerDependencies).
    let pkg_root = dir.join("node_modules").join(name);
    if let Some(children) = read_runtime_deps(&pkg_root) {
        for child in children {
            // Sub-deps resolve relative to the package's own dir (pnpm) — pass the
            // realpath of the package root as the `from` so the climb starts there.
            collect_dep_tree(&child, &real(&pkg_root), collected, false)?;
        }
    }
    Ok(())
}

/// Find the directory `D` such that `D/node_modules/<name>/package.json` exists,
/// realpath-resolving `from` first (pnpm) and then climbing `node_modules` parents
/// (npm hoisting). `None` if not found up to the filesystem root.
fn find_package_dir(name: &str, from: &Path) -> Option<PathBuf> {
    let mut dir = real(from);
    loop {
        let candidate = dir.join("node_modules").join(name).join("package.json");
        if candidate.is_file() {
            return Some(dir);
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => return None,
        }
    }
}

/// Read the union of `dependencies` + `optionalDependencies` keys from a package's
/// `package.json`. `None` if the file is unreadable/unparsable.
fn read_runtime_deps(pkg_root: &Path) -> Option<Vec<String>> {
    let raw = std::fs::read_to_string(pkg_root.join("package.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let mut out = Vec::new();
    for key in ["dependencies", "optionalDependencies"] {
        if let Some(map) = value.get(key).and_then(|v| v.as_object()) {
            out.extend(map.keys().cloned());
        }
    }
    Some(out)
}

/// Whether a package declares a native build: `binding.gyp` at its root OR
/// `gypfile: true` in its `package.json` (SPEC B §3 `assertNativeBinariesPresent`).
fn is_native_package(pkg_root: &Path) -> bool {
    if pkg_root.join("binding.gyp").is_file() {
        return true;
    }
    if let Ok(raw) = std::fs::read_to_string(pkg_root.join("package.json"))
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        return value.get("gypfile").and_then(|v| v.as_bool()) == Some(true);
    }
    false
}

/// Recursively search a package root for any `*.node` file, NOT descending into
/// nested `node_modules` (SPEC B §3 `hasNativeBinary`).
fn find_dot_node(pkg_root: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path) -> Option<PathBuf> {
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            // Skip an entry we can't stat (e.g. a broken symlink) instead of aborting
            // the whole walk — one bad entry must not produce a false "no .node found".
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if path.file_name().and_then(|s| s.to_str()) == Some("node_modules") {
                    continue;
                }
                if let Some(found) = walk(&path) {
                    return Some(found);
                }
            } else if path.extension().and_then(|s| s.to_str()) == Some("node") {
                return Some(path);
            }
        }
        None
    }
    walk(pkg_root)
}

/// Copy declared native deps' `node_modules` subtrees into `<install_dir>/node_modules`.
///
/// Removes any prior `<install_dir>/node_modules` first (a clean recopy, matching
/// `deploy-extension.js`'s `rm -rf dest/node_modules`), then copies each collected
/// package's realpath-resolved root to `<install_dir>/node_modules/<name>`.
pub fn copy_native_modules(report: &NativeDepReport, install_dir: &Path) -> CmdResult<()> {
    let node_modules = install_dir.join("node_modules");
    if node_modules.exists() {
        std::fs::remove_dir_all(&node_modules).map_err(copy_err(&node_modules))?;
    }
    if report.deps.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(&node_modules).map_err(copy_err(&node_modules))?;
    for dep in &report.deps {
        let dst = node_modules.join(&dep.name);
        copy_dir_recursive(&dep.pkg_root, &dst)?;
    }
    Ok(())
}

/// Recursively copy `src` to `dst`. Symlinks are recreated as symlinks where the OS
/// allows (falling back to copying the target file), matching `copyDirRecursive` in
/// `deploy-extension.js` so pnpm's internal symlinks survive the copy.
fn copy_dir_recursive(src: &Path, dst: &Path) -> CmdResult<()> {
    std::fs::create_dir_all(dst).map_err(copy_err(dst))?;
    let entries = std::fs::read_dir(src).map_err(copy_err(src))?;
    for entry in entries.flatten() {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let file_type = entry.file_type().map_err(copy_err(&from))?;
        if file_type.is_symlink() {
            copy_symlink(&from, &to)?;
        } else if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(copy_err(&to))?;
        }
    }
    Ok(())
}

/// Recreate a symlink at `to` pointing at `from`'s target; if that fails (e.g. a
/// dangling or Windows-restricted link), fall back to copying the resolved file/dir.
fn copy_symlink(from: &Path, to: &Path) -> CmdResult<()> {
    let target = std::fs::read_link(from).map_err(copy_err(from))?;
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(&target, to).is_ok();
    #[cfg(windows)]
    let made = {
        // Best-effort: try a file symlink; dir symlinks need a separate call.
        std::os::windows::fs::symlink_file(&target, to).is_ok()
            || std::os::windows::fs::symlink_dir(&target, to).is_ok()
    };
    #[cfg(not(any(unix, windows)))]
    let made = false;

    if made {
        return Ok(());
    }
    // Fallback: copy the realpath of the link target.
    let resolved = real(from);
    if resolved.is_dir() {
        copy_dir_recursive(&resolved, to)
    } else if resolved.is_file() {
        std::fs::copy(&resolved, to)
            .map(|_| ())
            .map_err(copy_err(to))
    } else {
        // Dangling link — nothing to copy; skip rather than fail the deploy.
        Ok(())
    }
}

/// The §3.7 missing-`.node` error: plain English, points at `deploy --fix`, NEVER a
/// bare `pnpm` command as the primary instruction (DESIGN §3.7 / §6.3).
pub fn missing_node_error(names: &[String]) -> RkError {
    let list = names.join(", ");
    RkError::of(
        ErrorCode::NativeDepNotCompiled,
        format!("this extension uses a compiled component that needs to be built ({list})"),
        "run `rackabel deploy --fix` — it builds the native component for you.\n\
         (nothing was installed into your User Library.)",
    )
}

/// `--fix`: locate pnpm and run `approve-builds` + `rebuild` under the hood so the
/// user never types a `pnpm` command (DESIGN §3.7). The raw pnpm commands are visible
/// only under `--verbose`.
///
/// If pnpm isn't found we fail with a plain-English environment error rather than
/// telling the musician to install pnpm — the help line stays at the rackabel level.
pub fn fix(project: &Project, ext: &ResolvedExtension, ctx: &Ctx) -> CmdResult<()> {
    if ext.native_deps.is_empty() {
        // Nothing to fix; treat as a no-op success so `deploy --fix` on a pure-JS
        // project doesn't error.
        return Ok(());
    }

    let Some(pnpm) = locate_pnpm(ctx) else {
        return Err(RkError::of(
            ErrorCode::NativeDepNotCompiled,
            "couldn't find the package manager needed to build the native component",
            "this project was set up with pnpm — install pnpm (https://pnpm.io),\n\
             then rerun `rackabel deploy --fix`.",
        ));
    };

    let pnpm = pnpm.to_string_lossy().into_owned();
    if ctx.verbose && ctx.echo_on() {
        ui::frame::note(
            &format!(
                "running: {pnpm} approve-builds (in {})",
                project.root.display()
            ),
            ctx,
        );
    }

    // 1. Approve the blocked build scripts (pnpm blocks install scripts by default).
    //    A non-zero status here is not necessarily fatal — `approve-builds` may have
    //    nothing to approve — so we only surface a hard failure on `rebuild`.
    run_pnpm(&pnpm, &["approve-builds"], &project.root, ctx)?;

    // 2. Rebuild the declared native deps so their `.node` binaries are produced. A `--`
    //    separator ends pnpm's own option parsing so a dep name is never mistaken for a
    //    flag (e.g. a manifest entry that begins with `-`); the names come from the
    //    author's rackabel.toml and are passed as argv (no shell), so this is the only
    //    way one could be misinterpreted.
    let mut rebuild_args: Vec<&str> = vec!["rebuild", "--"];
    for dep in &ext.native_deps {
        rebuild_args.push(dep);
    }
    if ctx.verbose && ctx.echo_on() {
        ui::frame::note(&format!("running: {pnpm} {}", rebuild_args.join(" ")), ctx);
    }
    let out = run_pnpm(&pnpm, &rebuild_args, &project.root, ctx)?;
    if !out.success() {
        return Err(RkError::new(
            ErrorCode::NativeDepNotCompiled,
            ExitClass::Environment,
            "the native component failed to build",
            "rerun with --verbose to see the build output, then check that your\n\
             toolchain (a C/C++ compiler) is installed.",
        )
        .raw(anyhow::anyhow!("{}{}", out.stdout, out.stderr)));
    }

    // Re-audit: the `.node` files should now be present.
    let report = audit(project, ext)?;
    if !report.is_ok() {
        return Err(RkError::new(
            ErrorCode::NativeDepNotCompiled,
            ExitClass::Environment,
            "the native component still has no compiled binary after the build",
            "rerun with --verbose to see what the build did, and report this if it persists.",
        ));
    }
    Ok(())
}

/// Locate the `pnpm` binary: the `--eh-node`-style override seam isn't applicable
/// here, so we look on PATH. (A future managed-toolchain pnpm would be checked first.)
fn locate_pnpm(_ctx: &Ctx) -> Option<PathBuf> {
    which::which("pnpm").ok()
}

/// Run a pnpm subcommand, capturing output (RK0304 / environment on a spawn failure).
fn run_pnpm(pnpm: &str, args: &[&str], cwd: &Path, _ctx: &Ctx) -> CmdResult<proc::Captured> {
    proc::capture(
        pnpm,
        args,
        cwd,
        ErrorCode::NativeDepNotCompiled,
        ExitClass::Environment,
    )
}

/// Realpath a path, resolving symlinks; falls back to the path itself if it can't be
/// canonicalized (e.g. it doesn't exist yet).
fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Frame an IO error as a deploy-copy failure naming the offending path.
fn copy_err(path: &Path) -> impl Fn(std::io::Error) -> RkError {
    let path = path.to_path_buf();
    move |e| {
        RkError::new(
            ErrorCode::DeployCopyFailed,
            ExitClass::BuildRuntime,
            "could not copy a native dependency into the deployed extension",
            "check write permissions for the User Library folder, then retry",
        )
        .at(path.display().to_string())
        .raw(e.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Write a package.json with the given JSON body at `<dir>/package.json`.
    fn write_pkg(dir: &Path, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("package.json"), body).unwrap();
    }

    /// A minimal extension declaring the given native deps.
    fn ext(native: Vec<&str>) -> ResolvedExtension {
        ResolvedExtension {
            name: "x".into(),
            author: "a".into(),
            version: semver::Version::new(0, 1, 0),
            entry: PathBuf::from("src/extension.ts"),
            minimum_api_version: semver::Version::new(1, 0, 0),
            extra_dist_files: vec![],
            native_deps: native.into_iter().map(String::from).collect(),
            pack_targets: vec![],
            inferred: vec![],
        }
    }

    fn project_at(root: &Path) -> Project {
        Project {
            root: root.to_path_buf(),
            raw: crate::manifest::ManifestRaw::default(),
            manifest_path: None,
            pkg: None,
            kind_override: None,
        }
    }

    #[test]
    fn audit_empty_when_no_native_deps() {
        let tmp = tempdir().unwrap();
        let proj = project_at(tmp.path());
        let report = audit(&proj, &ext(vec![])).unwrap();
        assert!(report.deps.is_empty());
        assert!(report.is_ok());
    }

    #[test]
    fn audit_finds_hoisted_dep_with_node_binary() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        // node_modules/easymidi with a binding.gyp and a compiled .node.
        let dep = root.join("node_modules/easymidi");
        write_pkg(&dep, r#"{"name":"easymidi","version":"1.0.0"}"#);
        fs::write(dep.join("binding.gyp"), "{}").unwrap();
        let build = dep.join("build/Release");
        fs::create_dir_all(&build).unwrap();
        fs::write(build.join("midi.node"), b"\0").unwrap();

        let proj = project_at(root);
        let report = audit(&proj, &ext(vec!["easymidi"])).unwrap();
        assert!(report.is_ok());
        assert_eq!(report.deps.len(), 1);
        let d = &report.deps[0];
        assert_eq!(d.name, "easymidi");
        assert!(d.native);
        assert!(d.dot_node.is_some());
    }

    #[test]
    fn audit_reports_missing_node_for_native_without_binary() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let dep = root.join("node_modules/abletonlink");
        write_pkg(&dep, r#"{"name":"abletonlink","gypfile":true}"#);
        // No .node produced (pnpm blocked the build).

        let proj = project_at(root);
        let report = audit(&proj, &ext(vec!["abletonlink"])).unwrap();
        assert!(!report.is_ok());
        assert_eq!(report.missing_node, vec!["abletonlink".to_string()]);
    }

    #[test]
    fn audit_declared_dep_not_installed_is_rk0304() {
        let tmp = tempdir().unwrap();
        let proj = project_at(tmp.path());
        let err = audit(&proj, &ext(vec!["easymidi"])).unwrap_err();
        assert_eq!(err.code, ErrorCode::NativeDepNotCompiled);
    }

    #[test]
    fn audit_follows_pnpm_symlinks_and_subdeps() {
        // Fabricate a pnpm-style layout:
        //   node_modules/easymidi -> .pnpm/easymidi@1/node_modules/easymidi (symlink)
        //   .pnpm/easymidi@1/node_modules/easymidi/{package.json deps:@julusian/midi}
        //   .pnpm/easymidi@1/node_modules/@julusian/midi -> .pnpm/@julusian+midi@2/.../midi
        //   that midi has binding.gyp + a .node
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let nm = root.join("node_modules");
        let pnpm = nm.join(".pnpm");

        // The real easymidi package.
        let easymidi_real = pnpm.join("easymidi@1.0.0/node_modules/easymidi");
        write_pkg(
            &easymidi_real,
            r#"{"name":"easymidi","version":"1.0.0","dependencies":{"@julusian/midi":"^2"}}"#,
        );
        // easymidi's sibling symlink to @julusian/midi inside its own node_modules.
        let julusian_real = pnpm.join("@julusian+midi@2.0.0/node_modules/@julusian/midi");
        write_pkg(
            &julusian_real,
            r#"{"name":"@julusian/midi","version":"2.0.0"}"#,
        );
        fs::write(julusian_real.join("binding.gyp"), "{}").unwrap();
        let rel = julusian_real.join("build/Release");
        fs::create_dir_all(&rel).unwrap();
        fs::write(rel.join("midi.node"), b"\0").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            // top-level node_modules/easymidi -> the real one.
            symlink(&easymidi_real, nm.join("easymidi")).unwrap();
            // easymidi's node_modules/@julusian/midi -> the real one.
            let easymidi_nm = pnpm.join("easymidi@1.0.0/node_modules/@julusian");
            fs::create_dir_all(&easymidi_nm).unwrap();
            symlink(&julusian_real, easymidi_nm.join("midi")).unwrap();
        }

        let proj = project_at(root);
        let report = audit(&proj, &ext(vec!["easymidi"])).unwrap();

        #[cfg(unix)]
        {
            // Both easymidi and @julusian/midi should be collected.
            let names: Vec<&str> = report.deps.iter().map(|d| d.name.as_str()).collect();
            assert!(names.contains(&"easymidi"), "names={names:?}");
            assert!(names.contains(&"@julusian/midi"), "names={names:?}");
            // The native sub-dep has its .node; easymidi (declared, non-native) is ok.
            assert!(report.is_ok(), "missing={:?}", report.missing_node);
        }
        // On non-unix the symlinks aren't created; the test still must not panic.
        #[cfg(not(unix))]
        let _ = report;
    }

    #[test]
    fn copy_native_modules_copies_subtree_and_clears_prior() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let dep = root.join("node_modules/easymidi");
        write_pkg(&dep, r#"{"name":"easymidi"}"#);
        let build = dep.join("build/Release");
        fs::create_dir_all(&build).unwrap();
        fs::write(build.join("midi.node"), b"binary").unwrap();

        let install = tmp.path().join("install");
        // Pre-existing node_modules content that must be cleared.
        let stale = install.join("node_modules/stale");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("old.txt"), b"old").unwrap();

        let report = NativeDepReport {
            deps: vec![NativeDep {
                name: "easymidi".into(),
                dir: root.to_path_buf(),
                pkg_root: real(&dep),
                native: true,
                dot_node: Some(build.join("midi.node")),
            }],
            missing_node: vec![],
        };
        copy_native_modules(&report, &install).unwrap();

        // The stale entry is gone, the dep + its .node copied.
        assert!(!install.join("node_modules/stale").exists());
        assert!(install.join("node_modules/easymidi/package.json").is_file());
        assert!(
            install
                .join("node_modules/easymidi/build/Release/midi.node")
                .is_file()
        );
    }

    #[test]
    fn copy_native_modules_no_deps_clears_node_modules() {
        let tmp = tempdir().unwrap();
        let install = tmp.path().join("install");
        let stale = install.join("node_modules/stale");
        fs::create_dir_all(&stale).unwrap();
        copy_native_modules(&NativeDepReport::default(), &install).unwrap();
        assert!(!install.join("node_modules").exists());
    }

    #[test]
    fn find_dot_node_skips_nested_node_modules() {
        let tmp = tempdir().unwrap();
        let pkg = tmp.path().join("pkg");
        // a .node nested under the package's own node_modules must be ignored.
        let nested = pkg.join("node_modules/inner");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("inner.node"), b"\0").unwrap();
        assert!(find_dot_node(&pkg).is_none());
        // a .node at the package root is found.
        let rel = pkg.join("build/Release");
        fs::create_dir_all(&rel).unwrap();
        fs::write(rel.join("x.node"), b"\0").unwrap();
        assert!(find_dot_node(&pkg).is_some());
    }
}
