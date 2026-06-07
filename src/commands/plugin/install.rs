//! `rackabel plugin install OWNER/REPO|<path|tarball>` (DESIGN §5.4).
//!
//! OWNED BY THE PLUGIN-MGMT AGENT. Thin entry point: thread `--yes` through to the store
//! engine and run it. The engine ([`crate::plugin::store`]) does the work — source
//! classification, the §5.7 announce + remote-consent gate, fetch/clone/sideload into the
//! per-name store, the sha256/commit pin + `RK4007` mismatch enforcement, the managed-bin
//! symlink, and the `plugins.lock` record (including the inert 0.5 `rackabel-plugin.toml`
//! presence + hook list). `--json` emits the machine-readable install result (§7).

use crate::cli::PluginInstallArgs;
use crate::context::Ctx;
use crate::error::CmdResult;
use crate::plugin::store;

pub fn run(args: &PluginInstallArgs, ctx: &Ctx) -> CmdResult<()> {
    // `--yes` is carried on the install args; thread it to the engine's consent gate
    // without widening the shared, frozen `Ctx` model just for this one flag.
    store::set_yes(args.yes);
    store::install(args, ctx)
}
