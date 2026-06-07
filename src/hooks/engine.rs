//! The hook-execution engine — FROZEN SIGNATURE, real body (DESIGN §5.3, §5.7).
//!
//! FOUNDATION-OWNED SIGNATURE; the BODY is a 0.5 feature agent's. [`run_hook`] is the single
//! entry point the build/deploy/dev/doctor/new call sites use to execute one resolved hook.
//! The foundation froze its shape and landed a compiling stub; this body replaces the stub
//! with the real subprocess machinery WITHOUT changing the signature.
//!
//! ## The §5.3 execution contract this body implements
//!   - Spawn the resolved command ([`super::discovery::ResolvedHook::command_path`]) as a
//!     subprocess with the FULL §5.2 env contract
//!     ([`crate::plugin::env_contract::build`]) PLUS `RACKABEL_HOOK_API` =
//!     [`super::HOOK_API`] ([`super::RACKABEL_HOOK_API_ENV`]).
//!   - Write EXACTLY the one JSON payload object ([`super::payload::HookPayload::to_json`])
//!     to the child's stdin, then CLOSE stdin (EOF framing): a hook that reads to EOF
//!     terminates naturally; one that blocks for more input hits the timeout.
//!   - Enforce the per-hook wall-clock timeout (`ResolvedHook::timeout_ms`, default
//!     [`super::DEFAULT_TIMEOUT_MS`]): on overrun send SIGTERM, then SIGKILL after a
//!     [`super::TIMEOUT_GRACE_MS`] grace, and treat the hook EXACTLY like a nonzero exit.
//!   - Map `(stdout, exit_code, timed_out)` to a [`HookOutcome`] per the hook's row.
//!   - A hanging hook is reaped by the timeout machinery itself (no orphaned child).

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};

use super::discovery::ResolvedHook;
use super::outcome::{DoctorLine, HookOutcome, TemplateChoice, VetoDecision};
use super::payload::HookPayload;
use super::{HookKind, HookSource, RACKABEL_HOOK_API_ENV};

/// The raw `(stdout, exit_code, timed_out)` triple a hook run produces, BEFORE it is mapped
/// to a kind-specific [`HookOutcome`]. Exposed (crate-internal) so a future caller / test can
/// reason about the channels directly; `run_hook` is the public entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRun {
    /// The hook's captured stdout (UTF-8 lossy).
    pub stdout: String,
    /// The hook's captured stderr (UTF-8 lossy) — surfaced only in the log line / `--raw`.
    pub stderr: String,
    /// The exit code (or a synthesized nonzero for a signal-killed / timed-out child).
    pub exit_code: i32,
    /// Whether the wall-clock timeout fired (SIGTERM→SIGKILL path) — combination (d).
    pub timed_out: bool,
}

impl RawRun {
    /// Whether the hook "failed" in the informational sense: a nonzero exit OR a timeout.
    fn failed(&self) -> bool {
        self.timed_out || self.exit_code != 0
    }
}

/// Run ONE resolved hook with its payload (DESIGN §5.3). FROZEN SIGNATURE.
///
/// `hook` carries the source (project-local vs an enabled plugin — for trust + framing),
/// the resolved command, and the wall-clock timeout. `payload` is the typed §5.3 stdin
/// object for the hook's kind; the kind MUST match `hook.kind` (debug-asserted).
///
/// Returns the [`HookOutcome`] the caller interprets per the phase (informational hooks
/// are swallowed; a `pre_deploy` `Veto` aborts the deploy; a `doctor_check` produces a
/// row; a `new_template` contributes a wizard choice). A framed `RkError` is reserved for
/// an engine-level failure even an informational hook can't swallow: the command path does
/// not exist / could not be spawned (RK1309). Every OTHER failure (nonzero, timeout, a
/// crash mid-run) is encoded in the returned outcome, not an `Err`.
pub fn run_hook(hook: &ResolvedHook, payload: &HookPayload, ctx: &Ctx) -> CmdResult<HookOutcome> {
    debug_assert_eq!(
        hook.kind,
        payload.kind(),
        "run_hook: payload kind must match the resolved hook's kind"
    );

    let raw = spawn_and_wait(hook, payload, ctx)?;
    Ok(map_outcome(hook.kind, &raw))
}

/// Spawn the resolved command, write the one-JSON-object stdin then close it, and wait under
/// the per-hook wall-clock timeout (SIGTERM → SIGKILL after the grace). A spawn failure is
/// the one `RkError` boundary (RK1309); everything else lands in [`RawRun`].
fn spawn_and_wait(hook: &ResolvedHook, payload: &HookPayload, ctx: &Ctx) -> CmdResult<RawRun> {
    let cmd_path = hook.command_path();

    // The env contract: the FULL §5.2 map PLUS RACKABEL_HOOK_API. A project-local hook
    // resolves the project root from the source; a plugin hook is identified by name (its
    // project context is the current cwd's project, if any, since deploy/build hooks run
    // inside a project — doctor_check/new_template can run outside, where project is None).
    let project_root = match &hook.source {
        HookSource::Project { project_root } => Some(project_root.clone()),
        HookSource::Plugin { .. } => crate::plugin::env_contract::resolve_project_root(ctx),
    };
    let mut env = crate::plugin::env_contract::build(ctx, project_root.as_deref());
    env.insert(
        RACKABEL_HOOK_API_ENV.to_string(),
        super::HOOK_API.to_string(),
    );

    // The cwd: the owning root (project root / plugin store dir), so a relative helper that
    // shells out resolves its own siblings.
    let cwd = hook.source.base_dir();

    let mut child = Command::new(&cmd_path)
        .envs(&env)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| spawn_failed(hook, &cmd_path, e))?;

    // Write exactly one JSON object then CLOSE stdin (EOF framing). The write runs on a
    // detached thread so a hook that never drains stdin cannot wedge us before the timeout
    // (the pipe buffer could fill); dropping the handle on this side closes the writer end.
    if let Some(stdin) = child.stdin.take() {
        let body =
            serde_json::to_vec(&payload.to_json()).expect("hook payloads serialize infallibly");
        std::thread::spawn(move || {
            let mut stdin = stdin;
            // Best-effort: a hook that exits before reading gives us a broken pipe; that is
            // not our failure (its exit code drives the outcome).
            let _ = stdin.write_all(&body);
            // Dropping `stdin` here closes the pipe ⇒ the child sees EOF.
        });
    }

    wait_with_timeout(child, hook.timeout_ms)
}

/// Wait for `child` to exit, killing it (SIGTERM, then SIGKILL after the grace) if it
/// exceeds `timeout_ms`. Captures stdout/stderr fully. A timed-out run is reaped here (no
/// orphan) and reported with `timed_out = true` (= combination (d)).
fn wait_with_timeout(mut child: std::process::Child, timeout_ms: u64) -> CmdResult<RawRun> {
    // Drain stdout/stderr on threads so a chatty hook cannot deadlock against a full pipe
    // while we wait. We join them after the process is reaped.
    let out_handle = child.stdout.take().map(spawn_reader);
    let err_handle = child.stderr.take().map(spawn_reader);

    let pid = child.id();
    // Wait on a thread so the main thread can enforce the wall clock with recv_timeout.
    let (tx, rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let status = child.wait();
        let _ = tx.send((child, status));
    });

    let timeout = Duration::from_millis(timeout_ms);
    let (mut child, status, timed_out) = match rx.recv_timeout(timeout) {
        Ok((child, status)) => (child, status, false),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // The bounded-DoS mitigation (§5.7): SIGTERM, grace, then SIGKILL — then reap.
            terminate(pid);
            std::thread::sleep(Duration::from_millis(super::TIMEOUT_GRACE_MS));
            kill_hard(pid);
            // The waiter thread now observes the dead child and sends it back; block for it
            // so we reap the zombie (no orphan) before returning.
            let (child, status) = rx
                .recv()
                .expect("the wait thread always sends once the child is reaped");
            (child, status, true)
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            // The waiter panicked without sending — should not happen; treat as a failure.
            let _ = waiter.join();
            return Err(RkError::of(
                ErrorCode::HookFailed,
                "the hook process could not be waited on",
                "this is a bug in rackabel's hook engine; please report it",
            ));
        }
    };
    let _ = &mut child; // child is owned again; dropping it is fine (already reaped).
    let _ = waiter.join();

    let stdout = out_handle.map(join_reader).unwrap_or_default();
    let stderr = err_handle.map(join_reader).unwrap_or_default();

    // The exit code: the real code on a clean exit; a synthesized nonzero (124, the
    // conventional timeout code; 137 = 128+SIGKILL for a signal) so the (stdout,exit,timed)
    // triple is unambiguous. `timed_out` is the authoritative timeout signal regardless.
    let exit_code = if timed_out {
        124
    } else {
        match status {
            Ok(s) => s.code().unwrap_or(137), // None ⇒ killed by a signal ⇒ nonzero.
            Err(_) => 1,
        }
    };

    Ok(RawRun {
        stdout,
        stderr,
        exit_code,
        timed_out,
    })
}

/// Spawn a thread that reads a child pipe to EOF, returning the join handle.
fn spawn_reader<R: std::io::Read + Send + 'static>(mut r: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut r, &mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

fn join_reader(h: std::thread::JoinHandle<String>) -> String {
    h.join().unwrap_or_default()
}

/// Send SIGTERM to the hook process (graceful stop). On non-unix, fall back to a hard kill
/// (no graceful signal available without extra machinery).
#[cfg(unix)]
fn terminate(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
}

#[cfg(not(unix))]
fn terminate(_pid: u32) {
    // No portable graceful signal; the hard kill below does the work.
}

/// Force-kill the hook process (SIGKILL on unix). Best-effort: a process that already exited
/// during the grace is a no-op.
#[cfg(unix)]
fn kill_hard(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(pid as i32), Signal::SIGKILL);
}

#[cfg(not(unix))]
fn kill_hard(_pid: u32) {
    // The std `Child::kill` path is reached via the waiter; on non-unix the timeout simply
    // waits for the OS to reap. (rackabel's hook surface is exercised on unix; the non-unix
    // build keeps compiling.)
}

/// Map a finished hook run to its kind-specific [`HookOutcome`] per the §5.3 table.
fn map_outcome(kind: HookKind, raw: &RawRun) -> HookOutcome {
    match kind {
        HookKind::PostBuild | HookKind::OnReload => {
            // Informational: stdout ignored; nonzero/timeout logged + skipped (never fatal).
            HookOutcome::Informational {
                failed: raw.failed(),
            }
        }
        HookKind::PreDeploy => {
            // The ONE veto: Allow on a clean exit, Veto on nonzero OR timeout.
            if raw.failed() {
                HookOutcome::Veto(VetoDecision::Veto {
                    timed_out: raw.timed_out,
                })
            } else {
                HookOutcome::Veto(VetoDecision::Allow)
            }
        }
        HookKind::DoctorCheck => {
            // The a-d precedence lives in DoctorLine::resolve (the line wins when present).
            HookOutcome::Doctor(DoctorLine::resolve(
                &raw.stdout,
                raw.exit_code,
                raw.timed_out,
            ))
        }
        HookKind::NewTemplate => {
            // The choice, or None when nothing printed / nonzero / timed out.
            let choice = if raw.failed() {
                None
            } else {
                TemplateChoice::parse(&raw.stdout)
            };
            HookOutcome::Template(choice)
        }
    }
}

/// The engine-level "could not spawn the hook command" frame (RK1309). This is the ONE
/// boundary `run_hook` returns `Err` for: a missing/unexecutable command path is a setup
/// problem the caller surfaces (an informational caller still logs + skips it).
fn spawn_failed(hook: &ResolvedHook, cmd_path: &Path, e: std::io::Error) -> RkError {
    RkError::of(
        ErrorCode::HookFailed,
        format!(
            "the {} hook from {} could not be started",
            hook.kind,
            hook.source.label()
        ),
        "check the hook command path exists and is executable, then retry \
         (or `rackabel plugin disable <name>` to stop invoking it)",
    )
    .at(cmd_path.display().to_string())
    .raw(e.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::outcome::{DoctorResolution, DoctorSymbol};
    use crate::hooks::payload::{DoctorCheckPayload, NewTemplatePayload};

    fn ctx() -> Ctx {
        Ctx {
            no_input: true,
            json: false,
            quiet: false,
            verbose: false,
            raw: false,
            color: crate::ui::color::ColorMode::Never,
            color_err: crate::ui::color::ColorMode::Never,
            cwd: std::env::temp_dir(),
            rackabel_home: std::env::temp_dir().join(".rackabel-hook-test"),
            home: std::env::temp_dir(),
            ableton_app: None,
            ableton_user_library: None,
            ableton_eh_mod: None,
            ableton_eh_node: None,
            ableton_extensions_dir: None,
            ableton_storage_base: None,
            rackabel_host_cmd: None,
        }
    }

    /// Write an executable shell script into a temp dir and return a project-source
    /// ResolvedHook pointing at it.
    #[cfg(unix)]
    fn script_hook(dir: &Path, kind: HookKind, body: &str, timeout_ms: u64) -> ResolvedHook {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("hook.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        ResolvedHook {
            source: HookSource::Project {
                project_root: dir.to_path_buf(),
            },
            kind,
            command: "hook.sh".to_string(),
            timeout_ms,
        }
    }

    #[test]
    fn spawn_failure_is_framed_rk1309() {
        let hook = ResolvedHook {
            source: HookSource::Project {
                project_root: std::path::PathBuf::from("/nonexistent-root-xyz"),
            },
            kind: HookKind::DoctorCheck,
            command: "definitely-not-here".to_string(),
            timeout_ms: 30_000,
        };
        let payload = HookPayload::DoctorCheck(DoctorCheckPayload {
            project_dir: None,
            manifest_toml: None,
        });
        let err = run_hook(&hook, &payload, &ctx()).unwrap_err();
        assert_eq!(err.code, ErrorCode::HookFailed);
    }

    #[cfg(unix)]
    #[test]
    fn doctor_check_line_wins_even_on_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let hook = script_hook(
            dir.path(),
            HookKind::DoctorCheck,
            r#"echo '{"symbol":"warn","message":"creds missing","help":"set KEY"}'; exit 3"#,
            30_000,
        );
        let payload = HookPayload::DoctorCheck(DoctorCheckPayload {
            project_dir: None,
            manifest_toml: None,
        });
        let outcome = run_hook(&hook, &payload, &ctx()).unwrap();
        match outcome {
            HookOutcome::Doctor(DoctorResolution::Line(line)) => {
                assert_eq!(line.symbol, DoctorSymbol::Warn);
                assert_eq!(line.message, "creds missing");
                assert_eq!(line.help.as_deref(), Some("set KEY"));
            }
            other => panic!("expected a doctor line, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn doctor_check_exit0_no_line_is_pass() {
        let dir = tempfile::tempdir().unwrap();
        let hook = script_hook(
            dir.path(),
            HookKind::DoctorCheck,
            "echo just chatter; exit 0",
            30_000,
        );
        let payload = HookPayload::DoctorCheck(DoctorCheckPayload {
            project_dir: None,
            manifest_toml: None,
        });
        assert_eq!(
            run_hook(&hook, &payload, &ctx()).unwrap(),
            HookOutcome::Doctor(DoctorResolution::Pass)
        );
    }

    #[cfg(unix)]
    #[test]
    fn doctor_check_nonzero_no_line_is_generic_fail() {
        let dir = tempfile::tempdir().unwrap();
        let hook = script_hook(
            dir.path(),
            HookKind::DoctorCheck,
            "echo oops 1>&2; exit 5",
            30_000,
        );
        let payload = HookPayload::DoctorCheck(DoctorCheckPayload {
            project_dir: None,
            manifest_toml: None,
        });
        assert_eq!(
            run_hook(&hook, &payload, &ctx()).unwrap(),
            HookOutcome::Doctor(DoctorResolution::GenericFail)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_hanging_hook_is_reaped_by_the_timeout_as_generic_fail() {
        let dir = tempfile::tempdir().unwrap();
        // Sleep far past a short timeout; the engine must SIGTERM/SIGKILL + reap it.
        let hook = script_hook(
            dir.path(),
            HookKind::DoctorCheck,
            "sleep 30; echo never",
            150, // 150ms timeout
        );
        let payload = HookPayload::DoctorCheck(DoctorCheckPayload {
            project_dir: None,
            manifest_toml: None,
        });
        let start = std::time::Instant::now();
        let outcome = run_hook(&hook, &payload, &ctx()).unwrap();
        // Combination (d): a timeout produced no line ⇒ generic fail.
        assert_eq!(outcome, HookOutcome::Doctor(DoctorResolution::GenericFail));
        // It returned well before the 30s sleep (timeout + 5s grace, not 30s).
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the hang should be bounded by the timeout, took {:?}",
            start.elapsed()
        );
    }

    #[cfg(unix)]
    #[test]
    fn new_template_choice_is_parsed_from_stdout() {
        let dir = tempfile::tempdir().unwrap();
        let hook = script_hook(
            dir.path(),
            HookKind::NewTemplate,
            "echo gh:acme/starter@v2",
            30_000,
        );
        let payload = HookPayload::NewTemplate(NewTemplatePayload {
            kind: "extension".to_string(),
        });
        let outcome = run_hook(&hook, &payload, &ctx()).unwrap();
        assert_eq!(
            outcome,
            HookOutcome::Template(Some(TemplateChoice::Ref("gh:acme/starter@v2".to_string())))
        );
    }

    #[cfg(unix)]
    #[test]
    fn new_template_nonzero_omits_the_choice() {
        let dir = tempfile::tempdir().unwrap();
        let hook = script_hook(
            dir.path(),
            HookKind::NewTemplate,
            "echo /opt/x; exit 1",
            30_000,
        );
        let payload = HookPayload::NewTemplate(NewTemplatePayload {
            kind: "extension".to_string(),
        });
        // A nonzero exit ⇒ the choice is omitted (logged), even though it printed a path.
        assert_eq!(
            run_hook(&hook, &payload, &ctx()).unwrap(),
            HookOutcome::Template(None)
        );
    }

    #[cfg(unix)]
    #[test]
    fn payload_reaches_the_hook_on_stdin_as_one_json_object() {
        let dir = tempfile::tempdir().unwrap();
        // The hook echoes a doctor line built from the `kind` field of the stdin JSON,
        // proving the payload arrives and stdin is closed (jq-free: grep the raw text).
        let hook = script_hook(
            dir.path(),
            HookKind::DoctorCheck,
            r#"in=$(cat); case "$in" in *'"project_dir"'*) echo '{"symbol":"ok","message":"in-project"}';; *) echo '{"symbol":"ok","message":"no-project"}';; esac"#,
            30_000,
        );
        let payload = HookPayload::DoctorCheck(DoctorCheckPayload {
            project_dir: Some("/some/proj".to_string()),
            manifest_toml: None,
        });
        let outcome = run_hook(&hook, &payload, &ctx()).unwrap();
        match outcome {
            HookOutcome::Doctor(DoctorResolution::Line(line)) => {
                assert_eq!(line.message, "in-project");
            }
            other => panic!("expected a doctor line, got {other:?}"),
        }
    }
}
