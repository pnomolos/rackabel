//! `rackabel pack` — production build → distributable `.ablx` / `.amxd`.
//!
//! OWNED BY THE PACK AGENT. Pure-JS extensions shell out to `extensions-cli
//! package` (the `.ablx`); native-dep extensions use rackabel's own packer. The
//! foundation provides a compiling stub; dispatch by kind is wired so the device
//! path can be added by the M4L milestone.

use crate::cli::PackArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::{Kind, Project};

pub fn run(args: &PackArgs, ctx: &Ctx) -> CmdResult<()> {
    let project = Project::discover_cwd(ctx)?;
    match project.kind()? {
        Kind::Extension => {
            let _ = args;
            Err(crate::services::esbuild::not_implemented(
                "pack (extension)",
            ))
        }
        Kind::Device => Err(crate::services::esbuild::not_implemented("pack (device)")),
        Kind::Workspace => Err(RkError::new(
            ErrorCode::AmbiguousKind,
            ExitClass::Usage,
            "this is a workspace root, not a single project",
            "cd into a member directory to pack it",
        )),
    }
}
