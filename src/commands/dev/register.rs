//! `rackabel dev register` — add a path to the persistent registry (DESIGN §2, §3.2).
//!
//! OWNED BY THE REGISTRY AGENT. STUB: the foundation owns the registry MODEL
//! (`crate::dev::registry::Registry` — load/save/add/add_recursive/disambiguate); the
//! registry agent wires this verb to it, including the interactive vs `--no-input`
//! `RK0312 NameCollision` policy and the recursive-vs-`[workspace].members`
//! reconciliation. clap already rejects `--name`+`--recursive` at parse time (exit 2).

use crate::cli::DevRegisterArgs;
use crate::context::Ctx;
use crate::dev::todo_err;
use crate::error::{CmdResult, ErrorCode};

pub fn run(_args: &DevRegisterArgs, _ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::NoDaemon, "`rackabel dev register`")
}
