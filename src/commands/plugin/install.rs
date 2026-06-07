//! `rackabel plugin install OWNER/REPO|<path|tarball>` (DESIGN §5.4) — STUB.
//!
//! OWNED BY THE PLUGIN-MGMT AGENT. The foundation freezes the surface this fills:
//!   - source classification via [`crate::plugin::source::PluginSource`] (gh repo vs
//!     sideloaded path/tarball);
//!   - the §5.7 remote-install confirmation (print what will be fetched/run; `--yes` to
//!     script; `--no-input` fails with `RK0403` instead of prompting);
//!   - pin + verify via [`crate::plugin::sha256`] / [`crate::plugin::git`]
//!     (`RK4007 PinMismatch` on mismatch; `--force` announces a deliberate update);
//!   - record the result in [`crate::plugin::lock::LockFile`] (including the inert 0.5
//!     `rackabel-plugin.toml` presence + hook list) and symlink into the managed bin.
//!
//! The body below classifies the source (the frozen, tested part) and then returns the
//! not-implemented frame so the tree builds and the boundary is explicit.

use crate::cli::PluginInstallArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::plugin::source::PluginSource;

pub fn run(args: &PluginInstallArgs, _ctx: &Ctx) -> CmdResult<()> {
    let source = PluginSource::parse(&args.source).ok_or_else(|| {
        RkError::of(
            ErrorCode::UsageError,
            format!("`{}` is not a valid install source", args.source),
            "use OWNER/REPO, a local path, or a .tgz tarball",
        )
        .at(args.source.clone())
    })?;

    Err(RkError::of(
        ErrorCode::PluginNotFound,
        format!(
            "plugin install for `{}` is not implemented yet",
            source.display()
        ),
        "plugin install (release-asset/clone+run, sideload, sha256/commit pinning) lands \
         with the 0.4 plugin-management work; the model + seams are in place",
    ))
}
