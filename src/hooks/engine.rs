//! The hook-execution engine (DESIGN §5.3, §5.7).
//!
//! [`run_hook`] is the single entry point the build/deploy/dev/doctor/new call sites use
//! to execute one resolved hook. It implements the §5.3 execution contract exactly:
//!
//!   - Spawn the resolved command ([`super::discovery::ResolvedHook::command_path`]) as a
//!     subprocess with the FULL §5.2 env contract
//!     ([`crate::plugin::env_contract::build`]) PLUS `RACKABEL_HOOK_API` =
//!     [`super::HOOK_API`] ([`super::RACKABEL_HOOK_API_ENV`]).
//!   - Write EXACTLY the one JSON payload object ([`super::payload::HookPayload::to_json`])
//!     to the child's stdin, then CLOSE stdin (EOF framing): a hook that reads to EOF
//!     terminates naturally; one that blocks for more input hits the timeout.
//!   - Enforce the per-hook wall-clock timeout (`ResolvedHook::timeout_ms`, default
//!     [`super::DEFAULT_TIMEOUT_MS`]): on overrun send SIGTERM to the child's process group,
//!     then SIGKILL after a [`super::TIMEOUT_GRACE_MS`] grace, and treat the hook EXACTLY
//!     like a nonzero exit. A hanging hook is reaped by this machinery itself — no orphan.
//!   - Map `(stdout, exit_code, timed_out)` to a [`HookOutcome`] per the hook's row:
//!       * `post_build`/`on_reload` ⇒ [`HookOutcome::Informational`] (stdout ignored;
//!         failure logged + skipped, never fatal);
//!       * `pre_deploy` ⇒ [`HookOutcome::Veto`] (`Allow` on exit 0, `Veto` on nonzero OR
//!         timeout — the ONE veto);
//!       * `doctor_check` ⇒ [`HookOutcome::Doctor`] via
//!         [`super::outcome::DoctorLine::resolve`] (the a-d precedence);
//!       * `new_template` ⇒ [`HookOutcome::Template`] via
//!         [`super::outcome::TemplateChoice::parse`] (the choice, or `None`).
//!
//! ## Output discipline (§6.2)
//!
//! A hook's own stdout is interpreted ONLY per its contract (doctor line / template
//! choice); everything else — the child's stderr, a nonzero exit, a timeout — is
//! INFORMATIONAL logging routed to rackabel's stderr via [`crate::ui::frame::ewarn`], so
//! it never corrupts the §6.2 dev-chain stdout line or a `--json` envelope on stdout.
//!
//! ## Platform
//!
//! Subprocess + process-group signalling is implemented on Unix (the milestone's target,
//! matching the dev host). On a non-Unix build [`run_hook`] returns a clear framed
//! `RK1309` — hooks are not yet supported there (recorded as a deviation).

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};

use super::discovery::ResolvedHook;
use super::outcome::{DoctorLine, HookOutcome, TemplateChoice, VetoDecision};
use super::payload::HookPayload;
use super::{HookKind, RACKABEL_HOOK_API_ENV};

/// The raw result of running a hook subprocess to completion (or to a timeout): the
/// captured stdout, the exit code (synthesized as nonzero for a signal/timeout kill), and
/// whether the wall-clock timeout fired. The per-kind mapping turns this into a
/// [`HookOutcome`].
#[derive(Debug, Clone, PartialEq, Eq)]
struct RawRun {
    stdout: String,
    stderr: String,
    exit_code: i32,
    timed_out: bool,
}

impl RawRun {
    /// Whether the hook "failed" in the §5.3 sense: a nonzero exit OR a timeout. Used by
    /// the informational/veto mappings (a timeout is a failure for every kind).
    fn failed(&self) -> bool {
        self.timed_out || self.exit_code != 0
    }
}

/// Run ONE resolved hook with its payload (DESIGN §5.3).
///
/// `hook` carries the source (project-local vs an enabled plugin — for trust + framing),
/// the resolved command, and the wall-clock timeout. `payload` is the typed §5.3 stdin
/// object for the hook's kind; the kind MUST match `hook.kind` (debug-asserted).
///
/// Returns the [`HookOutcome`] the caller interprets per the phase (informational hooks
/// are swallowed; a `pre_deploy` `Veto` aborts the deploy; a `doctor_check` produces a
/// row; a `new_template` contributes a wizard choice). A framed `RkError` is reserved for
/// an engine-level failure that even an informational hook can't swallow (the command path
/// does not exist / cannot be spawned) — the caller still decides per the phase whether
/// that is fatal (a `pre_deploy` spawn failure aborts; an informational one is logged).
pub fn run_hook(hook: &ResolvedHook, payload: &HookPayload, ctx: &Ctx) -> CmdResult<HookOutcome> {
    debug_assert_eq!(
        hook.kind,
        payload.kind(),
        "run_hook: payload kind must match the resolved hook's kind"
    );

    let raw = exec(hook, payload, ctx)?;

    // Route the child's stderr (and a nonzero/timeout note) to rackabel's stderr as
    // informational logging — NEVER stdout (so the §6.2 chain line / --json envelope stay
    // clean). The hook's stdout is data ONLY for the kinds that parse it (below).
    log_informational(hook, &raw, ctx);

    Ok(map_outcome(hook.kind, &raw))
}

/// Map a [`RawRun`] to the [`HookOutcome`] for `kind`, per the §5.3 per-hook contract.
fn map_outcome(kind: HookKind, raw: &RawRun) -> HookOutcome {
    match kind {
        // Informational: stdout ignored; a nonzero exit / timeout is logged + skipped and
        // NEVER aborts the phase. We still report `failed` so the caller can log it.
        HookKind::PostBuild | HookKind::OnReload => HookOutcome::Informational {
            failed: raw.failed(),
        },
        // The ONE veto: Allow on a clean exit, Veto on nonzero OR timeout. `timed_out`
        // distinguishes the bounded-DoS path for the §6.1 frame.
        HookKind::PreDeploy => {
            if raw.failed() {
                HookOutcome::Veto(VetoDecision::Veto {
                    timed_out: raw.timed_out,
                })
            } else {
                HookOutcome::Veto(VetoDecision::Allow)
            }
        }
        // The stdout-line-wins a-d precedence (a timeout is combination d by definition).
        HookKind::DoctorCheck => HookOutcome::Doctor(DoctorLine::resolve(
            &raw.stdout,
            raw.exit_code,
            raw.timed_out,
        )),
        // Enumerate: the single stdout line becomes a choice — but only on a clean run.
        // A nonzero exit / timeout omits the choice (logged), per §5.3.
        HookKind::NewTemplate => {
            if raw.failed() {
                HookOutcome::Template(None)
            } else {
                HookOutcome::Template(TemplateChoice::parse(&raw.stdout))
            }
        }
    }
}

/// Emit the hook's informational logging to rackabel's STDERR (never stdout). The child's
/// own stderr is forwarded verbatim (so a `post_build` lint hook's diagnostics reach the
/// developer), and a failure/timeout adds a one-line frame naming the hook + its budget.
/// Suppressed only when `ctx` is fully silent in a way that forbids even stderr — but the
/// dev chain's `quiet` (which owns *stdout*) does NOT silence these stderr log lines, so a
/// failing on-save hook is still visible without corrupting the chain line.
fn log_informational(hook: &ResolvedHook, raw: &RawRun, ctx: &Ctx) {
    // Forward the child's stderr as-is (informational). Keep it on rackabel's stderr.
    let trimmed = raw.stderr.trim_end();
    if !trimmed.is_empty() {
        // Prefix each line so interleaved hook output is attributable in a busy log.
        for line in trimmed.lines() {
            crate::ui::frame::ewarn(&format!("{}: {line}", hook.source.label()), ctx);
        }
    }
    // A failure note (the veto/abort framing is the CALLER's; this is the log breadcrumb).
    if raw.timed_out {
        crate::ui::frame::ewarn(
            &format!(
                "{} hook from {} timed out after {}ms (SIGTERM, then SIGKILL)",
                hook.kind,
                hook.source.label(),
                hook.timeout_ms
            ),
            ctx,
        );
    } else if raw.exit_code != 0 {
        crate::ui::frame::ewarn(
            &format!(
                "{} hook from {} exited {}",
                hook.kind,
                hook.source.label(),
                raw.exit_code
            ),
            ctx,
        );
    }
}

/// Build the framed error for a command that cannot be spawned (does not exist / not
/// executable). The caller decides whether it is fatal for the phase; for the veto path it
/// surfaces directly, for informational hooks the caller swallows it after logging.
fn spawn_error(hook: &ResolvedHook, e: std::io::Error) -> RkError {
    RkError::of(
        ErrorCode::HookFailed,
        format!("the {} hook command could not be started", hook.kind),
        "check the command path exists and is executable (it is resolved relative to the \
         hook's owning root), then retry",
    )
    .at(format!(
        "{} from {}",
        hook.command_path().display(),
        hook.source.label()
    ))
    .raw(e.into())
}

// --- the subprocess body (Unix) ------------------------------------------------

#[cfg(unix)]
fn exec(hook: &ResolvedHook, payload: &HookPayload, ctx: &Ctx) -> CmdResult<RawRun> {
    use std::io::{Read, Write};
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    // The env contract (§5.2) for this invocation: present project vars only inside a
    // project. We resolve the project root from the hook's source (a project hook is rooted
    // there; a plugin hook uses the cwd's project, if any).
    let project = match &hook.source {
        super::HookSource::Project { project_root } => Some(project_root.clone()),
        super::HookSource::Plugin { .. } => crate::plugin::env_contract::resolve_project_root(ctx),
    };
    let env = crate::plugin::env_contract::build(ctx, project.as_deref());

    let payload_bytes = serde_json::to_vec(&payload.to_json())
        .expect("a hook payload always serializes to JSON bytes");

    let mut command = Command::new(hook.command_path());
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // The §5.2 contract vars PLUS the tier-3 hook API integer. Additive: we never
        // clear the inherited env, only overwrite the keys rackabel owns.
        .env(RACKABEL_HOOK_API_ENV, super::HOOK_API.to_string());
    for (k, v) in &env {
        command.env(k, v);
    }
    // Run from the hook's owning root so a relative resource (a config file next to the
    // script) resolves the way the author expects.
    command.current_dir(hook.source.base_dir());

    // Put the child in its OWN process group (leader == child) so a timeout `killpg`
    // reaches the whole tree even if the hook spawns grandchildren that reparent — the
    // bounded-DoS guarantee (§5.7): a hanging hook can never leave an orphan running.
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setpgid(Pid::from_raw(0), Pid::from_raw(0))
                .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
            Ok(())
        });
    }

    let mut child = command.spawn().map_err(|e| spawn_error(hook, e))?;
    let pgid = Pid::from_raw(child.id() as i32);

    // Write EXACTLY one JSON object to stdin, then CLOSE it (drop the handle → EOF). A hook
    // reading to EOF terminates naturally; one blocking for more input hits the timeout.
    // The write runs on a thread so a hook that never drains its stdin can't deadlock us
    // against a full pipe (we still own the timeout regardless).
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&payload_bytes);
        // Dropping `stdin` here closes the pipe → the child sees EOF.
    });

    // Drain stdout/stderr on threads so a chatty hook can't fill a pipe and wedge — and so
    // we still have the captured output after a timeout kill.
    let mut stdout_pipe = child.stdout.take().expect("piped stdout");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let mut stderr_pipe = child.stderr.take().expect("piped stderr");
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });

    // Wall-clock wait under the per-hook timeout. Poll `try_wait` until the deadline; on
    // overrun SIGTERM the group, grace, then SIGKILL — then reap so no zombie/orphan.
    let deadline = Instant::now() + Duration::from_millis(hook.timeout_ms);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    break kill_group_and_reap(&mut child, pgid);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                // Lost track of the child — kill the group defensively and synthesize a
                // failed status so the caller treats it as a hook failure.
                timed_out = false;
                break kill_group_and_reap(&mut child, pgid);
            }
        }
    };

    // The writer/readers finish once the child's pipes close (which a kill guarantees).
    let _ = writer.join();
    let stdout = String::from_utf8_lossy(&stdout_reader.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_reader.join().unwrap_or_default()).into_owned();

    // A killed/timed-out process has no clean code: synthesize a nonzero one so the
    // per-kind mapping treats it as a failure (§5.3: timeout == nonzero exit).
    let exit_code = exit_code_of(&status, timed_out);

    // Belt-and-suspenders: a final group SIGKILL in case the leader exited but a grandchild
    // is still up (the verified orphan case) — bounding the DoS even on a clean exit.
    let _ = killpg(pgid, Signal::SIGKILL);

    Ok(RawRun {
        stdout,
        stderr,
        exit_code,
        timed_out,
    })
}

/// SIGTERM the group, wait up to the grace for the leader, then SIGKILL and reap. Returns
/// the reaped status (used only to confirm exit; the synthesized code comes from
/// [`exit_code_of`]).
#[cfg(unix)]
fn kill_group_and_reap(
    child: &mut std::process::Child,
    pgid: nix::unistd::Pid,
) -> std::process::ExitStatus {
    use std::time::{Duration, Instant};

    use nix::sys::signal::{Signal, killpg};

    let _ = killpg(pgid, Signal::SIGTERM);
    let deadline = Instant::now() + Duration::from_millis(super::TIMEOUT_GRACE_MS);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status,
            _ => {
                if Instant::now() >= deadline {
                    let _ = killpg(pgid, Signal::SIGKILL);
                    // Block until the leader is reaped so we never leave a zombie.
                    return child.wait().unwrap_or_else(|_| dead_status());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// Resolve the integer exit code from a process status, synthesizing a nonzero code for a
/// timeout / signal-kill (which has no clean exit code) so the §5.3 "timeout == nonzero"
/// rule holds uniformly.
#[cfg(unix)]
fn exit_code_of(status: &std::process::ExitStatus, timed_out: bool) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    if let Some(code) = status.code() {
        // A timed-out process that somehow reported a 0 code must still count as failed.
        if timed_out && code == 0 { 137 } else { code }
    } else if let Some(sig) = status.signal() {
        // Conventional 128 + signal (e.g. SIGKILL=9 ⇒ 137), the shell convention.
        128 + sig
    } else {
        137
    }
}

/// A synthetic "killed" status for the unreachable case where `wait` itself errors.
#[cfg(unix)]
fn dead_status() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(9) // SIGKILL
}

// --- the non-Unix fallback -----------------------------------------------------

#[cfg(not(unix))]
fn exec(hook: &ResolvedHook, _payload: &HookPayload, _ctx: &Ctx) -> CmdResult<RawRun> {
    // Process-group signalling (the timeout/SIGKILL guarantee) is implemented on Unix only
    // in this milestone — matching the dev host (§9.3). A hook on a non-Unix build returns a
    // clear framed error rather than running without the bounded-DoS guarantee.
    Err(RkError::of(
        ErrorCode::HookFailed,
        format!(
            "lifecycle hooks are not supported on this platform yet ({})",
            hook.kind
        ),
        "run hooks on macOS/Linux; non-Unix hook execution lands in a later milestone",
    )
    .at(hook.source.label()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::payload::{
        DoctorCheckPayload, NewTemplatePayload, OnReloadPayload, PostBuildPayload, PreDeployPayload,
    };
    use crate::hooks::{HookKind, HookSource};
    use serde_json::json;

    fn ctx() -> Ctx {
        Ctx {
            no_input: true,
            json: false,
            quiet: false,
            verbose: false,
            raw: false,
            color: crate::ui::color::ColorMode::Never,
            color_err: crate::ui::color::ColorMode::Never,
            cwd: std::path::PathBuf::from("/tmp"),
            rackabel_home: std::path::PathBuf::from("/tmp/.rackabel"),
            home: std::path::PathBuf::from("/tmp"),
            ableton_app: None,
            ableton_user_library: None,
            ableton_eh_mod: None,
            ableton_eh_node: None,
            ableton_extensions_dir: None,
            ableton_storage_base: None,
            rackabel_host_cmd: None,
        }
    }

    // ---- the pure mapping (platform-independent) ----

    fn raw(stdout: &str, exit_code: i32, timed_out: bool) -> RawRun {
        RawRun {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code,
            timed_out,
        }
    }

    #[test]
    fn informational_never_aborts_but_reports_failure() {
        assert_eq!(
            map_outcome(HookKind::PostBuild, &raw("", 0, false)),
            HookOutcome::Informational { failed: false }
        );
        assert_eq!(
            map_outcome(HookKind::PostBuild, &raw("", 1, false)),
            HookOutcome::Informational { failed: true }
        );
        assert_eq!(
            map_outcome(HookKind::OnReload, &raw("", 0, true)),
            HookOutcome::Informational { failed: true },
            "a timeout is a failure even for informational hooks"
        );
    }

    #[test]
    fn pre_deploy_is_the_one_veto() {
        assert_eq!(
            map_outcome(HookKind::PreDeploy, &raw("", 0, false)),
            HookOutcome::Veto(VetoDecision::Allow)
        );
        assert_eq!(
            map_outcome(HookKind::PreDeploy, &raw("", 7, false)),
            HookOutcome::Veto(VetoDecision::Veto { timed_out: false })
        );
        assert_eq!(
            map_outcome(HookKind::PreDeploy, &raw("", 0, true)),
            HookOutcome::Veto(VetoDecision::Veto { timed_out: true }),
            "a timeout aborts the deploy and is flagged as a timeout for the frame"
        );
    }

    #[test]
    fn doctor_check_applies_the_a_to_d_precedence() {
        // (a) exit 0 + line ⇒ line wins.
        let line = r#"{"symbol":"warn","message":"creds missing"}"#;
        assert!(matches!(
            map_outcome(HookKind::DoctorCheck, &raw(line, 0, false)),
            HookOutcome::Doctor(crate::hooks::outcome::DoctorResolution::Line(_))
        ));
        // (d) timeout ⇒ generic fail.
        assert_eq!(
            map_outcome(HookKind::DoctorCheck, &raw("", 0, true)),
            HookOutcome::Doctor(crate::hooks::outcome::DoctorResolution::GenericFail)
        );
    }

    #[test]
    fn new_template_choice_omitted_on_failure() {
        // Clean exit + an abs path ⇒ a choice.
        assert_eq!(
            map_outcome(HookKind::NewTemplate, &raw("/opt/tpl\n", 0, false)),
            HookOutcome::Template(Some(TemplateChoice::Path("/opt/tpl".into())))
        );
        // Nonzero exit ⇒ no choice, even with a valid-looking line.
        assert_eq!(
            map_outcome(HookKind::NewTemplate, &raw("/opt/tpl\n", 1, false)),
            HookOutcome::Template(None)
        );
        // Timeout ⇒ no choice.
        assert_eq!(
            map_outcome(HookKind::NewTemplate, &raw("/opt/tpl\n", 0, true)),
            HookOutcome::Template(None)
        );
    }

    // ---- the subprocess body (Unix only) ----

    #[cfg(unix)]
    mod subprocess {
        use super::*;
        use std::os::unix::fs::PermissionsExt;
        use std::path::Path;
        use tempfile::TempDir;

        /// Write an executable shell script at `<dir>/<name>` and return a project-rooted
        /// ResolvedHook pointing at it (relative command resolved off the project root).
        fn script_hook(
            dir: &Path,
            name: &str,
            body: &str,
            kind: HookKind,
            timeout_ms: u64,
        ) -> ResolvedHook {
            let path = dir.join(name);
            std::fs::write(&path, body).unwrap();
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
            ResolvedHook {
                source: HookSource::Project {
                    project_root: dir.to_path_buf(),
                },
                kind,
                command: name.to_string(),
                timeout_ms,
            }
        }

        fn post_build_payload() -> HookPayload {
            HookPayload::PostBuild(PostBuildPayload {
                project_dir: "/p".to_string(),
                manifest_toml: json!({ "extension": { "name": "x" } }),
                bundle_path: Some("/p/dist/extension.js".to_string()),
                build_hash: "h".to_string(),
                kind: "extension".to_string(),
                release: false,
            })
        }

        fn pre_deploy_payload() -> HookPayload {
            HookPayload::PreDeploy(PreDeployPayload {
                project_dir: "/p".to_string(),
                manifest_toml: json!({}),
                bundle_path: "/p/dist/extension.js".to_string(),
                user_library: "/ul".to_string(),
                slug: "x".to_string(),
            })
        }

        /// Count live processes whose command line contains `needle` (a PID-free orphan
        /// check). Used to assert a hung hook leaves NO process behind after the timeout.
        fn process_count(needle: &str) -> usize {
            let out = std::process::Command::new("ps")
                .args(["-A", "-o", "command"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| l.contains(needle))
                .count()
        }

        #[test]
        fn success_post_build_is_informational_not_failed() {
            let tmp = TempDir::new().unwrap();
            let hook = script_hook(
                tmp.path(),
                "pb",
                "#!/bin/sh\ncat >/dev/null\nexit 0\n",
                HookKind::PostBuild,
                5_000,
            );
            let out = run_hook(&hook, &post_build_payload(), &ctx()).unwrap();
            assert_eq!(out, HookOutcome::Informational { failed: false });
        }

        #[test]
        fn nonzero_post_build_is_informational_failed_never_errors() {
            let tmp = TempDir::new().unwrap();
            let hook = script_hook(
                tmp.path(),
                "pb",
                "#!/bin/sh\ncat >/dev/null\nexit 3\n",
                HookKind::PostBuild,
                5_000,
            );
            // An informational failure NEVER returns Err — it is logged + skipped.
            let out = run_hook(&hook, &post_build_payload(), &ctx()).unwrap();
            assert_eq!(out, HookOutcome::Informational { failed: true });
        }

        #[test]
        fn crashing_hook_is_a_failure_not_a_panic() {
            let tmp = TempDir::new().unwrap();
            // `kill -SEGV $$` — a crash with no clean exit code.
            let hook = script_hook(
                tmp.path(),
                "crash",
                "#!/bin/sh\ncat >/dev/null\nkill -SEGV $$\n",
                HookKind::PreDeploy,
                5_000,
            );
            let out = run_hook(&hook, &pre_deploy_payload(), &ctx()).unwrap();
            assert_eq!(
                out,
                HookOutcome::Veto(VetoDecision::Veto { timed_out: false }),
                "a crash is a nonzero exit ⇒ a veto for pre_deploy"
            );
        }

        #[test]
        fn pre_deploy_clean_exit_allows() {
            let tmp = TempDir::new().unwrap();
            let hook = script_hook(
                tmp.path(),
                "pd",
                "#!/bin/sh\ncat >/dev/null\nexit 0\n",
                HookKind::PreDeploy,
                5_000,
            );
            let out = run_hook(&hook, &pre_deploy_payload(), &ctx()).unwrap();
            assert_eq!(out, HookOutcome::Veto(VetoDecision::Allow));
        }

        #[test]
        fn hanging_hook_is_reaped_by_the_timeout_within_budget_no_orphan() {
            let tmp = TempDir::new().unwrap();
            // A unique marker so we can ps-assert no orphan survives.
            let marker = format!("rk-hang-marker-{}", std::process::id());
            // The hook NEVER exits and never reads stdin to EOF: `sleep 600` while echoing
            // the marker into its own argv via a comment is not visible in ps, so we sleep
            // under a uniquely named copy is overkill — instead embed the marker in a no-op
            // arg to sleep is not possible; we use a long-named subprocess via `exec -a`.
            let body = format!("#!/bin/sh\n# {marker}\nexec -a {marker} sleep 600\n");
            let hook = script_hook(tmp.path(), "hang", &body, HookKind::PreDeploy, 300);

            let start = std::time::Instant::now();
            let out = run_hook(&hook, &pre_deploy_payload(), &ctx()).unwrap();
            let elapsed = start.elapsed();

            // It is a timed-out veto.
            assert_eq!(
                out,
                HookOutcome::Veto(VetoDecision::Veto { timed_out: true })
            );
            // Returned within the timeout + grace + slack (300ms timeout + 5s grace budget;
            // the sleep is SIGTERM-killed immediately so we should be well under that).
            assert!(
                elapsed < std::time::Duration::from_secs(7),
                "engine must return within the timeout budget, took {elapsed:?}"
            );
            // No orphan: the uniquely-named sleep must be gone after the reap.
            // Give the OS a beat to finish reaping the killed group.
            std::thread::sleep(std::time::Duration::from_millis(200));
            assert_eq!(
                process_count(&marker),
                0,
                "the hung hook (and its process group) must be reaped — no orphan"
            );
        }

        #[test]
        fn timeout_override_is_honored() {
            let tmp = TempDir::new().unwrap();
            // Sleeps 5s but the timeout is 200ms ⇒ it must be killed fast.
            let hook = script_hook(
                tmp.path(),
                "slow",
                "#!/bin/sh\ncat >/dev/null\nsleep 5\n",
                HookKind::PostBuild,
                200,
            );
            let start = std::time::Instant::now();
            let out = run_hook(&hook, &post_build_payload(), &ctx()).unwrap();
            assert_eq!(out, HookOutcome::Informational { failed: true });
            assert!(
                start.elapsed() < std::time::Duration::from_secs(4),
                "the 200ms override must fire well before the 5s sleep finishes"
            );
        }

        #[test]
        fn eof_framing_lets_a_read_to_eof_hook_terminate_naturally() {
            let tmp = TempDir::new().unwrap();
            // The hook reads ALL of stdin to EOF then exits 0. If stdin were not closed it
            // would block forever and hit the timeout; a clean exit proves EOF was sent.
            let hook = script_hook(
                tmp.path(),
                "eof",
                "#!/bin/sh\ncat >/dev/null\nexit 0\n",
                HookKind::PostBuild,
                5_000,
            );
            let start = std::time::Instant::now();
            let out = run_hook(&hook, &post_build_payload(), &ctx()).unwrap();
            assert_eq!(out, HookOutcome::Informational { failed: false });
            assert!(
                start.elapsed() < std::time::Duration::from_secs(2),
                "a read-to-EOF hook must terminate naturally, not time out"
            );
        }

        #[test]
        fn env_contract_and_payload_are_present_inside_the_hook() {
            let tmp = TempDir::new().unwrap();
            let out_file = tmp.path().join("dump.txt");
            // The hook dumps the env var + the stdin payload to a file we then assert on.
            let body = format!(
                "#!/bin/sh\n\
                 printf 'API=%s\\n' \"$RACKABEL_HOOK_API\" > {out}\n\
                 printf 'PLUGIN_API=%s\\n' \"$RACKABEL_PLUGIN_API\" >> {out}\n\
                 printf 'RK=%s\\n' \"$RACKABEL\" >> {out}\n\
                 printf 'STDIN=' >> {out}\n\
                 cat >> {out}\n\
                 printf '\\n' >> {out}\n\
                 exit 0\n",
                out = out_file.display()
            );
            let hook = script_hook(tmp.path(), "envdump", &body, HookKind::PostBuild, 5_000);
            run_hook(&hook, &post_build_payload(), &ctx()).unwrap();

            let dump = std::fs::read_to_string(&out_file).unwrap();
            // RACKABEL_HOOK_API is set to the tier-3 contract integer.
            assert!(
                dump.contains(&format!("API={}", super::super::super::HOOK_API)),
                "RACKABEL_HOOK_API must be present: {dump}"
            );
            // The §5.2 contract vars are ALSO present (additive).
            assert!(dump.contains("PLUGIN_API=1"), "RACKABEL_PLUGIN_API present");
            assert!(dump.contains("RK="), "RACKABEL present");
            // The stdin payload is the exact JSON object (manifest_toml is an object).
            let stdin_line = dump
                .lines()
                .find(|l| l.starts_with("STDIN="))
                .unwrap()
                .trim_start_matches("STDIN=");
            let v: serde_json::Value = serde_json::from_str(stdin_line).unwrap();
            assert_eq!(v["kind"], "extension");
            assert!(v["manifest_toml"].is_object());
            assert_eq!(v["bundle_path"], "/p/dist/extension.js");
        }

        #[test]
        fn huge_stdout_does_not_wedge_the_engine() {
            let tmp = TempDir::new().unwrap();
            // Emit ~1MB on stdout — more than a pipe buffer — to prove the drain thread
            // keeps the child from blocking on a full pipe.
            let hook = script_hook(
                tmp.path(),
                "huge",
                "#!/bin/sh\ncat >/dev/null\nyes rackabel | head -n 100000\nexit 0\n",
                HookKind::PostBuild,
                10_000,
            );
            let out = run_hook(&hook, &post_build_payload(), &ctx()).unwrap();
            // post_build ignores stdout entirely; a clean exit is a clean informational run.
            assert_eq!(out, HookOutcome::Informational { failed: false });
        }

        #[test]
        fn garbage_stdout_doctor_is_generic_pass_or_fail_not_a_crash() {
            let tmp = TempDir::new().unwrap();
            // Non-contract stdout + exit 0 ⇒ a silent pass (combination c).
            let hook = script_hook(
                tmp.path(),
                "dc",
                "#!/bin/sh\ncat >/dev/null\necho 'not json at all {{{'\nexit 0\n",
                HookKind::DoctorCheck,
                5_000,
            );
            let payload = HookPayload::DoctorCheck(DoctorCheckPayload {
                project_dir: Some("/p".to_string()),
                manifest_toml: Some(json!({})),
            });
            let out = run_hook(&hook, &payload, &ctx()).unwrap();
            assert_eq!(
                out,
                HookOutcome::Doctor(crate::hooks::outcome::DoctorResolution::Pass)
            );
        }

        #[test]
        fn doctor_line_on_stdout_wins_over_nonzero_exit() {
            let tmp = TempDir::new().unwrap();
            // (b) nonzero + a valid line ⇒ the line wins.
            let hook = script_hook(
                tmp.path(),
                "dc",
                "#!/bin/sh\ncat >/dev/null\necho '{\"symbol\":\"fail\",\"message\":\"boom\"}'\nexit 1\n",
                HookKind::DoctorCheck,
                5_000,
            );
            let payload = HookPayload::DoctorCheck(DoctorCheckPayload {
                project_dir: None,
                manifest_toml: None,
            });
            let out = run_hook(&hook, &payload, &ctx()).unwrap();
            match out {
                HookOutcome::Doctor(crate::hooks::outcome::DoctorResolution::Line(l)) => {
                    assert_eq!(l.symbol, crate::hooks::outcome::DoctorSymbol::Fail);
                    assert_eq!(l.message, "boom");
                }
                other => panic!("expected the stdout line to win, got {other:?}"),
            }
        }

        #[test]
        fn new_template_emits_a_choice_from_stdout() {
            let tmp = TempDir::new().unwrap();
            let hook = script_hook(
                tmp.path(),
                "tpl",
                "#!/bin/sh\ncat >/dev/null\necho gh:acme/starter@v2\nexit 0\n",
                HookKind::NewTemplate,
                5_000,
            );
            let payload = HookPayload::NewTemplate(NewTemplatePayload {
                kind: "extension".to_string(),
            });
            let out = run_hook(&hook, &payload, &ctx()).unwrap();
            assert_eq!(
                out,
                HookOutcome::Template(Some(TemplateChoice::Ref("gh:acme/starter@v2".to_string())))
            );
        }

        #[test]
        fn on_reload_payload_runs_and_is_informational() {
            let tmp = TempDir::new().unwrap();
            let hook = script_hook(
                tmp.path(),
                "or",
                "#!/bin/sh\ncat >/dev/null\nexit 0\n",
                HookKind::OnReload,
                5_000,
            );
            let payload = HookPayload::OnReload(OnReloadPayload {
                project_dir: "/p".to_string(),
                manifest_toml: json!({}),
                name: "x".to_string(),
                reload_ms: 42,
                ok: true,
            });
            let out = run_hook(&hook, &payload, &ctx()).unwrap();
            assert_eq!(out, HookOutcome::Informational { failed: false });
        }

        #[test]
        fn missing_command_is_a_framed_spawn_error() {
            let tmp = TempDir::new().unwrap();
            let hook = ResolvedHook {
                source: HookSource::Project {
                    project_root: tmp.path().to_path_buf(),
                },
                kind: HookKind::PostBuild,
                command: "does-not-exist".to_string(),
                timeout_ms: 5_000,
            };
            let err = run_hook(&hook, &post_build_payload(), &ctx()).unwrap_err();
            assert_eq!(err.code, ErrorCode::HookFailed);
        }
    }
}
