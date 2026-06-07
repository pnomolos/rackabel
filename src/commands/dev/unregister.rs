//! `rackabel dev unregister` — remove an entry from the registry (DESIGN §2, §3.2).
//!
//! OWNED BY THE REGISTRY AGENT. STUB (model: `Registry::remove`).

use crate::cli::DevUnregisterArgs;
use crate::context::Ctx;
use crate::dev::todo_err;
use crate::error::{CmdResult, ErrorCode};

pub fn run(_args: &DevUnregisterArgs, _ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::NoDaemon, "`rackabel dev unregister`")
}
