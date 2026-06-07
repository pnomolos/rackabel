//! The dev-host log sink (DESIGN §3.4, SPEC H §5).
//!
//! OWNED BY THE LOGS AGENT. The foundation freezes the `LogSink`/`LogLine`/`LineKind`
//! surface (SPEC D §4) and lands compiling stubs. The logs agent fills the fan-out:
//! per-name file writers under `~/.rackabel/logs/<name>/<session>.log`, a broadcast
//! channel for `dev logs --follow`, framed lifecycle/liveness/activate-failure events
//! (reliably keyed by extension name) plus best-effort `console.*` attribution from
//! the host's `[<ext>]:`-tagged stdout, the `ExtensionHost.txt` tail+parser (SPEC H
//! §5 line format), and dist-sourcemap mapping for activate-failure frames.

use std::path::Path;
use std::sync::mpsc;

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode};

use super::{DevEvent, SourceLoc, todo_err};

/// The level of a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// What produced a log line (drives rendering + filtering, §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// A framed lifecycle/liveness event (reliably keyed by extension).
    Lifecycle,
    /// Best-effort `console.*` from the host (`[<ext>]:`-tagged when attributable).
    Console,
    /// Raw host/Node output not attributable to an extension.
    Host,
}

/// One normalized log line surfaced to `dev logs` / the watch UI.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub ts_ms: u64,
    pub level: Level,
    pub ext: Option<String>,
    pub kind: LineKind,
    pub text: String,
    pub mapped: Option<SourceLoc>,
}

/// A cloneable handle to the per-host log fan-out. STUB — the logs agent owns the
/// real Arc'd fan-out.
#[derive(Clone)]
pub struct LogSink {
    _private: (),
}

impl LogSink {
    /// Open the sink for a host session (creates `~/.rackabel/logs/…`). STUB.
    pub fn open(_ctx: &Ctx, _session: &str) -> CmdResult<LogSink> {
        todo_err(ErrorCode::NoDaemon, "opening the dev-host log sink")
    }

    /// Feed one raw host stdout/stderr line (best-effort `[<ext>]:` attribution). STUB.
    pub fn host_stdout(&self, _line: &str) {}

    /// Frame a lifecycle event into the per-extension log. STUB.
    pub fn event(&self, _ev: &DevEvent) {}

    /// Spawn a tail thread over the version-resolved `ExtensionHost.txt`. STUB.
    pub fn tail_exthost(&self, _path: &Path) {}

    /// A receiver for a `dev logs --follow` stream. STUB returns a dropped channel.
    pub fn subscribe(&self) -> mpsc::Receiver<LogLine> {
        let (_tx, rx) = mpsc::channel();
        rx
    }
}

/// Map a `dist/extension.js:line:col` back to its source via the dist sourcemap. STUB
/// — the logs agent implements this against the dev build's emitted sourcemap.
pub fn map_through_sourcemap(_dist_js: &Path, _line: u32, _col: u32) -> Option<SourceLoc> {
    None
}
