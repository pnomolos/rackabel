//! `rackabel plugin disable <name>` (DESIGN §5.4) — symmetric to [`super::enable`].
//!
//! OWNED BY THE PLUGIN-MGMT AGENT. Flips the `enabled` flag off. In 0.4 a disabled managed
//! plugin is skipped in the bin search (gated dispatch); `plugin run` still reaches it
//! explicitly (§5.6 escape hatch).

use crate::cli::PluginNameArgs;
use crate::context::Ctx;
use crate::error::CmdResult;

pub fn run(args: &PluginNameArgs, ctx: &Ctx) -> CmdResult<()> {
    super::enable::set_enabled(&args.name, false, ctx)
}
