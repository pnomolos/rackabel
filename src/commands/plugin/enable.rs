//! `rackabel plugin enable <name>` (DESIGN §5.4) — STUB.
//!
//! OWNED BY THE PLUGIN-MGMT AGENT. Flips the `enabled` flag on the
//! [`crate::plugin::lock`] entry (the consent gate for 0.5 hooks: enabling is the
//! consent — §5.7). The foundation lands the model + a clear not-found vs not-implemented
//! boundary.

use crate::cli::PluginNameArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::plugin::lock::LockFile;

pub fn run(args: &PluginNameArgs, ctx: &Ctx) -> CmdResult<()> {
    let lock = LockFile::load(ctx)?;
    if lock.find(&args.name).is_none() {
        return Err(not_installed(&args.name));
    }
    Err(RkError::of(
        ErrorCode::PluginNotFound,
        format!("plugin enable `{}` is not implemented yet", args.name),
        "enable/disable (the 0.5 hook consent gate) lands with the plugin-management work; \
         the lock model already records the `enabled` flag",
    ))
}

fn not_installed(name: &str) -> RkError {
    RkError::of(
        ErrorCode::PluginNotFound,
        format!("no plugin named `{name}` is installed"),
        "run `rackabel plugin list`, or `rackabel plugin install OWNER/REPO`",
    )
}
