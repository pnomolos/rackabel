//! rackabel's own `.ablx` packer (DESIGN §2 pack; SPEC A §1.4 layout; SPEC B §4) —
//! OWNED BY THE PACK AGENT.
//!
//! Two jobs the official `extensions-cli package` cannot do for us:
//!
//!   1. **The pure-JS layout, reproduced** (`--no-official-cli`). A `.ablx` is a
//!      plain ZIP (deflate) whose members are exactly `manifest.json` at the root,
//!      the manifest `entry` at its relative path (e.g. `dist/extension.js`), and any
//!      `-i`/`--include` at its relative path (SPEC A §1.4). We reproduce that member
//!      set; byte-for-byte identity with `archiver` is *not* a contract (different zip
//!      writers) — the member layout is (SPEC A §1.4 closing note).
//!
//!   2. **The native-dep layout** (one archive per target). The official packager
//!      archives **no** `node_modules` (SPEC C §0), so it cannot produce a working
//!      native bundle. rackabel walks the runtime dep graph (`collectDepTree` over
//!      deps + optionalDeps, NOT peer — SPEC B §3), slims each module's `prebuilds/`
//!      to the target suffix (`slimPrebuildsDir` — SPEC B §4), and writes
//!      `<slug>-v<version>-<os>-<arch>.ablx` containing `manifest.json` + `dist/` +
//!      `node_modules/` (SPEC B §4). Same-OS-different-arch is the only supported
//!      cross build; cross-OS is a clear framed error (SPEC B §4).
//!
//! The dep-graph walk lives here (not in `services::native_dep`, whose body is owned
//! by `deploy` and is still a foundation stub) so pack is self-contained; the two
//! converge on the same algorithm and can be unified by a later refactor.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;

use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};

/// Derive the official `.ablx` filename (SPEC A §1.4): the name with every run of
/// whitespace collapsed to a single `-`, then `-<version>.ablx`. The base name is the
/// manifest `name` if present else the extension-dir basename; the version defaults to
/// `0.0.0`. rackabel always has a resolved name + version, so we take them directly.
pub fn ablx_filename(name: &str, version: &str) -> String {
    let dashed = collapse_ws(name);
    format!("{dashed}-{version}.ablx")
}

/// The native per-target filename (SPEC B §4 / DESIGN §2): `<slug>-v<version>-<os>-<arch>.ablx`.
/// The slug is the project-dir basename (launcher convention); the `target` is the
/// hyphenated `os-arch` string (e.g. `darwin-arm64`).
pub fn native_ablx_filename(slug: &str, version: &str, target: &str) -> String {
    format!("{slug}-v{version}-{target}.ablx")
}

/// Collapse every maximal run of ASCII/Unicode whitespace to a single `-`
/// (SPEC A §1.4: `name.replace(/\s+/gu, "-")`).
fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            in_ws = true;
        } else {
            if in_ws {
                out.push('-');
                in_ws = false;
            }
            out.push(ch);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Include-guard validation (SPEC A §1.4 step 5; DESIGN §2 "Include guards").
// ---------------------------------------------------------------------------

/// Validate an `-i`/`--include` entry against the official rules, *before* any pack
/// runs, emitting a rackabel three-part error (RK4004) instead of the official
/// one-line stderr (SPEC A §6 deviation 2). The rules (in order, SPEC A §1.4 step 5):
///
///   - must be **relative** (an absolute path is rejected);
///   - resolved against `extension_dir`, must stay **inside** it (no `..` escape);
///   - must **exist** on disk.
///
/// Returns the original `inc` string on success (so the caller can pass the same
/// relative value through to the packer / official CLI).
pub fn validate_include(extension_dir: &Path, inc: &str) -> CmdResult<()> {
    let inc_path = Path::new(inc);
    if inc_path.is_absolute() {
        return Err(include_err(
            "an --include path must be relative to the extension directory",
            inc,
            "use a path relative to the project root, e.g. `-i assets/icon.png`",
        ));
    }

    let resolved = extension_dir.join(inc_path);
    if !is_inside_dir(extension_dir, &resolved) {
        return Err(include_err(
            "an --include path must stay inside the extension directory",
            inc,
            "remove the `..` segments — only files within the project can be bundled",
        ));
    }

    if !resolved.exists() {
        return Err(include_err(
            "an --include path was not found",
            inc,
            "check the path (it is resolved relative to the project root)",
        ));
    }

    Ok(())
}

fn include_err(problem: &str, inc: &str, help: &str) -> RkError {
    RkError::of(
        ErrorCode::IncludeInvalid,
        problem.to_string(),
        help.to_string(),
    )
    .at(format!("--include {inc}"))
}

/// `isInsideDir(parent, child)` (SPEC A §1.4): the relative path from parent to child
/// must not start with `..` and must not be absolute. We compare on normalized
/// (lexical) forms so the check does not depend on the paths existing.
fn is_inside_dir(parent: &Path, child: &Path) -> bool {
    let parent = lexical_normalize(parent);
    let child = lexical_normalize(child);
    match child.strip_prefix(&parent) {
        Ok(_) => true,
        Err(_) => child == parent,
    }
}

/// Lexically normalize a path (resolve `.`/`..` textually, no filesystem access). Good
/// enough for the inside-dir guard, which only needs to detect `..` escapes.
fn lexical_normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Pure-JS packer (the official `.ablx` member layout, reproduced).
// ---------------------------------------------------------------------------

/// Write a pure-JS `.ablx` (SPEC A §1.4 member set): `manifest.json` at root, the
/// `entry` at its relative path, plus each (already-validated) include at its relative
/// path. `entry_rel` is the manifest's `entry` value (e.g. `dist/extension.js`) used
/// verbatim as the archive member name. Members are deflate-compressed.
pub fn pack_pure_js(
    extension_dir: &Path,
    output: &Path,
    entry_rel: &str,
    includes: &[String],
) -> CmdResult<()> {
    let manifest_path = extension_dir.join("manifest.json");
    let entry_path = extension_dir.join(entry_rel);

    if !manifest_path.is_file() {
        return Err(pack_io_err(
            "manifest.json is missing — nothing to pack",
            &manifest_path,
            "run `rackabel build --release` first to generate it",
        ));
    }
    if !entry_path.is_file() {
        return Err(pack_io_err(
            "the built bundle is missing — nothing to pack",
            &entry_path,
            "run `rackabel build --release` first to produce the bundle",
        ));
    }

    let mut zip = ZipBuilder::create(output)?;
    zip.add_file(&manifest_path, "manifest.json")?;
    zip.add_file(&entry_path, entry_rel)?;
    for inc in includes {
        let resolved = extension_dir.join(inc);
        if resolved.is_dir() {
            zip.add_dir(&resolved, inc)?;
        } else {
            zip.add_file(&resolved, inc)?;
        }
    }
    zip.finish()
}

// ---------------------------------------------------------------------------
// Native-dep packer (one archive per target).
// ---------------------------------------------------------------------------

/// Write a native-dep `.ablx` for one target (SPEC B §4): a staged tree containing
/// `manifest.json`, `dist/extension.js` (+ extra dist files), and the collected native
/// `node_modules`, each module's `prebuilds/` slimmed to the target suffix.
///
/// `target` is the hyphenated `os-arch` (e.g. `darwin-arm64`). Cross-OS packing is a
/// clear framed error (SPEC B §4); same-OS-different-arch is allowed (the `.node`
/// selection is done at load time by the host — slimming is hygiene only).
#[allow(clippy::too_many_arguments)]
pub fn pack_native_target(
    extension_dir: &Path,
    output: &Path,
    entry_rel: &str,
    extra_dist_files: &[String],
    native_deps: &[String],
    includes: &[String],
    target: &str,
    host_target: &str,
) -> CmdResult<()> {
    // Cross-OS guard: only same-OS-different-arch is supported (SPEC B §4).
    let (target_os, _target_arch) = split_target(target)?;
    let (host_os, _host_arch) = split_target(host_target)?;
    if target_os != host_os {
        return Err(RkError::of(
            ErrorCode::PackFailed,
            format!(
                "can't pack for {target} from this {host_os} machine — only \
                 same-OS, different-arch cross builds are supported"
            ),
            format!("run `rackabel pack --target {target}` on a {target_os} host"),
        )
        .at(format!("--target {target}")));
    }

    let manifest_path = extension_dir.join("manifest.json");
    let entry_path = extension_dir.join(entry_rel);
    if !manifest_path.is_file() {
        return Err(pack_io_err(
            "manifest.json is missing — nothing to pack",
            &manifest_path,
            "run `rackabel build --release` first to generate it",
        ));
    }
    if !entry_path.is_file() {
        return Err(pack_io_err(
            "the built bundle is missing — nothing to pack",
            &entry_path,
            "run `rackabel build --release` first to produce the bundle",
        ));
    }

    // Collect the native dep graph (deps + optionalDeps, NOT peer; SPEC B §3).
    let collected = collect_dep_trees(extension_dir, native_deps)?;

    let mut zip = ZipBuilder::create(output)?;
    zip.add_file(&manifest_path, "manifest.json")?;
    // The bundle is always archived at the canonical dist/extension.js path.
    zip.add_file(&entry_path, entry_rel)?;
    // Extra dist files (relative to <ext>/dist/); missing ones are skipped (SPEC B §4).
    for rel in extra_dist_files {
        let src = extension_dir.join("dist").join(rel);
        if src.is_file() {
            zip.add_file(&src, &format!("dist/{rel}"))?;
        }
    }
    // Includes (same rules as pure-JS), at their relative path.
    for inc in includes {
        let resolved = extension_dir.join(inc);
        if resolved.is_dir() {
            zip.add_dir(&resolved, inc)?;
        } else if resolved.is_file() {
            zip.add_file(&resolved, inc)?;
        }
    }
    // The native node_modules, prebuilds slimmed to the target suffix.
    for (name, dir) in &collected {
        let member_root = format!("node_modules/{name}");
        add_module_slimmed(&mut zip, dir, &member_root, target)?;
    }

    zip.finish()
}

/// Split a `os-arch` target into its two parts, e.g. `darwin-arm64` -> (`darwin`,
/// `arm64`). A malformed target is a pack error.
pub fn split_target(target: &str) -> CmdResult<(String, String)> {
    match target.split_once('-') {
        Some((os, arch)) if !os.is_empty() && !arch.is_empty() => {
            Ok((os.to_string(), arch.to_string()))
        }
        _ => Err(RkError::of(
            ErrorCode::PackFailed,
            format!("`{target}` is not a valid os-arch target"),
            "use a target like `darwin-arm64`, `darwin-x64`, `win32-x64`",
        )
        .at(format!("--target {target}"))),
    }
}

// ---------------------------------------------------------------------------
// The dep-graph walk (collectDepTree, SPEC B §3) — pnpm + pnpm aware.
// ---------------------------------------------------------------------------

/// Walk the runtime graph for every declared native dep, returning a name -> package
/// dir map (sorted for determinism). Recurses over each package's `dependencies` +
/// `optionalDependencies` (NOT `peerDependencies`); skips already-seen names.
///
/// A declared top-level dep that is not installed is an error (`RK0304`); a *sub*-dep
/// missing on disk is skipped (SPEC B §3) — optional native helpers are frequently
/// absent.
pub fn collect_dep_trees(
    extension_dir: &Path,
    native_deps: &[String],
) -> CmdResult<BTreeMap<String, PathBuf>> {
    let mut collected: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for dep in native_deps {
        collect_one(dep, extension_dir, &mut collected, &mut seen)?;
    }
    Ok(collected)
}

fn collect_one(
    pkg_name: &str,
    from_dir: &Path,
    collected: &mut BTreeMap<String, PathBuf>,
    seen: &mut std::collections::BTreeSet<String>,
) -> CmdResult<()> {
    if seen.contains(pkg_name) {
        return Ok(());
    }
    seen.insert(pkg_name.to_string());

    let dir = match find_package_dir(pkg_name, from_dir) {
        Some(d) => d,
        None => {
            if collected.is_empty() {
                // The first (top-level) dep not being installed is fatal (SPEC B §3).
                return Err(RkError::of(
                    ErrorCode::NativeDepNotCompiled,
                    format!("the native dependency `{pkg_name}` isn't installed"),
                    "install the project's dependencies (e.g. `npm install`), then \
                     rerun the pack",
                )
                .at(from_dir.display().to_string()));
            }
            // A sub-dep missing on disk is skipped (optional helper, SPEC B §3).
            return Ok(());
        }
    };

    collected.insert(pkg_name.to_string(), dir.clone());

    // Recurse over dependencies + optionalDependencies (NOT peer).
    if let Ok(raw) = std::fs::read_to_string(dir.join("package.json"))
        && let Ok(pkg) = serde_json::from_str::<serde_json::Value>(&raw)
    {
        let mut subs: Vec<String> = Vec::new();
        for key in ["dependencies", "optionalDependencies"] {
            if let Some(map) = pkg.get(key).and_then(|v| v.as_object()) {
                subs.extend(map.keys().cloned());
            }
        }
        for sub in subs {
            collect_one(&sub, &dir, collected, seen)?;
        }
    }
    Ok(())
}

/// `findPackageDir(pkgName, fromDir)` (SPEC B §3): resolve symlinks at `fromDir`
/// FIRST (critical for pnpm's `.pnpm/<pkg>@<ver>/node_modules/<sub>` sibling layout;
/// a no-op for npm's hoisted layout), then climb directories testing
/// `<dir>/node_modules/<pkgName>/package.json`. Returns that package directory.
fn find_package_dir(pkg_name: &str, from_dir: &Path) -> Option<PathBuf> {
    let start = std::fs::canonicalize(from_dir).unwrap_or_else(|_| from_dir.to_path_buf());
    let mut dir: &Path = &start;
    loop {
        let candidate = dir.join("node_modules").join(pkg_name);
        if candidate.join("package.json").is_file() {
            return Some(candidate);
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// slimPrebuildsDir (SPEC B §4) — add a module to the zip, trimming prebuilds.
// ---------------------------------------------------------------------------

/// Add a package directory to the archive under `member_root`, but when copying a
/// `prebuilds/` directory, include only the subdir matching the target suffix
/// (`<os>-<arch>`) — `slimPrebuildsDir` (SPEC B §4). Nested `node_modules` inside a
/// package are NOT followed (the graph walk already collected what is needed; copying
/// nested trees would duplicate and bloat).
fn add_module_slimmed(
    zip: &mut ZipBuilder,
    src_root: &Path,
    member_root: &str,
    target: &str,
) -> CmdResult<()> {
    add_dir_filtered(zip, src_root, src_root, member_root, target)
}

/// Whether a `prebuilds/` subdir name matches the wanted target. pkg-prebuilds names
/// these either exactly `<os>-<arch>` (e.g. `darwin-arm64`) or with a package prefix
/// (`<pkg>@<ver>-<os>-<arch>`), so accept both the exact name and the `-<target>`
/// suffix (SPEC B §4 `slimPrebuildsDir`).
fn prebuild_dir_matches(name: &str, target: &str) -> bool {
    name == target || name.ends_with(&format!("-{target}"))
}

fn add_dir_filtered(
    zip: &mut ZipBuilder,
    src_root: &Path,
    dir: &Path,
    member_root: &str,
    target: &str,
) -> CmdResult<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        pack_io_err_raw(
            "could not read a native dependency directory while packing",
            dir,
            "rerun with --raw; check the file permissions of node_modules",
            e,
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        let rel = path.strip_prefix(src_root).unwrap_or(&path);
        let member = format!("{member_root}/{}", rel.to_string_lossy());

        if path.is_dir() {
            // Skip nested node_modules (the walk already collected siblings).
            if file_name == "node_modules" {
                continue;
            }
            // Inside a prebuilds/ dir, keep only the wanted-target subdir.
            if dir.file_name().map(|n| n == "prebuilds").unwrap_or(false)
                && !prebuild_dir_matches(&file_name, target)
            {
                continue;
            }
            add_dir_filtered(zip, src_root, &path, member_root, target)?;
        } else if path.is_file() {
            zip.add_file(&path, &member)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// A tiny zip builder over the `zip` crate (deflate), with framed IO errors.
// ---------------------------------------------------------------------------

struct ZipBuilder {
    writer: zip::ZipWriter<std::fs::File>,
    output: PathBuf,
}

impl ZipBuilder {
    fn create(output: &Path) -> CmdResult<Self> {
        if let Some(parent) = output.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|e| {
                pack_io_err_raw(
                    "could not create the output directory for the .ablx",
                    parent,
                    "check write permissions, then retry",
                    e,
                )
            })?;
        }
        let file = std::fs::File::create(output).map_err(|e| {
            pack_io_err_raw(
                "could not create the .ablx archive",
                output,
                "check write permissions for the output path, then retry",
                e,
            )
        })?;
        Ok(Self {
            writer: zip::ZipWriter::new(file),
            output: output.to_path_buf(),
        })
    }

    /// Member names always use forward slashes (zip convention; matches the official
    /// archiver members like `dist/extension.js`).
    fn member_name(name: &str) -> String {
        name.replace('\\', "/")
    }

    fn add_file(&mut self, src: &Path, member: &str) -> CmdResult<()> {
        let bytes = std::fs::read(src).map_err(|e| {
            pack_io_err_raw(
                "could not read a file while building the .ablx",
                src,
                "rerun with --raw; check the file still exists and is readable",
                e,
            )
        })?;
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644);
        self.writer
            .start_file(Self::member_name(member), opts)
            .map_err(|e| self.zip_err("start a zip entry", e))?;
        self.writer.write_all(&bytes).map_err(|e| {
            pack_io_err_raw(
                "could not write into the .ablx archive",
                &self.output,
                "check free disk space and write permissions",
                e,
            )
        })?;
        Ok(())
    }

    /// Recursively add a directory's files under `member_root` (preserving structure).
    fn add_dir(&mut self, src_root: &Path, member_root: &str) -> CmdResult<()> {
        for entry in walkdir::WalkDir::new(src_root).sort_by_file_name() {
            let entry = entry.map_err(|e| {
                RkError::of(
                    ErrorCode::PackFailed,
                    "could not read an included directory while packing",
                    "rerun with --raw; check the directory permissions",
                )
                .at(src_root.display().to_string())
                .raw(anyhow::anyhow!("{e}"))
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(src_root).unwrap_or(entry.path());
            let member = format!("{member_root}/{}", rel.to_string_lossy());
            self.add_file(entry.path(), &member)?;
        }
        Ok(())
    }

    fn finish(self) -> CmdResult<()> {
        let output = self.output.clone();
        self.writer.finish().map_err(|e| {
            RkError::of(
                ErrorCode::PackFailed,
                "could not finalize the .ablx archive",
                "rerun with --raw to see the underlying error",
            )
            .at(output.display().to_string())
            .raw(anyhow::anyhow!("{e}"))
        })?;
        Ok(())
    }

    fn zip_err(&self, what: &str, e: zip::result::ZipError) -> RkError {
        RkError::of(
            ErrorCode::PackFailed,
            format!("could not {what}"),
            "rerun with --raw to see the underlying error",
        )
        .at(self.output.display().to_string())
        .raw(anyhow::anyhow!("{e}"))
    }
}

fn pack_io_err(problem: &str, at: &Path, help: &str) -> RkError {
    RkError::new(
        ErrorCode::PackFailed,
        ExitClass::BuildRuntime,
        problem.to_string(),
        help.to_string(),
    )
    .at(at.display().to_string())
}

fn pack_io_err_raw(problem: &str, at: &Path, help: &str, e: std::io::Error) -> RkError {
    pack_io_err(problem, at, help).raw(e.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Read;
    use tempfile::tempdir;

    #[test]
    fn ablx_filename_collapses_whitespace() {
        assert_eq!(
            ablx_filename("Clip Renamer", "0.1.0"),
            "Clip-Renamer-0.1.0.ablx"
        );
        assert_eq!(ablx_filename("A   B\tC", "1.0.0"), "A-B-C-1.0.0.ablx");
        assert_eq!(ablx_filename("simple", "2.3.4"), "simple-2.3.4.ablx");
    }

    #[test]
    fn native_filename_uses_slug_and_target() {
        assert_eq!(
            native_ablx_filename("clip-renamer", "0.1.0", "darwin-arm64"),
            "clip-renamer-v0.1.0-darwin-arm64.ablx"
        );
    }

    #[test]
    fn include_guard_rejects_absolute() {
        let tmp = tempdir().unwrap();
        let err = validate_include(tmp.path(), "/etc/passwd").unwrap_err();
        assert_eq!(err.code, ErrorCode::IncludeInvalid);
        assert!(err.problem.contains("relative"));
    }

    #[test]
    fn include_guard_rejects_escape() {
        let tmp = tempdir().unwrap();
        let err = validate_include(tmp.path(), "../outside.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::IncludeInvalid);
        assert!(err.problem.contains("inside"));
    }

    #[test]
    fn include_guard_rejects_missing() {
        let tmp = tempdir().unwrap();
        let err = validate_include(tmp.path(), "nope.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::IncludeInvalid);
        assert!(err.problem.contains("not found"));
    }

    #[test]
    fn include_guard_accepts_existing_relative() {
        let tmp = tempdir().unwrap();
        fs::write(tmp.path().join("icon.png"), b"x").unwrap();
        assert!(validate_include(tmp.path(), "icon.png").is_ok());
        fs::create_dir_all(tmp.path().join("assets")).unwrap();
        fs::write(tmp.path().join("assets/a.txt"), b"x").unwrap();
        assert!(validate_include(tmp.path(), "assets/a.txt").is_ok());
        assert!(validate_include(tmp.path(), "assets").is_ok());
    }

    #[test]
    fn is_inside_dir_basics() {
        let p = Path::new("/proj");
        assert!(is_inside_dir(p, Path::new("/proj/dist/x.js")));
        assert!(is_inside_dir(p, Path::new("/proj")));
        assert!(!is_inside_dir(p, Path::new("/proj/../etc")));
        assert!(!is_inside_dir(p, Path::new("/other")));
    }

    fn zip_members(path: &Path) -> Vec<String> {
        let file = fs::File::open(path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut names: Vec<String> = (0..zip.len())
            .map(|i| zip.by_index(i).unwrap().name().to_string())
            .collect();
        names.sort();
        names
    }

    fn zip_read(path: &Path, member: &str) -> String {
        let file = fs::File::open(path).unwrap();
        let mut zip = zip::ZipArchive::new(file).unwrap();
        let mut f = zip.by_name(member).unwrap();
        let mut s = String::new();
        f.read_to_string(&mut s).unwrap();
        s
    }

    #[test]
    fn pure_js_member_layout() {
        let tmp = tempdir().unwrap();
        let ext = tmp.path();
        fs::write(ext.join("manifest.json"), r#"{"name":"x"}"#).unwrap();
        fs::create_dir_all(ext.join("dist")).unwrap();
        fs::write(ext.join("dist/extension.js"), "module.exports={};").unwrap();
        fs::write(ext.join("README.md"), "hi").unwrap();
        fs::create_dir_all(ext.join("assets")).unwrap();
        fs::write(ext.join("assets/icon.png"), "png").unwrap();

        let out = ext.join("x-0.1.0.ablx");
        pack_pure_js(
            ext,
            &out,
            "dist/extension.js",
            &["README.md".into(), "assets".into()],
        )
        .unwrap();

        let members = zip_members(&out);
        assert_eq!(
            members,
            vec![
                "README.md".to_string(),
                "assets/icon.png".to_string(),
                "dist/extension.js".to_string(),
                "manifest.json".to_string(),
            ]
        );
        assert_eq!(zip_read(&out, "manifest.json"), r#"{"name":"x"}"#);
    }

    #[test]
    fn pure_js_missing_bundle_errors() {
        let tmp = tempdir().unwrap();
        let ext = tmp.path();
        fs::write(ext.join("manifest.json"), "{}").unwrap();
        let out = ext.join("x.ablx");
        let err = pack_pure_js(ext, &out, "dist/extension.js", &[]).unwrap_err();
        assert_eq!(err.code, ErrorCode::PackFailed);
    }

    #[test]
    fn collect_dep_tree_walks_deps_and_optional_not_peer() {
        let tmp = tempdir().unwrap();
        let ext = tmp.path();
        let nm = ext.join("node_modules");
        // top-level "easymidi" depends on "midi-helper" (dep) + "opt-helper" (optional);
        // it peer-depends on "peer-thing" which must NOT be collected.
        write_pkg(
            &nm.join("easymidi"),
            r#"{"name":"easymidi","dependencies":{"midi-helper":"1"},"optionalDependencies":{"opt-helper":"1"},"peerDependencies":{"peer-thing":"1"}}"#,
        );
        write_pkg(&nm.join("midi-helper"), r#"{"name":"midi-helper"}"#);
        write_pkg(&nm.join("opt-helper"), r#"{"name":"opt-helper"}"#);
        write_pkg(&nm.join("peer-thing"), r#"{"name":"peer-thing"}"#);

        let collected = collect_dep_trees(ext, &["easymidi".into()]).unwrap();
        let names: Vec<&str> = collected.keys().map(String::as_str).collect();
        assert_eq!(names, vec!["easymidi", "midi-helper", "opt-helper"]);
        assert!(!collected.contains_key("peer-thing"));
    }

    #[test]
    fn collect_dep_tree_missing_toplevel_is_error() {
        let tmp = tempdir().unwrap();
        let err = collect_dep_trees(tmp.path(), &["nope".into()]).unwrap_err();
        assert_eq!(err.code, ErrorCode::NativeDepNotCompiled);
    }

    #[test]
    fn collect_dep_tree_missing_subdep_is_skipped() {
        let tmp = tempdir().unwrap();
        let nm = tmp.path().join("node_modules");
        write_pkg(
            &nm.join("top"),
            r#"{"name":"top","optionalDependencies":{"ghost":"1"}}"#,
        );
        let collected = collect_dep_trees(tmp.path(), &["top".into()]).unwrap();
        assert!(collected.contains_key("top"));
        assert!(!collected.contains_key("ghost"));
    }

    #[test]
    fn native_target_slims_prebuilds_and_excludes_nested_node_modules() {
        let tmp = tempdir().unwrap();
        let ext = tmp.path();
        fs::write(ext.join("manifest.json"), r#"{"name":"lidal"}"#).unwrap();
        fs::create_dir_all(ext.join("dist")).unwrap();
        fs::write(ext.join("dist/extension.js"), "x").unwrap();
        fs::write(ext.join("dist/editor-client.js"), "y").unwrap();

        let nm = ext.join("node_modules");
        let dep = nm.join("easymidi");
        write_pkg(&dep, r#"{"name":"easymidi"}"#);
        // prebuilds with a wanted + an unwanted suffix.
        fs::create_dir_all(dep.join("prebuilds/darwin-arm64")).unwrap();
        fs::write(dep.join("prebuilds/darwin-arm64/node.napi.node"), "good").unwrap();
        fs::create_dir_all(dep.join("prebuilds/linux-x64")).unwrap();
        fs::write(dep.join("prebuilds/linux-x64/node.napi.node"), "bad").unwrap();
        // a nested node_modules that must be excluded.
        fs::create_dir_all(dep.join("node_modules/inner")).unwrap();
        fs::write(dep.join("node_modules/inner/x.js"), "nested").unwrap();

        let out = ext.join("lidal-v0.1.0-darwin-arm64.ablx");
        pack_native_target(
            ext,
            &out,
            "dist/extension.js",
            &["editor-client.js".into()],
            &["easymidi".into()],
            &[],
            "darwin-arm64",
            "darwin-arm64",
        )
        .unwrap();

        let members = zip_members(&out);
        assert!(members.contains(&"manifest.json".to_string()));
        assert!(members.contains(&"dist/extension.js".to_string()));
        assert!(members.contains(&"dist/editor-client.js".to_string()));
        assert!(members.contains(&"node_modules/easymidi/package.json".to_string()));
        assert!(
            members.contains(
                &"node_modules/easymidi/prebuilds/darwin-arm64/node.napi.node".to_string()
            )
        );
        // The unwanted-arch prebuild and the nested node_modules are excluded.
        assert!(!members.iter().any(|m| m.contains("linux-x64")));
        assert!(
            !members
                .iter()
                .any(|m| m.contains("node_modules/easymidi/node_modules"))
        );
    }

    #[test]
    fn native_cross_os_is_error() {
        let tmp = tempdir().unwrap();
        let ext = tmp.path();
        fs::write(ext.join("manifest.json"), "{}").unwrap();
        fs::create_dir_all(ext.join("dist")).unwrap();
        fs::write(ext.join("dist/extension.js"), "x").unwrap();
        let out = ext.join("o.ablx");
        let err = pack_native_target(
            ext,
            &out,
            "dist/extension.js",
            &[],
            &[],
            &[],
            "win32-x64",
            "darwin-arm64",
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::PackFailed);
        assert!(err.problem.contains("same-OS"));
    }

    #[test]
    fn split_target_parses_and_rejects() {
        assert_eq!(
            split_target("darwin-arm64").unwrap(),
            ("darwin".to_string(), "arm64".to_string())
        );
        assert!(split_target("garbage").is_err());
    }

    fn write_pkg(dir: &Path, json: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("package.json"), json).unwrap();
    }
}
