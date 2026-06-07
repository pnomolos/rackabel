//! `rackabel install` — hidden alias of `deploy` (DESIGN §2).
//!
//! OWNED BY THE DEPLOY AGENT (trivial). Kept because the existing M4L workflow and
//! README use `install`; `deploy` is canonical.

use crate::cli::DeployArgs;
use crate::context::Ctx;
use crate::error::CmdResult;

pub fn run(args: &DeployArgs, ctx: &Ctx) -> CmdResult<()> {
    crate::commands::deploy::run(args, ctx)
}
