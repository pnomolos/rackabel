//! `rackabel validate` — lint manifest + artifact against ship rules.
//!
//! OWNED BY THE VALIDATE+EXPLAIN AGENT. Checks manifest completeness,
//! `minimumApiVersion ≤ host`, version-bump, identifier drift, native `.node`
//! presence. The foundation provides a compiling stub.

use crate::cli::ValidateArgs;
use crate::context::Ctx;
use crate::error::CmdResult;
use crate::manifest::Project;

pub fn run(args: &ValidateArgs, ctx: &Ctx) -> CmdResult<()> {
    // Discover so the no-project error (RK0001) surfaces consistently even from the
    // stub; the validate-owner replaces the body.
    let _project = Project::discover_cwd(ctx)?;
    let _ = args;
    Err(crate::services::esbuild::not_implemented("validate"))
}
