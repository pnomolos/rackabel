//! The detached daemon: process model, pidfile, socket server (DESIGN §3.1, SPEC H §3/§9).
//!
//! OWNED BY THE DAEMON-CORE AGENT. The foundation freezes the daemonize entry points
//! and the pidfile model (SPEC D §1) and lands compiling stubs. The daemon-core agent
//! fills the bodies: `dev start` re-execs the hidden `__daemon` subcommand; the
//! `__daemon` child `setsid()`s (becoming session+group leader so `killpg(pgid)`
//! reaches the host child), redirects stdio, writes the pidfile atomically, binds the
//! control socket, spawns the host ([`super::host::Host`]), and runs the supervisor +
//! `ipc::serve` loop. Liveness uses `kill(pid, None)`; a stale pidfile/socket is
//! reclaimed. `--foreground` skips the re-exec and `setsid` (keeps the TTY for
//! hotkeys) and puts the host in a fresh group via `setpgid`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode};

use super::todo_err;

/// The atomically-written pidfile contents (SPEC D §1). TOML at
/// `~/.rackabel/daemon/<hash>.pid`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PidFile {
    pub pid: i32,
    pub pgid: i32,
    /// The daemon's protocol/build version (a stale-or-incompatible pidfile is
    /// reclaimed rather than trusted).
    pub version: u32,
    pub sock: PathBuf,
    pub live_app: PathBuf,
    pub host_module: PathBuf,
    pub eh_node: PathBuf,
    pub started_at: u64,
}

/// The arguments the hidden `__daemon` re-exec carries (SPEC D §1). Mirrored by the
/// clap `DaemonArgs` in the CLI surface; this is the daemon-core-facing struct.
#[derive(Debug, Clone)]
pub struct DaemonParams {
    pub live_app: PathBuf,
    pub sock: PathBuf,
    pub state_home: PathBuf,
}

/// Start (or attach to) the daemon for the resolved Live install, daemonizing via the
/// `__daemon` re-exec. Returns once the daemon is up (pidfile + socket + a `Ping`) or
/// fails framed (`RK0307`). STUB.
pub fn start(_ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::DaemonStartFailed, "starting the dev host daemon")
}

/// The hidden `__daemon` entrypoint: `setsid`, write pidfile, bind socket, spawn host,
/// run the supervisor + socket-server loop. Never returns until shutdown. STUB.
pub fn run_daemon(_params: DaemonParams, _ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::DaemonStartFailed, "the dev host daemon loop")
}

/// `dev start --foreground`: run the supervisor + host + socket server in-process,
/// attached to the TTY (the CI / shell-tied escape hatch). STUB.
pub fn run_foreground(_ctx: &Ctx) -> CmdResult<()> {
    todo_err(ErrorCode::DaemonStartFailed, "the foreground dev host")
}

/// Whether a live daemon for the resolved Live install is already up (a parseable
/// pidfile + `kill(pid, None)` alive + an understood `version`). STUB returns `false`.
pub fn is_running(_ctx: &Ctx) -> bool {
    false
}
