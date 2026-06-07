//! `rackabel dev logs` — tail/filter the host's per-extension log sink (DESIGN §2, §3.4).
//!
//! OWNED BY THE LOGS AGENT. STUB (model: `crate::dev::logs::LogSink`).

use crate::cli::DevLogsArgs;
use crate::context::Ctx;
use crate::dev::todo_err;
use crate::error::{CmdResult, ErrorCode};

pub fn run(_args: &DevLogsArgs, _ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::NoDaemon, "`rackabel dev logs`")
}
