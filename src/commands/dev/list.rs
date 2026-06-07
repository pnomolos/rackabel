//! `rackabel dev list` (alias `ls`) — show the registry with status (DESIGN §2, §3.2).
//!
//! OWNED BY THE REGISTRY AGENT. STUB (model: `Registry::load`/`entries`). Works with a
//! dead daemon (§3.2) — the agent reads live state opportunistically if one is up.

use crate::context::Ctx;
use crate::dev::todo_err;
use crate::error::{CmdResult, ErrorCode};

pub fn run(_ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::NoDaemon, "`rackabel dev list`")
}
