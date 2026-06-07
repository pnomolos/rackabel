//! `rackabel dev disable` — flip an entry to disabled (DESIGN §2, §3.2).
//!
//! Flips the single documented `enabled` flag to `false` (registered but not loaded).
//! Operable with a dead daemon; a running host drops it on its next reload (it re-scans
//! the registry on every reload, SPEC H §2) — we hint that to the user.

use crate::cli::DevNameArg;
use crate::context::Ctx;
use crate::dev::registry::Registry;
use crate::error::CmdResult;
use crate::ui;

pub fn run(args: &DevNameArg, ctx: &Ctx) -> CmdResult<()> {
    let mut reg = Registry::load(ctx)?;
    let name = reg.set_enabled(&args.target, false)?.name.clone();
    reg.save()?;
    ui::frame::emit(ui::frame::Symbol::Good, &format!("disabled {name}"), ctx);
    ui::frame::note(
        "run `rackabel dev reload` to drop it from a running host",
        ctx,
    );
    Ok(())
}
