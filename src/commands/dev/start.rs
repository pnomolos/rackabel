//! `rackabel dev start` — launch the managed Extension Host (DESIGN §2, §3.1).
//!
//! OWNED BY THE DAEMON-CORE AGENT. STUB: the foundation wires the command into the
//! dispatch and freezes the signature; the daemon-core agent fills the daemonize /
//! foreground paths (re-exec `__daemon`, `setsid`, pidfile, socket, host spawn).

use crate::cli::DevStartArgs;
use crate::context::Ctx;
use crate::dev::todo_err;
use crate::error::{CmdResult, ErrorCode};

pub fn run(_args: &DevStartArgs, _ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::DaemonStartFailed, "`rackabel dev start`")
}
