//! `rackabel plugin search <term>` (DESIGN §5.4) — STUB.
//!
//! OWNED BY THE PLUGIN-MGMT AGENT. Queries the `rackabel-plugin` GitHub topic via the
//! REST API behind the [`crate::plugin::source::github_api_base`] seam (tests stub the
//! base URL; no live network in tests). The foundation freezes the seam and the
//! no-network / rate-limit error frame ([`ErrorCode::NoNetwork`]) + the `--json` boundary.

use crate::cli::PluginSearchArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::plugin::source::github_api_base;

pub fn run(args: &PluginSearchArgs, _ctx: &Ctx) -> CmdResult<()> {
    // The seam is resolved here so the boundary is exercised even from the stub: the
    // agent's body will issue `GET <base>/search/repositories?q=topic:rackabel-plugin+<term>`.
    let _base = github_api_base();
    Err(RkError::of(
        ErrorCode::PluginNotFound,
        format!("plugin search for `{}` is not implemented yet", args.term),
        "search (GitHub `rackabel-plugin` topic, with a clean no-network/rate-limit frame \
         and a --json shape) lands with the plugin-management work; the API seam is in place",
    ))
}
