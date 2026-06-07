//! `rackabel dev stop` — stop the daemon cleanly (DESIGN §2, §3.1).
//!
//! OWNED BY THE DAEMON-CORE AGENT. STUB.

use crate::context::Ctx;
use crate::dev::todo_err;
use crate::error::{CmdResult, ErrorCode};

pub fn run(_ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::NoDaemon, "`rackabel dev stop`")
}
