//! `rackabel dev logs [NAME] [--follow] [--since 5m] [--level LEVEL] [--json] [--raw]`
//! — tail/filter the host's per-extension log sink (DESIGN §2, §3.4).
//!
//! OWNED BY THE LOGS AGENT. The command works from *any* terminal: it asks the running
//! daemon for the per-extension log stream over the control socket (a `Logs` request,
//! one-shot for a non-`--follow` read, a stream for `--follow`). When the daemon is
//! **down**, it falls back to reading the persisted session-log files directly
//! (`~/.rackabel/logs/<name>/<session>.log`), so a read-only `dev logs` works with a
//! dead daemon (DESIGN §3.4). `--since`/`--level`/NAME filter the lines; `--json` emits
//! one JSON object per line; `--raw` shows the unfiltered host/Node stream too.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::cli::DevLogsArgs;
use crate::context::Ctx;
use crate::dev::ipc::{self, Request, Response};
use crate::dev::logs::{self, Level, LineKind, LogLine};
use crate::dev::{resolve, sock_path};
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::ui::color::Style;

pub fn run(args: &DevLogsArgs, ctx: &Ctx) -> CmdResult<()> {
    let since_ms = parse_since_arg(args, ctx)?;
    let min_level = args.level.as_deref().map(Level::parse);
    let name = args.name.as_deref();

    // A non-`--follow` read always comes from the persisted session files: they're
    // complete and the read must work with a dead daemon (§3.4). `--follow` prefers the
    // daemon's live broadcast (the freshest stream), falling back to file-tailing the
    // newest session log when no daemon is up.
    if !args.follow {
        return file_read(args, ctx, since_ms, min_level, name);
    }
    match try_daemon_follow(args, ctx, since_ms, min_level) {
        Ok(()) => Ok(()),
        Err(e) if e.code == ErrorCode::NoDaemon => {
            file_follow(args, ctx, since_ms, min_level, name)
        }
        Err(e) => Err(e),
    }
}

// --- daemon path -----------------------------------------------------------------

/// Resolve the per-Live socket and stream live logs over it. Returns `RK0309 NoDaemon`
/// if the Live target can't be resolved or the socket isn't connectable, so the caller
/// falls back to file-tailing.
fn try_daemon_follow(
    args: &DevLogsArgs,
    ctx: &Ctx,
    since_ms: Option<u64>,
    min_level: Option<Level>,
) -> CmdResult<()> {
    // Resolving Live may be impossible (no install) — treat that as "no daemon" so the
    // file fallback still works. Never prompt here.
    let target = resolve::resolve(ctx).map_err(|_| no_daemon())?;
    let sock = sock_path(ctx, target.app());
    let mut client = ipc::Client::connect(&sock)?; // RK0309 if absent/refused

    // Stop the stream cleanly on Ctrl-C by sending StopStream on a writer clone.
    let stop = install_sigint();
    let writer = client.writer_clone().ok();
    let stop_for_thread = Arc::clone(&stop);
    let watcher = writer.map(|mut w| {
        std::thread::spawn(move || {
            use std::io::Write;
            while !stop_for_thread.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let line =
                serde_json::to_string(&ipc::RequestEnvelope::new(Request::StopStream)).unwrap();
            let _ = w.write_all(line.as_bytes());
            let _ = w.write_all(b"\n");
            let _ = w.flush();
        })
    });
    let result = stream_daemon(&mut client, ctx, since_ms, min_level, args);
    stop.store(true, Ordering::SeqCst);
    if let Some(h) = watcher {
        let _ = h.join();
    }
    result
}

/// Drive a `--follow` daemon stream, printing each matching line until it ends.
fn stream_daemon(
    client: &mut ipc::Client,
    ctx: &Ctx,
    since_ms: Option<u64>,
    min_level: Option<Level>,
    args: &DevLogsArgs,
) -> CmdResult<()> {
    let req = Request::Logs {
        name: args.name.clone(),
        follow: true,
        since_ms,
        level: args.level.clone(),
        raw: args.raw,
    };
    let iter = client.stream(req)?;
    for item in iter {
        let resp = item?;
        if let Response::LogLine { .. } = &resp
            && let Some(line) = response_to_line(&resp)
            && keep(&line, since_ms, min_level, args)
        {
            print_line(&line, ctx, args);
        }
    }
    Ok(())
}

fn response_to_line(resp: &Response) -> Option<LogLine> {
    match resp {
        Response::LogLine {
            ts_ms,
            level,
            ext,
            kind,
            text,
            mapped,
        } => Some(LogLine {
            ts_ms: *ts_ms,
            level: Level::parse(level),
            ext: ext.clone(),
            kind: LineKind::parse(kind),
            text: text.clone(),
            mapped: mapped.clone(),
        }),
        _ => None,
    }
}

// --- file path (persisted session files; works with a dead daemon, §3.4) ---------

/// Read the persisted session-log files directly (the non-`--follow`, daemon-independent
/// read). Filters + prints all matching saved lines.
fn file_read(
    args: &DevLogsArgs,
    ctx: &Ctx,
    since_ms: Option<u64>,
    min_level: Option<Level>,
    name: Option<&str>,
) -> CmdResult<()> {
    let history = logs::read_session_lines(ctx, name);
    let mut printed = false;
    for line in &history {
        if keep(line, since_ms, min_level, args) {
            print_line(line, ctx, args);
            printed = true;
        }
    }
    if !printed && !ctx.json {
        eprintln!(
            "(no saved log lines for {} — run `rackabel dev` to start the host)",
            name.unwrap_or("any extension")
        );
    }
    Ok(())
}

/// Tail the newest persisted session file for new appends until Ctrl-C — the `--follow`
/// fallback when no daemon is up.
fn file_follow(
    args: &DevLogsArgs,
    ctx: &Ctx,
    since_ms: Option<u64>,
    min_level: Option<Level>,
    name: Option<&str>,
) -> CmdResult<()> {
    if ctx.echo_on() && !ctx.json {
        eprintln!(
            "{}",
            Style::Dim.paint(
                "(no dev host running — tailing saved logs; run `rackabel dev` for live output)",
                ctx.color_err
            )
        );
    }
    // Replay the saved lines first, then tail for new appends past the last seen ts.
    let history = logs::read_session_lines(ctx, name);
    let last_ts = history.last().map(|l| l.ts_ms);
    for line in &history {
        if keep(line, since_ms, min_level, args) {
            print_line(line, ctx, args);
        }
    }

    let Some(path) = logs::newest_session_file(ctx, name) else {
        if !ctx.json {
            eprintln!("(no session log to follow — start the dev host with `rackabel dev`)");
        }
        return Ok(());
    };
    let stop = install_sigint();
    let stop_poll = Arc::clone(&stop);
    let ctx2 = ctx.clone();
    let args2 = clone_args(args);
    logs::tail_session_file(
        &path,
        false,
        move |line| {
            // Skip anything already shown from history.
            if let Some(t) = last_ts
                && line.ts_ms <= t
            {
                return;
            }
            if keep(&line, since_ms, min_level, &args2) {
                print_line(&line, &ctx2, &args2);
            }
        },
        move || stop_poll.load(Ordering::SeqCst),
    );
    Ok(())
}

// --- shared rendering + filtering ------------------------------------------------

/// Whether a line survives the NAME/`--since`/`--level`/`--raw` filters. `--raw` keeps
/// host/Node internal lines that are otherwise hidden.
fn keep(
    line: &LogLine,
    since_ms: Option<u64>,
    min_level: Option<Level>,
    args: &DevLogsArgs,
) -> bool {
    // Without --raw, suppress unattributed host/Node internals (the shared stream is
    // noisy; framed lifecycle + attributed console lines are the signal, §3.4).
    if !args.raw && line.kind == LineKind::Host && line.ext.is_none() {
        return false;
    }
    line.matches(since_ms, min_level, args.name.as_deref())
}

/// Print one line, honoring `--json` (one object per line) vs the human format.
fn print_line(line: &LogLine, ctx: &Ctx, _args: &DevLogsArgs) {
    if ctx.json {
        println!("{}", serde_json::to_string(&line.to_json()).unwrap());
        return;
    }
    let lvl = match line.level {
        Level::Info => Style::Dim.paint("info ", ctx.color),
        Level::Warn => Style::Warn.paint("warn ", ctx.color),
        Level::Error => Style::Bad.paint("error", ctx.color),
    };
    let ext = match &line.ext {
        Some(e) => format!(" {}", Style::Heading.paint(e, ctx.color)),
        None => String::new(),
    };
    print!("{lvl}{ext}  {}", line.text);
    if let Some(m) = &line.mapped {
        print!(
            "  {}",
            Style::Dim.paint(&format!("({}:{})", m.file, m.line), ctx.color)
        );
    }
    println!();
}

/// Parse the `--since` argument into an absolute "newer than" epoch-ms cutoff.
fn parse_since_arg(args: &DevLogsArgs, _ctx: &Ctx) -> CmdResult<Option<u64>> {
    let Some(s) = &args.since else {
        return Ok(None);
    };
    let window_ms = logs::parse_since(s).map_err(|msg| {
        RkError::of(
            ErrorCode::UsageError,
            msg,
            "use a duration like `30s`, `5m`, `1h`, or `2d`",
        )
    })?;
    let now = now_ms();
    Ok(Some(now.saturating_sub(window_ms)))
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn no_daemon() -> RkError {
    RkError::of(
        ErrorCode::NoDaemon,
        "no dev host is running",
        "start it with `rackabel dev`, then retry",
    )
}

/// Install a SIGINT handler that flips a shared flag, so a `--follow` loop can stop
/// cleanly (and send `StopStream`). Best-effort: if installation fails the loop simply
/// relies on process termination.
fn install_sigint() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    static HANDLER_FLAG: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
    let _ = HANDLER_FLAG.set(Arc::clone(&flag));
    extern "C" fn on_sigint(_: libc::c_int) {
        if let Some(f) = HANDLER_FLAG.get() {
            f.store(true, Ordering::SeqCst);
        }
    }
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
    flag
}

/// Clone the args struct (it isn't `Clone`-derived; we need an owned copy for the tail
/// closure).
fn clone_args(args: &DevLogsArgs) -> DevLogsArgs {
    DevLogsArgs {
        name: args.name.clone(),
        follow: args.follow,
        since: args.since.clone(),
        level: args.level.clone(),
        raw: args.raw,
    }
}
