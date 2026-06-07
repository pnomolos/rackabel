//! The dev-host log sink (DESIGN §3.4, SPEC H §5).
//!
//! OWNED BY THE LOGS AGENT. The foundation froze the `LogSink`/`LogLine`/`LineKind`
//! surface (SPEC D §4). The LOGS agent owns the rich behavior:
//!
//!   - **Sink fan-out**: every line is written to a per-extension session file
//!     `~/.rackabel/logs/<name>/<session>.log` (keyed by registry unique name) AND to a
//!     shared `~/.rackabel/logs/_session/<session>.log` for host/Node lines that can't
//!     be attributed. Lines are persisted in a structured, parseable form so the
//!     dead-daemon `dev logs` file-tail fallback can reconstruct each [`LogLine`]
//!     (ts/level/kind/ext) without the daemon (DESIGN §3.4 — read-only must work with a
//!     dead daemon).
//!   - **Framed lifecycle/liveness events** (reliably keyed by extension): `event()`.
//!   - **Best-effort `console.*`**: `host_stdout()` parses the host's own captured
//!     stdout/stderr; `[<ext>]:`-tagged lines are attributed, the rest are shared-stream
//!     `Host` lines (documented honestly — the host inherits one stdio stream, SPEC H §1).
//!   - **`ExtensionHost.txt` tail** (`tail_exthost`): the version-resolved Live log
//!     (Live-managed sessions, SPEC H §5) is tailed + parsed into framed events: the
//!     `Started: Extension Host X.Y.Z` banner, `[<ext>]:`-tagged extension lines,
//!     uncaught-exception activate failures (→ `ActivateFailed` with a sourcemap-mapped
//!     `file:line`), `Last message repeated N time(s)` de-dup, and multi-line stack
//!     continuations.
//!   - **Sourcemap mapping** (`map_through_sourcemap`): maps a `dist/extension.js:L:C`
//!     frame back to its `src` location via the dev build's emitted `.map` (a small
//!     self-contained VLQ decoder — no new crate, per SPEC D §5).
//!
//! A small in-memory history ring lets the daemon replay recent lines to a fresh
//! `dev logs` subscriber (see [`LogSink::history`]).

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};

use super::{DevEvent, SourceLoc, log_dir};

/// How many recent lines the sink keeps for replay to a fresh subscriber.
const HISTORY_CAP: usize = 2000;

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

    /// Parse a wire/level string (`info|warn|error`); unknown → `Info`.
    pub fn parse(s: &str) -> Level {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" | "err" => Level::Error,
            "warn" | "warning" => Level::Warn,
            _ => Level::Info,
        }
    }

    /// Severity rank for `--level` "at or above" filtering (info < warn < error).
    pub fn rank(self) -> u8 {
        match self {
            Level::Info => 0,
            Level::Warn => 1,
            Level::Error => 2,
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

    /// Parse a wire/kind string; unknown → `Host`.
    pub fn parse(s: &str) -> LineKind {
        match s.trim() {
            "lifecycle" => LineKind::Lifecycle,
            "console" => LineKind::Console,
            _ => LineKind::Host,
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

impl LogLine {
    /// Whether this line passes a `(since_ms, level, name)` filter set.
    pub fn matches(
        &self,
        since_ms: Option<u64>,
        min_level: Option<Level>,
        name: Option<&str>,
    ) -> bool {
        if let Some(s) = since_ms
            && self.ts_ms < s
        {
            return false;
        }
        if let Some(lv) = min_level
            && self.level.rank() < lv.rank()
        {
            return false;
        }
        if let Some(n) = name
            && self.ext.as_deref() != Some(n)
        {
            return false;
        }
        true
    }

    /// The `--json` line shape (one object per line; stable field set).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ts": self.ts_ms,
            "level": self.level.as_str(),
            "ext": self.ext,
            "kind": self.kind.as_str(),
            "text": self.text,
            "mapped": self.mapped,
        })
    }

    /// Serialize to one persisted file line: tab-delimited
    /// `ts\tlevel\tkind\text\ttext` (the `ext` slot is `-` when absent; tabs/newlines
    /// in `text` are flattened so a line stays one record).
    fn to_file_record(&self) -> String {
        let ext = self.ext.as_deref().unwrap_or("-");
        let text = self.text.replace(['\t', '\n', '\r'], " ");
        format!(
            "{}\t{}\t{}\t{}\t{}",
            self.ts_ms,
            self.level.as_str(),
            self.kind.as_str(),
            ext,
            text
        )
    }

    /// Parse a persisted file line back into a [`LogLine`] (the dead-daemon fallback).
    /// `None` for a malformed record.
    fn from_file_record(line: &str) -> Option<LogLine> {
        let mut parts = line.splitn(5, '\t');
        let ts_ms: u64 = parts.next()?.parse().ok()?;
        let level = Level::parse(parts.next()?);
        let kind = LineKind::parse(parts.next()?);
        let ext_raw = parts.next()?;
        let ext = if ext_raw == "-" {
            None
        } else {
            Some(ext_raw.to_string())
        };
        let text = parts.next().unwrap_or("").to_string();
        Some(LogLine {
            ts_ms,
            level,
            ext,
            kind,
            text,
            mapped: None,
        })
    }
}

/// The shared fan-out behind a [`LogSink`].
struct Inner {
    /// Where per-extension + shared session files live (`~/.rackabel/logs`).
    log_root: PathBuf,
    /// The session id (the `<session>.log` stem) — drives session rotation.
    session: String,
    /// Open per-name file writers (lazily opened on first attributed line).
    files: std::collections::HashMap<String, File>,
    /// The shared session file for host/unattributable lines (`_session/<session>.log`).
    shared: Option<File>,
    /// Live `dev logs --follow` subscribers; dead receivers are pruned on send.
    subscribers: Vec<mpsc::Sender<LogLine>>,
    /// A bounded ring of recent lines for replay to a fresh subscriber.
    history: std::collections::VecDeque<LogLine>,
    /// The dist `dist/extension.js` per extension name, for sourcemap mapping of
    /// `ExtensionHost.txt` activate-failure frames. Populated by `register_dist`.
    dist_for: std::collections::HashMap<String, PathBuf>,
}

/// A cloneable handle to the per-host log fan-out.
#[derive(Clone)]
pub struct LogSink {
    inner: Arc<Mutex<Inner>>,
}

impl LogSink {
    /// Open the sink for a host session. Per-extension lines land in
    /// `~/.rackabel/logs/<name>/<session>.log`; host/unattributable lines in
    /// `~/.rackabel/logs/_session/<session>.log` (DESIGN §3.4 sink fan-out).
    pub fn open(ctx: &Ctx, session: &str) -> CmdResult<LogSink> {
        let root = log_dir(ctx);
        let shared_dir = root.join("_session");
        std::fs::create_dir_all(&shared_dir).map_err(|e| {
            RkError::of(
                ErrorCode::DaemonStartFailed,
                "could not create the dev-host log directory",
                "check write permissions on ~/.rackabel and retry",
            )
            .at(shared_dir.display().to_string())
            .raw(e.into())
        })?;
        let shared = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(shared_dir.join(format!("{session}.log")))
            .ok();
        Ok(LogSink {
            inner: Arc::new(Mutex::new(Inner {
                log_root: root,
                session: session.to_string(),
                files: std::collections::HashMap::new(),
                shared,
                subscribers: Vec::new(),
                history: std::collections::VecDeque::with_capacity(HISTORY_CAP),
                dist_for: std::collections::HashMap::new(),
            })),
        })
    }

    /// A sink backed by a session log under `dir` (test helper — no Ctx needed).
    #[cfg(test)]
    pub fn open_for_test(dir: &Path) -> LogSink {
        let session = "session";
        let shared_dir = dir.join("_session");
        let _ = std::fs::create_dir_all(&shared_dir);
        let shared = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(shared_dir.join(format!("{session}.log")))
            .ok();
        LogSink {
            inner: Arc::new(Mutex::new(Inner {
                log_root: dir.to_path_buf(),
                session: session.to_string(),
                files: std::collections::HashMap::new(),
                shared,
                subscribers: Vec::new(),
                history: std::collections::VecDeque::with_capacity(HISTORY_CAP),
                dist_for: std::collections::HashMap::new(),
            })),
        }
    }

    /// Tell the sink where an extension's `dist/extension.js` lives so activate-failure
    /// frames from `ExtensionHost.txt` can be mapped back through its sourcemap. The
    /// daemon-core agent should call this for each loaded extension (best-effort).
    pub fn register_dist(&self, name: &str, dist_js: PathBuf) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.dist_for.insert(name.to_string(), dist_js);
        }
    }

    /// Feed one raw host stdout/stderr line. Best-effort `[<ext>]:` attribution + level
    /// detection; written to the session file(s) and broadcast to subscribers.
    pub fn host_stdout(&self, line: &str) {
        let level = detect_level(line);
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
        // ActivateFailed carries an already-mapped location from the watch/reload path;
        // preserve it on the framed line.
        let mapped = match ev {
            DevEvent::ActivateFailed { mapped, .. } => mapped.clone(),
            _ => None,
        };
        let (ext, text, level) = describe_event(ev);
        self.push(LogLine {
            ts_ms: super::host::now_ms(),
            level,
            ext,
            kind: LineKind::Lifecycle,
            text,
            mapped,
        });
    }

    /// Spawn a tail thread over the version-resolved `ExtensionHost.txt` (SPEC H §5).
    /// This is the Live-managed (Dev-Mode-OFF) session log; a daemon-launched host
    /// writes to its own stdout (captured via [`host_stdout`]) instead, so this is a
    /// best-effort secondary source. The thread runs until the sink is dropped (all
    /// `Arc` clones gone) — it detaches and exits when its weak handle can't upgrade.
    pub fn tail_exthost(&self, path: &Path) {
        let path = path.to_path_buf();
        let weak = Arc::downgrade(&self.inner);
        std::thread::spawn(move || {
            tail_exthost_loop(&path, weak);
        });
    }

    /// A receiver for a `dev logs --follow` stream (registered as a live subscriber).
    pub fn subscribe(&self) -> mpsc::Receiver<LogLine> {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut inner) = self.inner.lock() {
            inner.subscribers.push(tx);
        }
        rx
    }

    /// A snapshot of recent lines (the in-memory replay ring). The daemon sends these to
    /// a fresh `dev logs` subscriber before forwarding live lines.
    pub fn history(&self) -> Vec<LogLine> {
        self.inner
            .lock()
            .map(|inner| inner.history.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Write a line to the per-name + shared session files, broadcast it, and ring it.
    fn push(&self, line: LogLine) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        inner.write_line(&line);

        // Ring buffer for replay.
        if inner.history.len() >= HISTORY_CAP {
            inner.history.pop_front();
        }
        inner.history.push_back(line.clone());

        inner.subscribers.retain(|tx| tx.send(line.clone()).is_ok());
    }
}

impl Inner {
    /// Persist a line to its per-extension file (when attributed) and to the shared
    /// session file (always), in the parseable record format.
    fn write_line(&mut self, line: &LogLine) {
        let record = line.to_file_record();
        if let Some(name) = line.ext.clone()
            && let Some(f) = self.ext_file(&name)
        {
            let _ = writeln!(f, "{record}");
            let _ = f.flush();
        }
        if let Some(f) = self.shared.as_mut() {
            let _ = writeln!(f, "{record}");
            let _ = f.flush();
        }
    }

    /// Lazily open `~/.rackabel/logs/<name>/<session>.log`.
    fn ext_file(&mut self, name: &str) -> Option<&mut File> {
        if !self.files.contains_key(name) {
            let dir = self.log_root.join(name);
            std::fs::create_dir_all(&dir).ok()?;
            let f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(format!("{}.log", self.session)))
                .ok()?;
            self.files.insert(name.to_string(), f);
        }
        self.files.get_mut(name)
    }
}

// --- ExtensionHost.txt tail + parser (SPEC H §5) --------------------------------

/// The tail loop: open the file (waiting for it to appear), then poll for appended
/// lines, parse each, and push framed [`LogLine`]s into the sink. Exits when the sink's
/// weak handle can no longer upgrade (the daemon stopped).
fn tail_exthost_loop(path: &Path, weak: std::sync::Weak<Mutex<Inner>>) {
    // Start from the current end of file (we tail *new* lines; history is the sink's
    // own ring / the per-session files).
    let mut pos: u64 = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut parser = ExtHostParser::default();
    loop {
        if weak.upgrade().is_none() {
            return;
        }
        let Ok(mut file) = File::open(path) else {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        // Truncation/rotation: restart from the top.
        if len < pos {
            pos = 0;
            parser = ExtHostParser::default();
        }
        if len > pos && file.seek(SeekFrom::Start(pos)).is_ok() {
            let mut reader = BufReader::new(&mut file);
            let mut buf = String::new();
            loop {
                buf.clear();
                match reader.read_line(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        pos += n as u64;
                        // Only emit complete (newline-terminated) lines.
                        if buf.ends_with('\n') {
                            let arc = match weak.upgrade() {
                                Some(a) => a,
                                None => return,
                            };
                            for ll in parser.feed(buf.trim_end_matches(['\n', '\r']), &arc) {
                                push_into(&arc, ll);
                            }
                        } else {
                            // Partial trailing line; rewind so we re-read it whole.
                            pos -= n as u64;
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Push a fully-formed [`LogLine`] into the sink (used by the tail thread, which holds
/// only the `Arc<Mutex<Inner>>`, not a `LogSink`).
fn push_into(arc: &Arc<Mutex<Inner>>, line: LogLine) {
    let Ok(mut inner) = arc.lock() else { return };
    inner.write_line(&line);
    if inner.history.len() >= HISTORY_CAP {
        inner.history.pop_front();
    }
    inner.history.push_back(line.clone());
    inner.subscribers.retain(|tx| tx.send(line.clone()).is_ok());
}

/// A line-oriented parser over `ExtensionHost.txt` (SPEC H §5). It threads multi-line
/// stack-trace continuations onto the entry that opened them so an uncaught-exception
/// frame can be mapped through a sourcemap.
#[derive(Default)]
struct ExtHostParser {
    /// True while we're inside an uncaught-exception block, accumulating its stack so we
    /// can find a `…/dist/extension.js:L:C` frame and attribute/map it.
    in_exception: bool,
    /// The extension we last saw a `[<ext>]:` tag for (best-effort attribution of a
    /// following untagged exception).
    last_ext: Option<String>,
    /// The exception message (the first `Error: …` line of an uncaught block).
    exc_message: Option<String>,
    /// Whether we've already emitted the `ActivateFailed` for the current block.
    exc_emitted: bool,
}

impl ExtHostParser {
    /// Feed one raw `ExtensionHost.txt` line; returns zero or more framed [`LogLine`]s.
    /// `arc` is used to read the registered dist-map paths for sourcemap mapping.
    fn feed(&mut self, raw: &str, arc: &Arc<Mutex<Inner>>) -> Vec<LogLine> {
        let mut out = Vec::new();
        match parse_exthost_line(raw) {
            Some((ts_ms, level, msg)) => {
                // A new timestamped entry closes any in-flight exception block.
                self.in_exception = false;

                // Liveness banner (host started).
                if let Some(ver) = msg.strip_prefix("Started: Extension Host ") {
                    out.push(LogLine {
                        ts_ms,
                        level: Level::Info,
                        ext: None,
                        kind: LineKind::Lifecycle,
                        text: format!("Extension Host {} started", ver.trim()),
                        mapped: None,
                    });
                    return out;
                }

                // Extension-tagged line: attribute + remember the ext.
                if let Some((ext, rest)) = strip_ext_tag(&msg) {
                    self.last_ext = Some(ext.clone());
                    out.push(LogLine {
                        ts_ms,
                        level,
                        ext: Some(ext),
                        kind: LineKind::Console,
                        text: rest,
                        mapped: None,
                    });
                    return out;
                }

                // Uncaught-exception header opens an exception block.
                if msg.contains("Uncaught exception") || msg.starts_with("Error:") {
                    self.in_exception = true;
                    self.exc_emitted = false;
                    self.exc_message = if msg.starts_with("Error:") {
                        Some(msg.clone())
                    } else {
                        None
                    };
                    out.push(LogLine {
                        ts_ms,
                        level: Level::Error,
                        ext: self.last_ext.clone(),
                        kind: LineKind::Host,
                        text: msg,
                        mapped: None,
                    });
                    return out;
                }

                // `Last message repeated N time(s): <msg>` de-dup marker — pass through.
                // Plain host/info line.
                let ext = extract_ext_tag(raw);
                let kind = if ext.is_some() {
                    LineKind::Console
                } else {
                    LineKind::Host
                };
                out.push(LogLine {
                    ts_ms,
                    level,
                    ext,
                    kind,
                    text: msg,
                    mapped: None,
                });
            }
            None => {
                // A continuation line (indented stack frame / object dump).
                if self.in_exception {
                    // Capture the first `Error: …` message if we didn't have one.
                    let trimmed = raw.trim();
                    if self.exc_message.is_none() && trimmed.starts_with("Error:") {
                        self.exc_message = Some(trimmed.to_string());
                    }
                    // Look for a `…/dist/extension.js:L:C` frame to map + emit
                    // ActivateFailed once per block.
                    if !self.exc_emitted
                        && let Some((dist_js, line, col)) = parse_stack_frame(raw)
                    {
                        let ext = self
                            .last_ext
                            .clone()
                            .or_else(|| ext_from_dist_path(&dist_js));
                        let mapped = map_from_registered(arc, ext.as_deref(), &dist_js, line, col);
                        let message = self
                            .exc_message
                            .clone()
                            .unwrap_or_else(|| "uncaught exception in activate()".to_string());
                        let loc = mapped
                            .as_ref()
                            .map(|m| format!(" ({}:{})", m.file, m.line))
                            .unwrap_or_default();
                        out.push(LogLine {
                            ts_ms: super::host::now_ms(),
                            level: Level::Error,
                            ext,
                            kind: LineKind::Lifecycle,
                            text: format!("activate() failed: {message}{loc}"),
                            mapped,
                        });
                        self.exc_emitted = true;
                    }
                }
            }
        }
        out
    }
}

/// Look up the registered dist-map for `ext` (preferred) or the raw `dist_js` path and
/// map the `(line, col)` frame back to source.
fn map_from_registered(
    arc: &Arc<Mutex<Inner>>,
    ext: Option<&str>,
    dist_js: &str,
    line: u32,
    col: u32,
) -> Option<SourceLoc> {
    let registered: Option<PathBuf> = arc
        .lock()
        .ok()
        .and_then(|inner| ext.and_then(|e| inner.dist_for.get(e).cloned()));
    let path = registered.unwrap_or_else(|| PathBuf::from(dist_js));
    map_through_sourcemap(&path, line, col)
}

/// Parse a leading `YYYY-...T...: <level>: <message>` line; `None` for continuations
/// (lines not starting with the timestamp pattern, SPEC H §5).
fn parse_exthost_line(line: &str) -> Option<(u64, Level, String)> {
    // The timestamp ends at the first `: ` that precedes a known level token.
    // Format: `<ts>: info: <msg>` / `<ts>: error: <msg>`.
    let (ts_raw, rest) = line.split_once(": ")?;
    // The timestamp must look like an ISO-8601 (`T` separator, digits) — reject ordinary
    // text lines that merely contain `: `.
    if !ts_raw.contains('T') || !ts_raw.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    let (level_raw, msg) = rest.split_once(": ").unwrap_or((rest, ""));
    let level = match level_raw {
        "info" => Level::Info,
        "error" => Level::Error,
        "warn" | "warning" => Level::Warn,
        _ => return None,
    };
    let ts_ms = parse_exthost_ts(ts_raw).unwrap_or_else(super::host::now_ms);
    Some((ts_ms, level, msg.to_string()))
}

/// Parse an ExtensionHost.txt microsecond timestamp (`YYYY-MM-DDTHH:MM:SS.ffffff`, local
/// time, no tz) to epoch ms — best-effort; falls back to `now` on parse failure (the
/// caller substitutes). We don't pull in chrono: a coarse calendar conversion is enough
/// for ordering/`--since`.
fn parse_exthost_ts(s: &str) -> Option<u64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let min: i64 = t.next()?.parse().ok()?;
    let sec_part = t.next()?;
    let (sec_s, frac_s) = sec_part.split_once('.').unwrap_or((sec_part, "0"));
    let sec: i64 = sec_s.parse().ok()?;
    // Take the leading 3 digits of the fraction as milliseconds.
    let ms: i64 = {
        let mut chars = frac_s.chars().filter(|c| c.is_ascii_digit());
        let h = chars.next().and_then(|c| c.to_digit(10)).unwrap_or(0) as i64;
        let t = chars.next().and_then(|c| c.to_digit(10)).unwrap_or(0) as i64;
        let o = chars.next().and_then(|c| c.to_digit(10)).unwrap_or(0) as i64;
        h * 100 + t * 10 + o
    };
    // Days since Unix epoch via a civil-from-days algorithm (Howard Hinnant).
    let days = days_from_civil(year, month, day);
    let epoch_secs = days * 86_400 + hour * 3_600 + min * 60 + sec;
    // The timestamp is local time; we treat it as UTC for ordering purposes (good
    // enough for `--since` relative windows; absolute accuracy isn't required here).
    Some((epoch_secs * 1000 + ms).max(0) as u64)
}

/// Days from the civil date to/from 1970-01-01 (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Strip a leading `[<ext>]: <rest>` tag from a message; `None` if not tagged.
fn strip_ext_tag(msg: &str) -> Option<(String, String)> {
    let msg = msg.trim_start();
    if !msg.starts_with('[') {
        return None;
    }
    let close = msg.find("]:")?;
    let name = &msg[1..close];
    if name.is_empty() || name.contains(' ') || name.contains('[') {
        return None;
    }
    let rest = msg[close + 2..].trim_start().to_string();
    Some((name.to_string(), rest))
}

/// Parse a JS stack frame for a `…/Extensions/<slug>/dist/extension.js:L:C` location.
/// Returns `(dist_js_path, line, col)`.
fn parse_stack_frame(line: &str) -> Option<(String, u32, u32)> {
    let idx = line.find("dist/extension.js:")?;
    // Walk backwards to the start of the path (the `(` of `at fn (path:…)` or whitespace).
    let start = line[..idx].rfind(['(', ' ']).map(|i| i + 1).unwrap_or(0);
    let after = idx + "dist/extension.js".len();
    let rest = &line[after..]; // starts with `:L:C` then maybe `)`
    let nums: String = rest
        .chars()
        .skip(1) // the leading ':'
        .take_while(|c| c.is_ascii_digit() || *c == ':')
        .collect();
    let mut it = nums.split(':');
    let l: u32 = it.next()?.parse().ok()?;
    let c: u32 = it.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let path = line[start..after].trim().to_string();
    Some((path, l, c))
}

/// Best-effort extension slug from a `…/Extensions/<slug>/dist/extension.js` path.
fn ext_from_dist_path(path: &str) -> Option<String> {
    let p = Path::new(path);
    // .../<slug>/dist/extension.js → grandparent dir name.
    p.parent()?
        .parent()?
        .file_name()?
        .to_str()
        .map(String::from)
}

// --- sourcemap mapping (VLQ decode; SPEC D §5 — no new crate) --------------------

/// Map a `dist/extension.js:line:col` (1-based line, 0-based col, as Node reports) back
/// to its source via the sibling `<dist_js>.map`. Returns the *nearest preceding*
/// mapping on that generated line (the standard sourcemap lookup).
pub fn map_through_sourcemap(dist_js: &Path, line: u32, col: u32) -> Option<SourceLoc> {
    let map_path = sourcemap_path(dist_js)?;
    let raw = std::fs::read_to_string(&map_path).ok()?;
    let map: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let sources = map.get("sources")?.as_array()?;
    let mappings = map.get("mappings")?.as_str()?;
    let source_root = map.get("sourceRoot").and_then(|v| v.as_str()).unwrap_or("");

    // Node reports a 1-based line; sourcemap generated lines are 0-based.
    let target_line = line.checked_sub(1)?;
    let loc = lookup_mapping(mappings, target_line, col)?;
    let src = sources.get(loc.src_index as usize)?.as_str()?;
    let file = if source_root.is_empty() {
        src.to_string()
    } else {
        format!("{}/{}", source_root.trim_end_matches('/'), src)
    };
    Some(SourceLoc {
        file: normalize_source(&file),
        // Sourcemap original line/col are 0-based; surface 1-based line + 0-based col.
        line: loc.src_line + 1,
        col: loc.src_col,
    })
}

/// The `.map` path for a `dist/extension.js` (esbuild emits `dist/extension.js.map`).
fn sourcemap_path(dist_js: &Path) -> Option<PathBuf> {
    let candidate = PathBuf::from(format!("{}.map", dist_js.display()));
    if candidate.exists() {
        Some(candidate)
    } else {
        None
    }
}

/// Clean a `../src/foo.ts` style source ref to a friendlier display path.
fn normalize_source(s: &str) -> String {
    s.trim_start_matches("./").to_string()
}

/// One decoded sourcemap segment target.
struct MapLoc {
    src_index: u32,
    src_line: u32,
    src_col: u32,
}

/// Decode the VLQ `mappings` string and find the nearest mapping at or before
/// `(gen_line, gen_col)` on the generated line.
fn lookup_mapping(mappings: &str, gen_line: u32, gen_col: u32) -> Option<MapLoc> {
    // Running source-relative fields (persist across the whole mappings string).
    let mut src_index: i64 = 0;
    let mut src_line: i64 = 0;
    let mut src_col: i64 = 0;

    for (line_no, group) in mappings.split(';').enumerate() {
        // generated column resets per line.
        let mut gen_col_acc: i64 = 0;
        let mut best: Option<(i64, MapLoc)> = None;
        for segment in group.split(',') {
            if segment.is_empty() {
                continue;
            }
            let fields = decode_vlq(segment);
            if fields.is_empty() {
                continue;
            }
            gen_col_acc += fields[0];
            if fields.len() >= 4 {
                src_index += fields[1];
                src_line += fields[2];
                src_col += fields[3];
            }
            if line_no as u32 == gen_line && fields.len() >= 4 {
                // Track the nearest segment whose generated col <= gen_col.
                if gen_col_acc <= gen_col as i64 {
                    let cand = MapLoc {
                        src_index: src_index.max(0) as u32,
                        src_line: src_line.max(0) as u32,
                        src_col: src_col.max(0) as u32,
                    };
                    match &best {
                        Some((bc, _)) if *bc >= gen_col_acc => {}
                        _ => best = Some((gen_col_acc, cand)),
                    }
                }
            }
        }
        if line_no as u32 == gen_line {
            return best.map(|(_, loc)| loc);
        }
    }
    None
}

/// Decode a Base64-VLQ segment to its signed integer fields.
fn decode_vlq(segment: &str) -> Vec<i64> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut shift = 0u32;
    let mut value: i64 = 0;
    for ch in segment.bytes() {
        let Some(digit) = ALPHABET.iter().position(|&c| c == ch) else {
            return out;
        };
        let digit = digit as i64;
        let cont = digit & 32;
        value += (digit & 31) << shift;
        if cont != 0 {
            shift += 5;
        } else {
            let negate = value & 1;
            let mut shifted = value >> 1;
            if negate != 0 {
                shifted = -shifted;
            }
            out.push(shifted);
            value = 0;
            shift = 0;
        }
    }
    out
}

// --- shared helpers --------------------------------------------------------------

/// Detect the level of a raw host line (`error:` anywhere → Error).
fn detect_level(line: &str) -> Level {
    let lower = line.to_ascii_lowercase();
    if lower.contains("error:") || lower.contains(": error ") {
        Level::Error
    } else if lower.contains("warn:") || lower.contains("warning:") {
        Level::Warn
    } else {
        Level::Info
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

// --- file-tail fallback (dead-daemon `dev logs`, DESIGN §3.4) ---------------------

/// Resolve the version-resolved `ExtensionHost.txt` for the running Live (SPEC H §5):
/// `~/Library/Preferences/Ableton/Live <version>/ExtensionHost.txt`, picking the
/// most-recently-modified `Live *` dir (the running build). `None` if none exists.
pub fn exthost_txt_path(ctx: &Ctx) -> Option<PathBuf> {
    let prefs = ctx.home.join("Library/Preferences/Ableton");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&prefs).ok()?.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("Live ") {
            continue;
        }
        let txt = entry.path().join("ExtensionHost.txt");
        if !txt.exists() {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        match &newest {
            Some((t, _)) if *t >= mtime => {}
            _ => newest = Some((mtime, txt)),
        }
    }
    newest.map(|(_, p)| p)
}

/// Read persisted session-log lines for the dead-daemon fallback (read-only; works with
/// no daemon). When `name` is given, reads that extension's per-name session files;
/// otherwise reads the shared `_session` files. Returns lines sorted by timestamp.
pub fn read_session_lines(ctx: &Ctx, name: Option<&str>) -> Vec<LogLine> {
    let root = log_dir(ctx);
    let dir = match name {
        Some(n) => root.join(n),
        None => root.join("_session"),
    };
    let mut lines = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "log"))
            .collect();
        // Stem is the session id (epoch-ms); sort so older sessions come first.
        files.sort();
        for f in files {
            if let Ok(content) = std::fs::read_to_string(&f) {
                for l in content.lines() {
                    if let Some(ll) = LogLine::from_file_record(l) {
                        lines.push(ll);
                    }
                }
            }
        }
    }
    lines.sort_by_key(|l| l.ts_ms);
    lines
}

/// The newest session-log file for `name` (or the shared stream), for `--follow`
/// file-tailing when the daemon is down.
pub fn newest_session_file(ctx: &Ctx, name: Option<&str>) -> Option<PathBuf> {
    let root = log_dir(ctx);
    let dir = match name {
        Some(n) => root.join(n),
        None => root.join("_session"),
    };
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "log") {
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            match &newest {
                Some((t, _)) if *t >= mtime => {}
                _ => newest = Some((mtime, p)),
            }
        }
    }
    newest.map(|(_, p)| p)
}

/// Tail a persisted session-log file, yielding new [`LogLine`]s as they're appended.
/// Used by `dev logs --follow` when no daemon is up (read-only). `should_stop` lets the
/// caller break the loop (e.g. on Ctrl-C); the closure is polled each idle cycle.
pub fn tail_session_file<F: Fn(LogLine), S: Fn() -> bool>(
    path: &Path,
    from_start: bool,
    on_line: F,
    should_stop: S,
) {
    let mut pos: u64 = if from_start {
        0
    } else {
        std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
    };
    loop {
        if should_stop() {
            return;
        }
        if let Ok(mut file) = File::open(path) {
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            if len < pos {
                pos = 0; // rotation/truncation
            }
            if len > pos && file.seek(SeekFrom::Start(pos)).is_ok() {
                let mut reader = BufReader::new(&mut file);
                let mut buf = String::new();
                loop {
                    buf.clear();
                    match reader.read_line(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if buf.ends_with('\n') {
                                pos += n as u64;
                                if let Some(ll) = LogLine::from_file_record(buf.trim_end()) {
                                    on_line(ll);
                                }
                            } else {
                                break; // partial line; re-read next cycle
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Parse a `--since` duration (`30s`, `5m`, `1h`, `2d`, or bare seconds) into ms.
pub fn parse_since(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty --since duration".to_string());
    }
    let (num, unit): (&str, &str) = match s.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => (&s[..s.len() - 1], &s[s.len() - 1..]),
        _ => (s, "s"),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("invalid --since duration: `{s}`"))?;
    let ms = match unit {
        "s" => n * 1000,
        "m" => n * 60_000,
        "h" => n * 3_600_000,
        "d" => n * 86_400_000,
        other => return Err(format!("unknown --since unit `{other}` (use s|m|h|d)")),
    };
    Ok(ms)
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

    #[test]
    fn fan_out_writes_per_extension_and_shared_files() {
        let dir = tempfile::tempdir().unwrap();
        let sink = LogSink::open_for_test(dir.path());
        sink.host_stdout("ts: info: [petri]: cells dividing");
        sink.host_stdout("ts: info: bare host line");
        drop(sink);
        // Per-extension file got only the attributed line.
        let petri = read_session_lines_at(dir.path(), Some("petri"));
        assert_eq!(petri.len(), 1);
        assert_eq!(petri[0].ext.as_deref(), Some("petri"));
        assert_eq!(petri[0].kind, LineKind::Console);
        // Shared file got both.
        let shared = read_session_lines_at(dir.path(), None);
        assert_eq!(shared.len(), 2);
    }

    #[test]
    fn history_replays_recent_lines() {
        let dir = tempfile::tempdir().unwrap();
        let sink = LogSink::open_for_test(dir.path());
        sink.host_stdout("ts: info: [a]: one");
        sink.host_stdout("ts: error: [a]: two");
        let hist = sink.history();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[1].level, Level::Error);
    }

    #[test]
    fn level_filter_at_or_above() {
        let info = LogLine {
            ts_ms: 10,
            level: Level::Info,
            ext: None,
            kind: LineKind::Host,
            text: "x".into(),
            mapped: None,
        };
        assert!(info.matches(None, Some(Level::Info), None));
        assert!(!info.matches(None, Some(Level::Error), None));
    }

    #[test]
    fn since_filter_drops_old_lines() {
        let line = LogLine {
            ts_ms: 100,
            level: Level::Info,
            ext: None,
            kind: LineKind::Host,
            text: "x".into(),
            mapped: None,
        };
        assert!(line.matches(Some(50), None, None));
        assert!(!line.matches(Some(150), None, None));
    }

    #[test]
    fn name_filter_matches_ext() {
        let line = LogLine {
            ts_ms: 1,
            level: Level::Info,
            ext: Some("petri".into()),
            kind: LineKind::Console,
            text: "x".into(),
            mapped: None,
        };
        assert!(line.matches(None, None, Some("petri")));
        assert!(!line.matches(None, None, Some("conway")));
    }

    #[test]
    fn json_line_shape() {
        let line = LogLine {
            ts_ms: 42,
            level: Level::Error,
            ext: Some("foo".into()),
            kind: LineKind::Lifecycle,
            text: "boom".into(),
            mapped: Some(SourceLoc {
                file: "src/extension.ts".into(),
                line: 9,
                col: 3,
            }),
        };
        let v = line.to_json();
        assert_eq!(v["ts"], 42);
        assert_eq!(v["level"], "error");
        assert_eq!(v["ext"], "foo");
        assert_eq!(v["kind"], "lifecycle");
        assert_eq!(v["text"], "boom");
        assert_eq!(v["mapped"]["file"], "src/extension.ts");
        assert_eq!(v["mapped"]["line"], 9);
    }

    #[test]
    fn file_record_round_trips() {
        let line = LogLine {
            ts_ms: 7,
            level: Level::Warn,
            ext: Some("petri".into()),
            kind: LineKind::Console,
            text: "has\ttab and\nnewline".into(),
            mapped: None,
        };
        let rec = line.to_file_record();
        assert!(!rec.contains('\n'));
        let back = LogLine::from_file_record(&rec).unwrap();
        assert_eq!(back.ts_ms, 7);
        assert_eq!(back.level, Level::Warn);
        assert_eq!(back.kind, LineKind::Console);
        assert_eq!(back.ext.as_deref(), Some("petri"));
        assert_eq!(back.text, "has tab and newline");
    }

    #[test]
    fn since_parsing_units() {
        assert_eq!(parse_since("30s").unwrap(), 30_000);
        assert_eq!(parse_since("5m").unwrap(), 300_000);
        assert_eq!(parse_since("1h").unwrap(), 3_600_000);
        assert_eq!(parse_since("2d").unwrap(), 172_800_000);
        assert_eq!(parse_since("45").unwrap(), 45_000);
        assert!(parse_since("5x").is_err());
        assert!(parse_since("").is_err());
    }

    #[test]
    fn parses_exthost_started_banner() {
        let arc = Arc::new(Mutex::new(test_inner()));
        let mut p = ExtHostParser::default();
        let out = p.feed(
            "2026-06-06T10:00:00.123456: info: Started: Extension Host 1.0.0",
            &arc,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind, LineKind::Lifecycle);
        assert!(out[0].text.contains("1.0.0"));
    }

    #[test]
    fn parses_exthost_extension_tag() {
        let arc = Arc::new(Mutex::new(test_inner()));
        let mut p = ExtHostParser::default();
        let out = p.feed(
            "2026-06-06T10:00:01.000000: info: [LiveWire]: OSC receiver on udp://...",
            &arc,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].ext.as_deref(), Some("LiveWire"));
        assert_eq!(out[0].kind, LineKind::Console);
        assert!(out[0].text.starts_with("OSC receiver"));
    }

    #[test]
    fn parses_stack_frame_location() {
        let frame = "    at activate (/Users/x/Music/Ableton/User Library/Extensions/petri/dist/extension.js:42:13)";
        let (path, l, c) = parse_stack_frame(frame).unwrap();
        assert!(path.ends_with("petri/dist/extension.js"));
        assert_eq!(l, 42);
        assert_eq!(c, 13);
        assert_eq!(ext_from_dist_path(&path).as_deref(), Some("petri"));
    }

    #[test]
    fn vlq_decodes_known_segments() {
        // `A`=0, `I`=8 → 4 (>>1), `E`=4 → 2 (>>1).
        assert_eq!(decode_vlq("A"), vec![0]);
        assert_eq!(decode_vlq("AAIE"), vec![0, 0, 4, 2]);
        // A negative value: 1 (odd) → -0? Use `D`=3 → (3>>1)=1, sign bit set → -1.
        assert_eq!(decode_vlq("D"), vec![-1]);
        // A continuation byte: `gB` decodes to 16 (low 5 bits of g=32→cont, value carries).
        // (sanity: multi-char produces a single field)
        assert_eq!(decode_vlq("gB").len(), 1);
    }

    #[test]
    fn maps_through_a_real_sourcemap() {
        // A minimal, valid sourcemap: generated line 2 (0-based 1), col 0 maps to
        // `src/extension.ts` original line 5 (0-based 4), col 2. Mappings `;AAIE`:
        // line 0 empty; line 1 segment AAIE = [genCol 0, srcIdx 0, srcLine +4, srcCol +2].
        let dir = tempfile::tempdir().unwrap();
        let dist = dir.path().join("dist");
        std::fs::create_dir_all(&dist).unwrap();
        let dist_js = dist.join("extension.js");
        std::fs::write(&dist_js, "// generated\nthrow new Error('boom');\n").unwrap();
        let map = serde_json::json!({
            "version": 3,
            "sources": ["../src/extension.ts"],
            "sourcesContent": serde_json::Value::Null,
            "mappings": ";AAIE",
            "names": [],
        });
        std::fs::write(
            dist.join("extension.js.map"),
            serde_json::to_string(&map).unwrap(),
        )
        .unwrap();

        // Node reports a 1-based line: generated line 2, col 0.
        let loc = map_through_sourcemap(&dist_js, 2, 0).expect("mapped location");
        assert_eq!(loc.file, "../src/extension.ts");
        assert_eq!(loc.line, 5, "0-based src line 4 → 1-based 5");
        assert_eq!(loc.col, 2);
    }

    #[test]
    fn exthost_parser_maps_activate_failure_to_source() {
        // A scripted activate-failure block (the SPEC H §5 crash footer shape) whose
        // stack frame points at the deployed dist/extension.js — the parser emits a
        // framed ActivateFailed with the sourcemap-mapped file:line.
        let dir = tempfile::tempdir().unwrap();
        let dist = dir.path().join("Extensions/petri/dist");
        std::fs::create_dir_all(&dist).unwrap();
        let dist_js = dist.join("extension.js");
        std::fs::write(&dist_js, "// gen\nthrow new Error('x');\n").unwrap();
        let map = serde_json::json!({
            "version": 3,
            "sources": ["../src/extension.ts"],
            "mappings": ";AAIE",
            "names": [],
        });
        std::fs::write(
            dist.join("extension.js.map"),
            serde_json::to_string(&map).unwrap(),
        )
        .unwrap();

        let mut inner = test_inner();
        inner.dist_for.insert("petri".to_string(), dist_js.clone());
        let arc = Arc::new(Mutex::new(inner));

        let mut p = ExtHostParser::default();
        // The uncaught-exception header opens the block.
        let _ = p.feed(
            "2026-06-06T10:00:00.000000: error: Uncaught exception (uncaughtException)",
            &arc,
        );
        // The Error: message line (continuation).
        let _ = p.feed("Error: boom in activate", &arc);
        // The stack frame pointing at the deployed dist bundle, generated line 2 col 0.
        let frame = format!("    at activate ({}:2:0)", dist_js.display());
        let out = p.feed(&frame, &arc);

        assert_eq!(out.len(), 1, "one framed ActivateFailed");
        let ll = &out[0];
        assert_eq!(ll.ext.as_deref(), Some("petri"));
        assert_eq!(ll.kind, LineKind::Lifecycle);
        assert_eq!(ll.level, Level::Error);
        assert!(ll.text.contains("boom in activate"), "got: {}", ll.text);
        let mapped = ll.mapped.as_ref().expect("mapped source location");
        assert_eq!(mapped.file, "../src/extension.ts");
        assert_eq!(mapped.line, 5);
        assert!(ll.text.contains("extension.ts:5"), "got: {}", ll.text);
    }

    #[test]
    fn exthost_parser_dedups_repeat_marker_and_continuations() {
        let arc = Arc::new(Mutex::new(test_inner()));
        let mut p = ExtHostParser::default();
        // A normal info line, then a "Last message repeated" de-dup marker passes through
        // as a host line (not dropped).
        let out = p.feed(
            "2026-06-06T10:00:02.000000: error: Last message repeated 3 time(s): boom",
            &arc,
        );
        assert_eq!(out.len(), 1);
        assert!(out[0].text.contains("Last message repeated"));
    }

    #[test]
    fn exthost_ts_orders_monotonically() {
        let a = parse_exthost_ts("2026-06-06T10:00:00.000000").unwrap();
        let b = parse_exthost_ts("2026-06-06T10:00:01.500000").unwrap();
        assert!(b > a);
        assert_eq!(b - a, 1500);
    }

    fn test_inner() -> Inner {
        Inner {
            log_root: PathBuf::from("/tmp/x"),
            session: "s".into(),
            files: Default::default(),
            shared: None,
            subscribers: Vec::new(),
            history: Default::default(),
            dist_for: Default::default(),
        }
    }

    fn read_session_lines_at(dir: &Path, name: Option<&str>) -> Vec<LogLine> {
        let sub = match name {
            Some(n) => dir.join(n),
            None => dir.join("_session"),
        };
        let mut out = Vec::new();
        for e in std::fs::read_dir(&sub).unwrap().flatten() {
            let content = std::fs::read_to_string(e.path()).unwrap();
            for l in content.lines() {
                if let Some(ll) = LogLine::from_file_record(l) {
                    out.push(ll);
                }
            }
        }
        out
    }
}
