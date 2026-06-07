//! The `rackabel plugin` command group + the PATH-subcommand catch-all dispatch
//! (DESIGN §2 plugin table, §5).
//!
//! FOUNDATION-OWNED dispatch + STUBS. The foundation freezes the routing, the
//! external-subcommand exec (the load-bearing §5.1 behavior — built-ins always win,
//! managed-bin-first, both-locations warning, the full env contract), and `plugin which`
//! (a pure read over the frozen [`crate::plugin::resolve`] surface so it works on day
//! one). The three 0.4 feature agents fill the heavier bodies:
//!   - install/search                → PLUGIN-MGMT agent
//!   - enable/disable + lock CRUD    → PLUGIN-MGMT agent
//!   - (templates live under `new`)  → TEMPLATES agent
//!
//! `plugin run` and the bare external dispatch are foundation-owned because they exercise
//! the resolution + env-contract surface every agent depends on.

pub mod disable;
pub mod enable;
pub mod external;
pub mod install;
pub mod list;
pub mod migrate;
pub mod run;
pub mod search;
pub mod which;

use crate::cli::{PluginArgs, PluginCommand};
use crate::context::Ctx;
use crate::error::CmdResult;

/// Dispatch `rackabel plugin <verb>`.
pub fn run(args: &PluginArgs, ctx: &Ctx) -> CmdResult<()> {
    match &args.command {
        PluginCommand::Install(a) => install::run(a, ctx),
        PluginCommand::List => list::run(ctx),
        PluginCommand::Which(a) => which::run(a, ctx),
        PluginCommand::Run(a) => run::run(a, ctx),
        PluginCommand::Enable(a) => enable::run(a, ctx),
        PluginCommand::Disable(a) => disable::run(a, ctx),
        PluginCommand::Search(a) => search::run(a, ctx),
        PluginCommand::Migrate(a) => migrate::run(a, ctx),
    }
}
