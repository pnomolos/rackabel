//! Locate & shell out to the vendored official `extensions-cli` (DESIGN §4.7, §2
//! pack; SPEC A §1.4) — OWNED BY THE PACK AGENT.
//!
//! For the **pure-JS** `pack` path, DESIGN §4.7 is explicit: rackabel is a thin,
//! legible wrapper over the SDK's own packer (`extensions-cli package`) rather than a
//! reimplementation that drifts. The official CLI ships as a `.mjs` entry
//! (`dist/cli.mjs`) inside the vendored `@ableton-extensions/cli` package; rackabel
//! drives it through the same resolved node used for the build:
//!
//! ```text
//! node <cli.mjs> package <extensionDir> -o <output> -i <inc> -i <inc> …
//! ```
//!
//! The official packager (SPEC A §1.4) derives its own `.ablx` filename
//! (`<name-with-ws→dash>-<version|0.0.0>.ablx`) and prints **only** the absolute
//! output path on success. rackabel always passes `-o` (an explicit, predetermined
//! path) so it can surface the exact filename and optionally collect outputs into a
//! chosen directory — but the *name* it picks mirrors the official derivation so the
//! distributed artifact is name-compatible with what `extensions-cli package` would
//! have produced (DESIGN §2 pack: "surfaces that exact `<name>-<version>.ablx`
//! filename").
//!
//! Include guards are pre-validated by the caller (`commands::pack`) *before* we ever
//! invoke the official CLI, so the user sees a rackabel three-part error (RK4004),
//! never the official one-line stderr (SPEC A §6 deviation 2).

use std::path::{Path, PathBuf};

use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::services::node::NodeRuntime;

/// A located official CLI entry: the `.mjs` to hand to node, plus the package root
/// (for reporting / diagnostics).
#[derive(Debug, Clone)]
pub struct OfficialCli {
    /// The `dist/cli.mjs` entry to run with `node`.
    pub entry: PathBuf,
    /// The `@ableton-extensions/cli` package root that contains it.
    pub package_root: PathBuf,
}

/// Locate the vendored official `extensions-cli` reachable from `project_root`.
///
/// Resolution order (the layouts the toolchain actually produces, SPEC A §3.4):
///   1. `<root>/node_modules/@ableton-extensions/cli/dist/cli.mjs` — the installed
///      `file:` dep (the normal post-`npm install` layout).
///   2. `<root>/vendor/ableton-extensions-cli*/package/dist/cli.mjs` — an
///      already-expanded vendored copy (rare, but cheap to support).
///
/// Returns `RK0201` (toolkit/CLI not found) with the friendly remedy if neither
/// exists — the same environment class as a missing SDK, since the pure-JS pack path
/// depends on it.
pub fn locate(project_root: &Path) -> CmdResult<OfficialCli> {
    // 1. Installed node_modules copy.
    let installed = project_root
        .join("node_modules")
        .join("@ableton-extensions")
        .join("cli")
        .join("dist")
        .join("cli.mjs");
    if installed.is_file() {
        let package_root = installed
            .parent()
            .and_then(Path::parent)
            .unwrap_or(project_root)
            .to_path_buf();
        return Ok(OfficialCli {
            entry: installed,
            package_root,
        });
    }

    // 2. Expanded vendored copy under <root>/vendor/<…cli…>/package/dist/cli.mjs.
    let vendor = project_root.join("vendor");
    if vendor.is_dir()
        && let Ok(entries) = std::fs::read_dir(&vendor)
    {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.contains("ableton-extensions-cli") {
                let candidate = entry.path().join("package").join("dist").join("cli.mjs");
                if candidate.is_file() {
                    return Ok(OfficialCli {
                        entry: candidate,
                        package_root: entry.path().join("package"),
                    });
                }
            }
        }
    }

    Err(RkError::of(
        ErrorCode::ToolkitNotFound,
        "couldn't find the official extensions packager to build the .ablx",
        "install the project's dependencies (its package.json pins \
         @ableton-extensions/cli), e.g. `npm install`, then rerun — or pass \
         --no-official-cli to use rackabel's own packer",
    )
    .at(project_root.display().to_string()))
}

/// Run `node <cli.mjs> package <extensionDir> -o <output> [-i <inc>]…`, returning the
/// captured output. A non-zero exit is wrapped as `RK1305` (pack failed) with the
/// official CLI's stderr available behind `--raw`.
///
/// `output` is the explicit `.ablx` path rackabel chose (so the filename is known and
/// surfaced); `includes` are the *already-validated* relative include paths.
pub fn package(
    cli: &OfficialCli,
    runtime: &NodeRuntime,
    extension_dir: &Path,
    output: &Path,
    includes: &[String],
) -> CmdResult<()> {
    let bin = runtime.bin.to_string_lossy().into_owned();
    let entry = cli.entry.to_string_lossy().into_owned();
    let dir = extension_dir.to_string_lossy().into_owned();
    let out = output.to_string_lossy().into_owned();

    let mut args: Vec<&str> = vec![&entry, "package", &dir, "-o", &out];
    for inc in includes {
        args.push("-i");
        args.push(inc);
    }

    let captured = crate::services::proc::capture(
        &bin,
        &args,
        extension_dir,
        ErrorCode::PackFailed,
        ExitClass::BuildRuntime,
    )?;

    if !captured.success() {
        return Err(RkError::new(
            ErrorCode::PackFailed,
            ExitClass::BuildRuntime,
            "the official extensions packager failed to build the .ablx",
            "rerun with --raw to see the packager's output; check that manifest.json \
             and the built bundle exist (run `rackabel build --release` first)",
        )
        .at(out.clone())
        .raw(anyhow::anyhow!("{}{}", captured.stdout, captured.stderr)));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn locate_finds_installed_node_modules_copy() {
        let tmp = tempdir().unwrap();
        let dist = tmp.path().join("node_modules/@ableton-extensions/cli/dist");
        fs::create_dir_all(&dist).unwrap();
        fs::write(dist.join("cli.mjs"), "// cli").unwrap();
        let cli = locate(tmp.path()).unwrap();
        assert!(cli.entry.ends_with("cli/dist/cli.mjs"));
        assert!(cli.package_root.ends_with("@ableton-extensions/cli"));
    }

    #[test]
    fn locate_finds_expanded_vendor_copy() {
        let tmp = tempdir().unwrap();
        let dist = tmp
            .path()
            .join("vendor/ableton-extensions-cli-1.0.0-beta.0/package/dist");
        fs::create_dir_all(&dist).unwrap();
        fs::write(dist.join("cli.mjs"), "// cli").unwrap();
        let cli = locate(tmp.path()).unwrap();
        assert!(cli.entry.ends_with("package/dist/cli.mjs"));
    }

    #[test]
    fn locate_missing_is_rk0201() {
        let tmp = tempdir().unwrap();
        let err = locate(tmp.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ToolkitNotFound);
    }
}
