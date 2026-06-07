//! `rackabel dev reload` — force a whole-host reload now (DESIGN §2, §3.3).
//!
//! OWNED BY THE REGISTRY AGENT (per SPEC D §3 file ownership). STUB: exit `0` once the
//! host re-inits and every targeted ext loaded; `1` (`RK1306`) on an `activate()`
//! failure; `3` (`RK0309`/`RK0306`) if the host can't reload. Pre-filtered skips are
//! reported (`RK4006` under `--strict`).

use crate::cli::DevReloadArgs;
use crate::context::Ctx;
use crate::dev::todo_err;
use crate::error::{CmdResult, ErrorCode};

pub fn run(_args: &DevReloadArgs, _ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::NoDaemon, "`rackabel dev reload`")
}
