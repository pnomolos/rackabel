//! The Extension Host child lifecycle (DESIGN §3.1/§3.3/§3.5, SPEC H).
//!
//! OWNED BY THE DAEMON-CORE AGENT. The foundation freezes the `HostConfig`/`Host`/
//! `ReloadOutcome`/`RespawnDecision` surface (SPEC D §4) and lands compiling stubs
//! that return the framed not-implemented error; the daemon-core agent fills the
//! bodies: build the `initialize()` extensions array, spawn Live's bundled node
//! DIRECTLY (no shell) into the daemon's process group via a `pre_exec`
//! `setpgid`, capture stdout/stderr into the [`LogSink`], wait for the
//! `FlipMessageStreamSocket send success` connected-marker, and implement
//! whole-host reload (SIGTERM old → spawn fresh — there is NO public granular reload,
//! SPEC H §2), `killpg`-based stop, and the crash-detection/backoff/crash-looping
//! state machine (SPEC H §3/§9).

use std::path::PathBuf;

use crate::error::{CmdResult, ErrorCode};

use super::logs::LogSink;
use super::{HostState, Inspect, todo_err};

/// One extension entry in the host's `initialize({extensions:[…]})` array. The daemon
/// `mkdir -p`s `storage_directory` and `temp_directory` before launch (SPEC H §1/§8).
#[derive(Debug, Clone)]
pub struct ExtensionSpec {
    pub name: String,
    pub path: PathBuf,
    pub storage_directory: PathBuf,
    pub temp_directory: PathBuf,
}

/// Everything needed to launch (or relaunch) the host (SPEC D §4).
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Live's bundled node (`…/ExtensionHost/node`) — preferred for native-ABI match
    /// (SPEC H §0). PATH `node` is only a fallback when the bundled binary is missing.
    pub eh_node: PathBuf,
    /// The host module (`…/ExtensionHost/ExtensionHostNodeModule.node`).
    pub eh_mod: PathBuf,
    /// The host-compatible (already pre-filtered) extension set.
    pub extensions: Vec<ExtensionSpec>,
    /// `--inspect[=host:port]` passthrough to the host's node (§7).
    pub inspect: Option<Inspect>,
    /// The test seam (SPEC D §6): when set, the daemon runs this command verbatim
    /// instead of `eh_node -e require(eh_mod).initialize(...)`, so hermetic
    /// daemon-lifecycle tests point at the FakeHost. Fed from `RACKABEL_HOST_CMD`.
    pub host_cmd_override: Option<Vec<String>>,
}

/// The outcome of a (whole-host) reload.
#[derive(Debug, Clone)]
pub struct ReloadOutcome {
    pub ms: u64,
    pub loaded: Vec<String>,
    pub failed: Vec<(String, String)>,
    pub skipped: Vec<(String, String)>,
}

/// What the supervisor should do after the child exits unexpectedly (SPEC H §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RespawnDecision {
    /// Respawn after a backoff.
    Respawn { after_ms: u64 },
    /// Foreground/TTY: ask the user `host crashed — reload? [Y/n]`.
    PromptTty,
    /// Past the bounded crash window: give up, mark `CrashLooping`, wait for an
    /// explicit reload/start.
    GaveUpCrashLooping,
}

/// A handle to the managed host process. STUB — daemon-core owns the real body.
pub struct Host {
    state: HostState,
}

impl Host {
    /// Launch the host (`mkdir -p` storage/temp, spawn into the group, wait for the
    /// connected marker). STUB.
    pub fn launch(_cfg: HostConfig, _sink: LogSink) -> CmdResult<Host> {
        todo_err(ErrorCode::HostLaunchFailed, "launching the Extension Host")
    }

    /// Whole-host reload: SIGTERM the old process, wait for exit, spawn a fresh one
    /// with the re-scanned list, wait for the greeting. STUB.
    pub fn reload(&mut self, _cfg: HostConfig) -> CmdResult<ReloadOutcome> {
        todo_err(
            ErrorCode::ReloadActivateFailed,
            "reloading the Extension Host",
        )
    }

    /// The current host state.
    pub fn state(&self) -> HostState {
        self.state.clone()
    }

    /// The last measured reload duration (ms), if any. STUB.
    pub fn last_reload_ms(&self) -> Option<u64> {
        None
    }

    /// The rolling p50 reload duration (ms), if enough samples. STUB.
    pub fn reload_p50_ms(&self) -> Option<u64> {
        None
    }

    /// Stop the host: `killpg(group, SIGTERM)` then `SIGKILL` after a short grace
    /// (SPEC H §3). STUB.
    pub fn stop(&mut self) {
        self.state = HostState::Stopped;
    }

    /// The host's process-group id (negate for `killpg`). STUB.
    #[cfg(unix)]
    pub fn pgid(&self) -> nix::unistd::Pid {
        nix::unistd::Pid::from_raw(0)
    }

    /// Decide what to do after the child exits unexpectedly (SPEC H §9). STUB.
    pub fn on_child_exit(&mut self, _code: Option<i32>, _tty: bool) -> RespawnDecision {
        RespawnDecision::GaveUpCrashLooping
    }
}
