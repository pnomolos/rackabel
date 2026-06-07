//! `rackabel dev enable` — flip a dormant entry back to enabled (DESIGN §2, §3.2).
//!
//! Flips the single documented `enabled` flag to `true`. Operable with a dead daemon;
//! a running host picks up the change on its next reload (it re-scans the registry on
//! every reload, SPEC H §2) — we hint that to the user.

use crate::cli::DevNameArg;
use crate::context::Ctx;
use crate::dev::registry::Registry;
use crate::error::CmdResult;
use crate::ui;

pub fn run(args: &DevNameArg, ctx: &Ctx) -> CmdResult<()> {
    let mut reg = Registry::load(ctx)?;
    let name = reg.set_enabled(&args.target, true)?.name.clone();
    reg.save()?;
    ui::frame::emit(ui::frame::Symbol::Good, &format!("enabled {name}"), ctx);
    ui::frame::note(
        "run `rackabel dev reload` to load it into a running host",
        ctx,
    );
    Ok(())
}
