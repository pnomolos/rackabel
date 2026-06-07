//! `rackabel dev unregister` — remove an entry from the registry (DESIGN §2, §3.2).
//!
//! `unregister NAME|PATH [--recursive]`: drops the named/at-path entry, or with
//! `--recursive` every entry whose path is under the given path (the recursive-register
//! inverse). Operable with a dead daemon. The not-found case is `RK0309` (the model's
//! framed error names the bad target and points at `dev list`).

use crate::cli::DevUnregisterArgs;
use crate::context::Ctx;
use crate::dev::registry::Registry;
use crate::error::CmdResult;
use crate::ui;

pub fn run(args: &DevUnregisterArgs, ctx: &Ctx) -> CmdResult<()> {
    let mut reg = Registry::load(ctx)?;
    let removed = reg.remove(&args.target, args.recursive)?;
    reg.save()?;
    for name in &removed {
        ui::frame::emit(
            ui::frame::Symbol::Good,
            &format!("unregistered {name}"),
            ctx,
        );
    }
    Ok(())
}
