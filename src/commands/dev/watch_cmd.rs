//! `rackabel dev watch` + the bare `rackabel dev` flagship loop (DESIGN §2, §3.1/§3.3).
//!
//! OWNED BY THE WATCH-LOOP AGENT. STUB: the foundation freezes the bare-vs-explicit
//! split and the name-vs-verb routing (`--only`/`-- <NAME…>` → registry name matcher,
//! never the verb table, §3.3). The watch-loop agent fills `run`/`run_bare`: resolve
//! the working set from the registry, start-if-needed (bare only — `dev watch` never
//! implicitly starts a daemon, RK0309 if none up), then drive the watch loop +
//! inline log tail over the IPC client.

use crate::cli::{DevArgs, DevWatchArgs};
use crate::context::Ctx;
use crate::dev::todo_err;
use crate::error::{CmdResult, ErrorCode};

/// `rackabel dev watch` — the explicit form (no implicit daemon start).
pub fn run(_args: &DevWatchArgs, _ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::NoDaemon, "`rackabel dev watch`")
}

/// Bare `rackabel dev` — start-if-needed + watch + tail (the flagship loop).
pub fn run_bare(_args: &DevArgs, _ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::DaemonStartFailed, "the bare `rackabel dev` loop")
}
