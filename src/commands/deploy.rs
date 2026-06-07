//! `rackabel deploy` (alias `install`) — build + copy into the Live User Library.
//!
//! OWNED BY THE DEPLOY AGENT for the Extension path (build-if-stale, copy
//! manifest+bundle+extra dist files into `<UserLibrary>/Extensions/<slug>`, native
//! deps, `--undo`, `--fix`). The foundation wires dispatch and preserves the
//! existing M4L `.amxd` install verbatim (the `[device]` path).

use std::path::Path;

use crate::cli::DeployArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::{DeviceProject, Kind, Project};
use crate::max::paths;

pub fn run(args: &DeployArgs, ctx: &Ctx) -> CmdResult<()> {
    let project = Project::discover_cwd(ctx)?;
    match project.kind()? {
        Kind::Extension => deploy_extension(args, ctx),
        Kind::Device => install_device(ctx),
        Kind::Workspace => Err(RkError::new(
            ErrorCode::AmbiguousKind,
            ExitClass::Usage,
            "this is a workspace root, not a single project",
            "cd into a member directory to deploy it",
        )),
    }
}

/// Extension deploy — STUB owned by the deploy-owner.
fn deploy_extension(args: &DeployArgs, _ctx: &Ctx) -> CmdResult<()> {
    let _ = args;
    Err(crate::services::esbuild::not_implemented(
        "deploy (extension)",
    ))
}

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
    std::fs::copy(&built, &dest).map_err(io_err(&dest))?;
    println!("Installed {}", dest.display());
    Ok(())
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> RkError {
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
