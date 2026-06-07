//! `rackabel dev disable` — flip an entry to disabled (DESIGN §2, §3.2).
//!
//! OWNED BY THE REGISTRY AGENT. STUB (model: `Registry::set_enabled`).

use crate::cli::DevNameArg;
use crate::context::Ctx;
use crate::dev::todo_err;
use crate::error::{CmdResult, ErrorCode};

pub fn run(_args: &DevNameArg, _ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::NoDaemon, "`rackabel dev disable`")
}
