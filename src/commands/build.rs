//! `rackabel build` — compile/bundle the artifact.
//!
//! OWNED BY THE BUILD AGENT for the Extension path (it fills
//! `services::esbuild::build_extension`). The foundation wires dispatch:
//! `Kind::Extension` → `services::esbuild::build_extension` (stub today), and
//! `Kind::Device` → the existing `.amxd` assembly path (behavior unchanged).
//! The device path is preserved verbatim, only adapted to the new error frame.

use crate::cli::BuildArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::{DeviceProject, Kind, Project};
use crate::services::esbuild::{self, BuildOptions};

pub fn run(args: &BuildArgs, ctx: &Ctx) -> CmdResult<()> {
    let project = Project::discover_cwd(ctx)?;
    match project.kind()? {
        Kind::Extension => {
            let opts = BuildOptions {
                release: args.release,
                clean: args.clean,
                typecheck: args.typecheck_choice(),
                print_config: args.print_config,
                dry_run: args.dry_run,
                json: ctx.json,
            };
            esbuild::build_extension(&project, &opts, ctx).map(|_| ())
        }
        Kind::Device => build_device(ctx),
        Kind::Workspace => Err(RkError::new(
            ErrorCode::AmbiguousKind,
            ExitClass::Usage,
            "this is a workspace root, not a single project",
            "cd into a member directory, or run a workspace build (lands in a later milestone)",
        )),
    }
}

/// The existing `.amxd` assembly path. Behavior is unchanged from the previous
/// implementation: it validates the entry patch is well-formed JSON, then reports
/// that assembly is not implemented yet (the M4L `build` was always a stub).
fn build_device(ctx: &Ctx) -> CmdResult<()> {
    let project = DeviceProject::discover_cwd(ctx)?;
    let entry = project.root.join(&project.device.entry);
    if !entry.is_file() {
        return Err(RkError::new(
            ErrorCode::BundleSanity,
            ExitClass::BuildRuntime,
            "entry patch not found",
            "check `entry` in rackabel.toml",
        )
        .at(entry.display().to_string()));
    }

    let raw = std::fs::read_to_string(&entry).map_err(|e| {
        RkError::new(
            ErrorCode::BundleSanity,
            ExitClass::BuildRuntime,
            "could not read the entry patch",
            "check the file's permissions",
        )
        .at(entry.display().to_string())
        .raw(e.into())
    })?;
    serde_json::from_str::<serde_json::Value>(&raw).map_err(|e| {
        RkError::new(
            ErrorCode::BundleSanity,
            ExitClass::BuildRuntime,
            "the entry patch is not valid patch JSON",
            "open it in Max and re-save, or fix the JSON",
        )
        .at(entry.display().to_string())
        .raw(e.into())
    })?;

    // TODO (M4L milestone): wrap the patch in the .amxd container and write it to
    // build/<name>.amxd. Unchanged from the previous stub.
    Err(RkError::new(
        ErrorCode::BuildFailed,
        ExitClass::BuildRuntime,
        format!(
            "device `build` isn't implemented yet — it will assemble {} into build/{}.amxd",
            project.device.entry.display(),
            project.device.name
        ),
        "track the Max for Live milestone for device assembly",
    ))
}
