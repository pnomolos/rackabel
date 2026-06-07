//! The dev-host log sink (DESIGN §3.4, SPEC H §5).
//!
//! OWNED BY THE LOGS AGENT. The foundation froze the `LogSink`/`LogLine`/`LineKind`
//! surface (SPEC D §4). The LOGS agent owns the rich behavior: the version-resolved
//! `ExtensionHost.txt` tail+parser, per-`[<ext>]:` attribution, framed
//! lifecycle/liveness/activate-failure events, and dist-sourcemap mapping.
//!
//! DAEMON-CORE NOTE: the host child lifecycle (`host.rs`) needs a *working* sink to
//! capture stdout/stderr and to broadcast lifecycle events to `dev logs --follow`
//! subscribers — a panicking stub would make the daemon unrunnable. So this file lands
//! a minimal-but-real fan-out: a session log file plus a set of broadcast senders. The
//! parser/attribution/sourcemap richness is the LOGS agent's to layer on top without
//! changing this frozen surface.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};

use super::{DevEvent, SourceLoc, log_dir};

/// The level of a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    /// The lowercase wire string (matches the IPC `LogLine.level` field).
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
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

impl LineKind {
    /// The lowercase wire string (matches the IPC `LogLine.kind` field).
    pub fn as_str(self) -> &'static str {
        match self {
            LineKind::Lifecycle => "lifecycle",
            LineKind::Console => "console",
            LineKind::Host => "host",
        }
    }
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

/// The shared fan-out behind a [`LogSink`]: the session log file + the live subscriber
/// senders for `dev logs --follow`.
struct Inner {
    /// The session log file (best-effort; `None` if it couldn't be opened).
    file: Option<File>,
    /// Live `dev logs --follow` subscribers; dead receivers are pruned on send.
    subscribers: Vec<mpsc::Sender<LogLine>>,
}

/// A cloneable handle to the per-host log fan-out.
#[derive(Clone)]
pub struct LogSink {
    inner: Arc<Mutex<Inner>>,
}

impl LogSink {
    /// Open the sink for a host session (creates `~/.rackabel/logs/_session/<session>.log`).
    /// Per-extension separation for framed events is the LOGS agent's refinement; the
    /// daemon-core path needs the session capture + broadcast to work today.
    pub fn open(ctx: &Ctx, session: &str) -> CmdResult<LogSink> {
        let dir = log_dir(ctx).join("_session");
        std::fs::create_dir_all(&dir).map_err(|e| {
            RkError::of(
                ErrorCode::DaemonStartFailed,
                "could not create the dev-host log directory",
                "check write permissions on ~/.rackabel and retry",
            )
            .at(dir.display().to_string())
            .raw(e.into())
        })?;
        let path = dir.join(format!("{session}.log"));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        Ok(LogSink {
            inner: Arc::new(Mutex::new(Inner {
                file,
                subscribers: Vec::new(),
            })),
        })
    }

    /// A sink backed by a session log under `dir` (test helper — no Ctx needed).
    #[cfg(test)]
    pub fn open_for_test(dir: &Path) -> LogSink {
        let path = dir.join("session.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        LogSink {
            inner: Arc::new(Mutex::new(Inner {
                file,
                subscribers: Vec::new(),
            })),
        }
    }

    /// Feed one raw host stdout/stderr line. Best-effort `[<ext>]:` attribution + level
    /// detection; written to the session file and broadcast to subscribers.
    pub fn host_stdout(&self, line: &str) {
        let level = if line.contains(": error:") || line.contains("error:") {
            Level::Error
        } else {
            Level::Info
        };
        let ext = extract_ext_tag(line);
        let kind = if ext.is_some() {
            LineKind::Console
        } else {
            LineKind::Host
        };
        self.push(LogLine {
            ts_ms: super::host::now_ms(),
            level,
            ext,
            kind,
            text: line.to_string(),
            mapped: None,
        });
    }

    /// Frame a lifecycle event into the log (reliably keyed by extension where the
    /// variant carries an `ext`). Broadcast to subscribers as a `Lifecycle` line.
    pub fn event(&self, ev: &DevEvent) {
        let (ext, text, level) = describe_event(ev);
        self.push(LogLine {
            ts_ms: super::host::now_ms(),
            level,
            ext,
            kind: LineKind::Lifecycle,
            text,
            mapped: None,
        });
    }

    /// Spawn a tail thread over the version-resolved `ExtensionHost.txt`. The LOGS agent
    /// owns the parser; the daemon-core path captures the host's own stdout/stderr
    /// directly (the reliable per-session stream), so this is a no-op stub for now.
    pub fn tail_exthost(&self, _path: &Path) {}

    /// A receiver for a `dev logs --follow` stream (registered as a live subscriber).
    pub fn subscribe(&self) -> mpsc::Receiver<LogLine> {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut inner) = self.inner.lock() {
            inner.subscribers.push(tx);
        }
        rx
    }

    /// Write a line to the session file and broadcast it; prune dead subscribers.
    fn push(&self, line: LogLine) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if let Some(f) = inner.file.as_mut() {
            let _ = writeln!(f, "{}", line.text);
            let _ = f.flush();
        }
        inner.subscribers.retain(|tx| tx.send(line.clone()).is_ok());
    }
}

/// Extract the `[<ext>]:` tag from a host line (`info: [Foo]: …` → `Foo`), if present.
fn extract_ext_tag(line: &str) -> Option<String> {
    let open = line.find('[')?;
    let close = line[open + 1..].find(']')? + open + 1;
    let name = &line[open + 1..close];
    if name.is_empty() || name.contains(' ') {
        None
    } else {
        Some(name.to_string())
    }
}

/// Render a `DevEvent` to `(ext, text, level)` for the log line.
fn describe_event(ev: &DevEvent) -> (Option<String>, String, Level) {
    match ev {
        DevEvent::BuildStarted { ext } => {
            (Some(ext.clone()), format!("building {ext}…"), Level::Info)
        }
        DevEvent::BuildOk { ext, ms, .. } => (
            Some(ext.clone()),
            format!("built {ext} ({ms}ms)"),
            Level::Info,
        ),
        DevEvent::BuildFailed { ext, message } => (
            Some(ext.clone()),
            format!("build failed for {ext}: {message}"),
            Level::Error,
        ),
        DevEvent::Deployed { ext, ms } => (
            Some(ext.clone()),
            format!("deployed {ext} ({ms}ms)"),
            Level::Info,
        ),
        DevEvent::ReloadStarted { trigger } => (
            None,
            match trigger {
                Some(t) => format!("reloading (triggered by {t})…"),
                None => "reloading…".to_string(),
            },
            Level::Info,
        ),
        DevEvent::ReloadDone { ms, reloaded, .. } => (
            None,
            format!("reloaded {} extension(s) ({ms}ms)", reloaded.len()),
            Level::Info,
        ),
        DevEvent::Liveness { ext, summary } => (Some(ext.clone()), summary.clone(), Level::Info),
        DevEvent::ActivateFailed { ext, message, .. } => (
            Some(ext.clone()),
            format!("{ext} failed to activate: {message}"),
            Level::Error,
        ),
        DevEvent::HostCrashed { code } => {
            (None, format!("host crashed (code {code:?})"), Level::Error)
        }
        DevEvent::HostCrashLooping { attempts } => (
            None,
            format!("host crash-looping after {attempts} attempts"),
            Level::Error,
        ),
    }
}

/// Map a `dist/extension.js:line:col` back to its source via the dist sourcemap. STUB
/// — the LOGS agent implements this against the dev build's emitted sourcemap.
pub fn map_through_sourcemap(_dist_js: &Path, _line: u32, _col: u32) -> Option<SourceLoc> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ext_tag() {
        assert_eq!(
            extract_ext_tag("ts: info: [Foo]: hello"),
            Some("Foo".to_string())
        );
        assert_eq!(extract_ext_tag("ts: info: plain line"), None);
        // A bracketed phrase with a space is not a tag.
        assert_eq!(extract_ext_tag("ts: info: [not a tag]"), None);
    }

    #[test]
    fn subscriber_receives_pushed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let sink = LogSink::open_for_test(dir.path());
        let rx = sink.subscribe();
        sink.host_stdout("ts: info: [Foo]: hi");
        let line = rx.recv().unwrap();
        assert_eq!(line.ext.as_deref(), Some("Foo"));
        assert_eq!(line.kind, LineKind::Console);
    }

    #[test]
    fn event_is_keyed_by_ext() {
        let dir = tempfile::tempdir().unwrap();
        let sink = LogSink::open_for_test(dir.path());
        let rx = sink.subscribe();
        sink.event(&DevEvent::Liveness {
            ext: "harmonic-lens".into(),
            summary: "harmonic-lens v0.3 active".into(),
        });
        let line = rx.recv().unwrap();
        assert_eq!(line.ext.as_deref(), Some("harmonic-lens"));
        assert_eq!(line.kind, LineKind::Lifecycle);
    }
}
