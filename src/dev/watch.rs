//! The watch loop (DESIGN §3.3, §4.4).
//!
//! OWNED BY THE WATCH-LOOP AGENT. The foundation freezes the `WatchPlan`/`WatchOpts`
//! surface (SPEC D §4) and lands compiling stubs. The watch-loop agent fills the
//! bodies: derive watch roots+globs from each extension's build config AND the `src`
//! of every internal `workspace:*` library it depends on (§4.4); the debounced atomic
//! chain `build library? → build extension → deploy → reload` (gated on a real
//! compiler-success signal, never a sentinel, §3.3) reusing the 0.2 `esbuild`/`deploy`
//! services verbatim; working-set scoping; the TTY hotkey legend; and reload-ms +
//! scope-hint reporting. It talks to the daemon over [`super::ipc::Client`].

use std::path::PathBuf;

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode};

use super::ipc::Client;
use super::{Inspect, RegistryEntry, todo_err};

/// The derived watch set: the roots to register with `notify` and the glob filter.
pub struct WatchPlan {
    pub roots: Vec<PathBuf>,
    pub globs: globset::GlobSet,
}

/// Options for the watch loop (bare `dev` / `dev watch`).
pub struct WatchOpts {
    pub auto_reload: bool,
    pub debounce_ms: u64,
    pub raw: bool,
    pub inspect: Option<Inspect>,
    pub emit_launch_config: bool,
}

/// A debounced set of changed paths, classified into library vs extension sources by
/// the watch-loop agent. The foundation keeps it opaque.
pub struct ChangeSet {
    pub paths: Vec<PathBuf>,
}

/// Derive watch roots+globs for a working set, expanding each extension's
/// `workspace:*` library sources (§4.4). STUB.
pub fn plan(_working_set: &[RegistryEntry], _ctx: &Ctx) -> CmdResult<WatchPlan> {
    todo_err(ErrorCode::NoDaemon, "deriving the watch plan")
}

/// The blocking watch loop used by bare `dev` and `dev watch`. STUB.
pub fn run(
    _client: Client,
    _working_set: Vec<RegistryEntry>,
    _opts: WatchOpts,
    _ctx: &Ctx,
) -> CmdResult<()> {
    todo_err(ErrorCode::NoDaemon, "the dev watch loop")
}

/// The atomic build→deploy→reload chain on a debounced change. Reuses the 0.2
/// `esbuild::build_extension` + `deploy` services (must NOT duplicate them) and only
/// issues a `reload` IPC on a clean build (§3.3). STUB.
pub fn build_deploy_reload(_changed: &ChangeSet, _client: &Client, _ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::NoDaemon, "the build→deploy→reload chain")
}
