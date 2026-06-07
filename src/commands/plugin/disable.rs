//! `rackabel plugin disable <name>` (DESIGN §5.4) — STUB.
//!
//! OWNED BY THE PLUGIN-MGMT AGENT. Symmetric to [`super::enable`].

use crate::cli::PluginNameArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::plugin::lock::LockFile;

pub fn run(args: &PluginNameArgs, ctx: &Ctx) -> CmdResult<()> {
    let lock = LockFile::load(ctx)?;
    if lock.find(&args.name).is_none() {
        return Err(RkError::of(
            ErrorCode::PluginNotFound,
            format!("no plugin named `{}` is installed", args.name),
            "run `rackabel plugin list`, or `rackabel plugin install OWNER/REPO`",
        ));
    }
    Err(RkError::of(
        ErrorCode::PluginNotFound,
        format!("plugin disable `{}` is not implemented yet", args.name),
        "enable/disable lands with the plugin-management work; the lock model already \
         records the `enabled` flag",
    ))
}
