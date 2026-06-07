//! The detached daemon: process model, pidfile, socket server (DESIGN §3.1, SPEC H §3/§9).
//!
//! OWNED BY THE DAEMON-CORE AGENT. `dev start`/bare `dev` re-exec the current binary
//! into the hidden `__daemon` subcommand; that child `setsid()`s (becoming session +
//! process-group leader, so `killpg(daemon_pgid)` reaches the host child — the verified
//! orphan fix), redirects stdio to a per-Live capture file, writes its pidfile
//! atomically, binds the control socket, builds the (pre-filtered) registry, launches the
//! host ([`super::host::Host`]), and runs the supervisor + JSON-Lines socket server until
//! `shutdown`. Liveness uses `kill(pid, None)`; a stale pidfile/socket is reclaimed.
//! `--foreground` skips the re-exec/`setsid` (keeps the TTY) and supervises in-process.

use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::signal::kill;
use nix::unistd::{Pid, getpid, setsid};
use serde::{Deserialize, Serialize};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::ui;

use super::host::{Host, RespawnDecision, now_ms};
use super::ipc::{self, InspectorState, Request, Response, ResponseSink};
use super::logs::LogSink;
use super::registry::Registry;
use super::resolve::{self, DevTarget};
use super::{
    DEV_PROTOCOL_VERSION, Inspect, RegistryEntry, host_out_path, pid_path, sock_path,
    start_lock_path,
};

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
    /// The current Extension Host child's process-group id, recorded so a `dev start`
    /// after a `-9`'d daemon can killpg the orphaned host (finding #9/#11). The host is
    /// its own group leader (`pgid == host_pid`); `None`/`0` = no host recorded yet.
    /// Defaults for backward-compat with a pidfile written by an older daemon.
    #[serde(default)]
    pub host_pgid: Option<i32>,
}

/// The arguments the hidden `__daemon` re-exec carries (SPEC D §1).
#[derive(Debug, Clone)]
pub struct DaemonParams {
    pub live_app: PathBuf,
    pub sock: PathBuf,
    pub state_home: PathBuf,
}

/// Start (or attach to) the daemon for the resolved Live install, daemonizing via the
/// `__daemon` re-exec. Returns once the daemon is up (pidfile + socket + a `Ping`) or
/// fails framed (`RK0307`). Idempotent: a live daemon is reused.
pub fn start(ctx: &Ctx) -> CmdResult<DevTarget> {
    let target = resolve::resolve(ctx)?;
    let sock = sock_path(ctx, target.app());

    // Serialize concurrent starts for THIS Live (finding #11): hold an exclusive
    // lockfile across the is-running check + re-exec + wait-until-up, so a second
    // concurrent `dev start` observes the first daemon and reuses it instead of racing
    // to spawn its own daemon+host (leaking an unreachable orphan host). The guard is a
    // best-effort advisory `O_CREAT|O_EXCL` lock; if it can't be taken in time we still
    // proceed (degrading to the old behavior) rather than failing a legitimate start.
    let _start_guard = StartLock::acquire(&start_lock_path(ctx, target.app()));

    // Already up (identity-verified)? Reuse it (idempotent double-start).
    if is_running(ctx, target.app()) {
        return Ok(target);
    }
    // Reclaim a stale pidfile/socket (and any orphaned host) from a previous run.
    reclaim_stale(ctx, target.app());

    re_exec_daemon(ctx, &target)?;
    wait_until_up(&sock)?;
    Ok(target)
}

/// Re-exec the current binary into `__daemon` (detached). The child inherits the same
/// `ABLETON_*`/`RACKABEL_*` env, so it resolves the same Live + host_cmd seam.
fn re_exec_daemon(ctx: &Ctx, target: &DevTarget) -> CmdResult<()> {
    let exe = std::env::current_exe().map_err(|e| {
        RkError::of(
            ErrorCode::DaemonStartFailed,
            "could not locate the rackabel executable to start the dev host",
            "this is unexpected; run with --raw for details",
        )
        .raw(e.into())
    })?;
    let sock = sock_path(ctx, target.app());

    let mut cmd = std::process::Command::new(exe);
    cmd.arg("__daemon")
        .arg("--live")
        .arg(target.app())
        .arg("--sock")
        .arg(&sock)
        .arg("--state")
        .arg(&ctx.rackabel_home)
        // Detach stdio: the daemon redirects its own to the capture file once up.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());

    cmd.spawn().map(|_| ()).map_err(|e| {
        RkError::of(
            ErrorCode::DaemonStartFailed,
            "could not start the dev host daemon",
            "run `rackabel doctor` to confirm the environment, then retry",
        )
        .raw(e.into())
    })
}

/// Wait (bounded ~5s) for the daemon's pidfile + socket to appear and a `Ping` to
/// succeed; `RK0307` otherwise (SPEC D §1).
fn wait_until_up(sock: &Path) -> CmdResult<()> {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if sock.exists()
            && let Ok(mut client) = ipc::Client::connect(sock)
            && let Ok(Response::Pong { .. }) = client.call(Request::Ping)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(RkError::of(
                ErrorCode::DaemonStartFailed,
                "the dev host daemon did not come up in time",
                "run `rackabel doctor` to confirm Live + the host module + Developer \
                 Mode are ready, then retry; run with --raw for the daemon's output",
            )
            .at(sock.display().to_string()));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether a live daemon for `live_app` is up — IDENTITY-bound, not just pid-alive
/// (finding #10). A bare `kill(pid, 0)` can be fooled by PID reuse (a `-9`'d daemon
/// orphans its pidfile; the OS recycles that pid for an unrelated process). So we
/// require a successful control-socket `Ping` whose `Pong.pid` matches the pidfile's
/// pid: that proves the live process is actually OUR daemon, not a recycled stranger.
/// A pidfile whose pid is dead, whose version is unknown, or whose socket won't answer
/// a matching Pong is treated as NOT running (and reclaimed by the caller).
pub fn is_running(ctx: &Ctx, live_app: &Path) -> bool {
    match read_pidfile(ctx, live_app) {
        Some(pf) => {
            pf.version == DEV_PROTOCOL_VERSION && pid_alive(pf.pid) && ping_identity_ok(&pf)
        }
        None => false,
    }
}

/// Probe the daemon's socket with a `Ping` and confirm the `Pong` carries the pidfile's
/// own pid + a matching protocol version — the identity handshake that defeats PID reuse
/// (finding #10). Any connect/parse/mismatch failure ⇒ not our daemon.
fn ping_identity_ok(pf: &PidFile) -> bool {
    let Ok(mut client) = ipc::Client::connect(&pf.sock) else {
        return false;
    };
    matches!(
        client.call(Request::Ping),
        Ok(Response::Pong { pid, protocol_v, .. })
            if pid == pf.pid && protocol_v == DEV_PROTOCOL_VERSION
    )
}

/// Reclaim a stale pidfile + socket (process gone, identity mismatch, or a socket
/// without a verifiable daemon). When the recorded process is NOT our live daemon but a
/// leftover host process group may still be alive (a `-9`'d daemon's orphan, finding
/// #9), killpg the recorded pgid first so the next start doesn't leak it.
fn reclaim_stale(ctx: &Ctx, live_app: &Path) {
    let pidf = pid_path(ctx, live_app);
    let sockf = sock_path(ctx, live_app);
    let Some(pf) = read_pidfile(ctx, live_app) else {
        // No pidfile — just clear any stray socket.
        let _ = std::fs::remove_file(&sockf);
        return;
    };
    // Our live daemon? Leave it entirely alone.
    if pf.version == DEV_PROTOCOL_VERSION && pid_alive(pf.pid) && ping_identity_ok(&pf) {
        return;
    }
    // Not our daemon: reap the orphaned Extension Host process group recorded in the
    // pidfile (best-effort) so a `-9`'d-daemon's host doesn't survive across start
    // cycles. The host is its OWN group leader (its pgid != the daemon's), so we use the
    // recorded `host_pgid`. We only signal it when the daemon pid is dead AND the host
    // group leader is still alive (so we never killpg a recycled stranger's group).
    if !pid_alive(pf.pid)
        && let Some(hpgid) = pf.host_pgid
        && hpgid > 1
        && pid_alive(hpgid)
    {
        let _ = nix::sys::signal::killpg(Pid::from_raw(hpgid), nix::sys::signal::Signal::SIGKILL);
    }
    let _ = std::fs::remove_file(&pidf);
    let _ = std::fs::remove_file(&sockf);
}

/// The recorded daemon PID for `live_app`, if a pidfile exists (parses regardless of
/// liveness — callers verify liveness separately).
pub fn read_pid(ctx: &Ctx, live_app: &Path) -> Option<i32> {
    read_pidfile(ctx, live_app).map(|pf| pf.pid)
}

fn read_pidfile(ctx: &Ctx, live_app: &Path) -> Option<PidFile> {
    let path = pid_path(ctx, live_app);
    let text = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&text).ok()
}

/// `kill(pid, 0)`: Ok ⇒ alive, ESRCH ⇒ dead.
fn pid_alive(pid: i32) -> bool {
    matches!(kill(Pid::from_raw(pid), None), Ok(()))
}

/// Atomically write the pidfile (write temp + rename).
fn write_pidfile(ctx: &Ctx, pf: &PidFile) -> CmdResult<()> {
    let path = pid_path(ctx, &pf.live_app);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| daemon_io_err(parent, e))?;
    }
    let body = toml::to_string_pretty(pf).map_err(|e| {
        RkError::of(
            ErrorCode::DaemonStartFailed,
            "could not serialize the daemon pidfile",
            "this is a bug; please report it",
        )
        .raw(e.into())
    })?;
    let tmp = path.with_extension("pid.tmp");
    std::fs::write(&tmp, body).map_err(|e| daemon_io_err(&tmp, e))?;
    std::fs::rename(&tmp, &path).map_err(|e| daemon_io_err(&path, e))?;
    Ok(())
}

fn daemon_io_err(path: &Path, e: std::io::Error) -> RkError {
    RkError::of(
        ErrorCode::DaemonStartFailed,
        "could not write daemon state",
        "check write permissions on ~/.rackabel/daemon and retry",
    )
    .at(path.display().to_string())
    .raw(e.into())
}

// --- the __daemon entrypoint ---------------------------------------------------

/// The hidden `__daemon` entrypoint: `setsid`, redirect stdio to the capture file, write
/// the pidfile, bind the socket, build + pre-filter the registry, launch the host, and
/// run the supervisor + socket-server loop until `shutdown`. Never returns until then.
pub fn run_daemon(params: DaemonParams, ctx: &Ctx) -> CmdResult<()> {
    // Become a session + process-group leader so killpg(our pgid) reaches the host
    // child (the verified orphan fix). setsid also detaches the controlling terminal.
    let _ = setsid();
    let pid = getpid().as_raw();
    let pgid = pid; // setsid ⇒ pgid == pid.

    // Redirect our own stdout/stderr to the per-Live capture file (the host child also
    // feeds the log sink; this catches anything the daemon itself emits).
    redirect_stdio(ctx, &params.live_app);

    // Resolve the host binaries for this Live (the start side already validated them,
    // but the daemon re-resolves so it owns a complete target).
    let target = resolve::resolve(ctx)?;

    // Write the pidfile atomically before binding so a racing `start` sees us coming up.
    let pf = PidFile {
        pid,
        pgid,
        version: DEV_PROTOCOL_VERSION,
        sock: params.sock.clone(),
        live_app: target.app().to_path_buf(),
        host_module: target.eh_mod.clone(),
        eh_node: target.eh_node.clone(),
        started_at: now_ms(),
        host_pgid: None,
    };
    write_pidfile(ctx, &pf)?;

    // Bind the control socket (0600), unlinking any stale one first.
    let _ = std::fs::remove_file(&params.sock);
    let listener = UnixListener::bind(&params.sock).map_err(|e| {
        RkError::of(
            ErrorCode::DaemonStartFailed,
            "could not bind the dev host control socket",
            "remove a stale socket under ~/.rackabel/daemon and retry",
        )
        .at(params.sock.display().to_string())
        .raw(e.into())
    })?;
    set_socket_mode_0600(&params.sock);

    // The log sink for this session.
    let session = format!("{}", now_ms());
    let sink = LogSink::open(ctx, &session)?;

    // Install SIGTERM/SIGINT/SIGHUP handlers so a kill (logout, shutdown, `kill <pid>`)
    // runs the SAME host-teardown + unlink cleanup instead of dying by default action
    // and orphaning the host (finding #9). Rust Drop does NOT run on signal death, so
    // the accept loop polls a flag the handler sets and falls through to cleanup. (A
    // `kill -9` is still uncatchable by design — the next `dev start` reaps that orphan
    // via the recorded host_pgid.)
    install_shutdown_signals();

    // Build the shared daemon state and launch the initial host.
    let state = Arc::new(DaemonState::new(target, sink, ctx.clone()));
    state.launch_initial();

    // Spawn the crash-recovery supervisor.
    let sup_state = Arc::clone(&state);
    let supervisor = std::thread::spawn(move || sup_state.supervise());

    // Run the accept loop until shutdown (an IPC `Shutdown` OR a caught signal).
    serve_until_shutdown(listener, Arc::clone(&state));

    // Shutdown: mark the flag (so the supervisor stops respawning), stop the host, join
    // the supervisor, unlink socket + pidfile.
    state.shutdown.store(true, Ordering::SeqCst);
    state.stop_host();
    let _ = supervisor.join();
    let _ = std::fs::remove_file(&params.sock);
    let _ = std::fs::remove_file(pid_path(ctx, pf.live_app.as_path()));
    Ok(())
}

/// A process-global flag a signal handler sets to request shutdown (finding #9). The
/// accept loop polls it; signal handlers must touch only async-signal-safe state, so
/// this is the entire handler body.
static SIGNAL_SHUTDOWN: AtomicBool = AtomicBool::new(false);

extern "C" fn on_shutdown_signal(_sig: libc::c_int) {
    SIGNAL_SHUTDOWN.store(true, Ordering::SeqCst);
}

/// Install the catchable-shutdown signal handlers (SIGTERM/SIGINT/SIGHUP). Best-effort:
/// a failed install leaves the default action (the pre-fix behavior) for that signal.
fn install_shutdown_signals() {
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
    let action = SigAction::new(
        SigHandler::Handler(on_shutdown_signal),
        SaFlags::empty(),
        SigSet::empty(),
    );
    for sig in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
        // SAFETY: the handler only stores into a static AtomicBool (async-signal-safe).
        unsafe {
            let _ = sigaction(sig, &action);
        }
    }
}

/// `dev start --foreground`: supervise the host in-process, attached to the TTY (the
/// CI / shell-tied escape hatch, §3.1). No `setsid` (keeps the controlling terminal);
/// the host child still goes into its own process group so `killpg` works. Ctrl-C
/// (SIGINT/SIGTERM to this process) tears the host down via the `Drop`/stop path.
pub fn run_foreground(ctx: &Ctx, inspect: Option<Inspect>) -> CmdResult<()> {
    let target = resolve::resolve(ctx)?;
    super::preflight::ensure_ready(ctx)?;

    let session = format!("{}", now_ms());
    let sink = LogSink::open(ctx, &session)?;
    let state = Arc::new(DaemonState::new(target, sink, ctx.clone()));
    if let Some(ins) = inspect {
        *state.inspect.lock().unwrap() = Some(ins);
    }

    state.launch_initial();
    match state.host_state_is_running() {
        true => {
            if ctx.echo_on() {
                ui::frame::emit(ui::Symbol::Good, "dev host running (foreground)", ctx);
                println!("  press ctrl-c to stop");
            }
        }
        false => {
            state.stop_host();
            return Err(RkError::of(
                ErrorCode::HostLaunchFailed,
                "the Extension Host did not come up",
                "run `rackabel doctor`, then retry",
            ));
        }
    }

    // Supervise in the foreground (a TTY crash prompts; here we keep it simple and
    // auto-respawn with backoff like the daemon, so a non-interactive --foreground in CI
    // behaves deterministically). Blocks until the host is stopped externally.
    state.supervise();
    Ok(())
}

/// Redirect the daemon's stdout/stderr to the per-Live capture file.
fn redirect_stdio(ctx: &Ctx, live_app: &Path) {
    let path = host_out_path(ctx, live_app);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::os::unix::io::AsRawFd;
        let fd = file.as_raw_fd();
        unsafe {
            libc::dup2(fd, libc::STDOUT_FILENO);
            libc::dup2(fd, libc::STDERR_FILENO);
        }
        // Keep `file` alive for the duration: leak it (the fds now alias it).
        std::mem::forget(file);
    }
}

/// `chmod 0600` the control socket (created world-rw by default umask otherwise).
fn set_socket_mode_0600(sock: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(sock, std::fs::Permissions::from_mode(0o600));
}

// --- shared daemon state + handler --------------------------------------------

/// The daemon's shared state (behind `Arc`), reached by the socket threads + supervisor.
struct DaemonState {
    target: DevTarget,
    sink: LogSink,
    ctx: Ctx,
    host: Mutex<Option<Host>>,
    /// The full enabled registry set (re-read on each reload).
    full_set: Mutex<Vec<RegistryEntry>>,
    /// The transient working set (`SetWorkingSet`); `None` = the full enabled set.
    working_set: Mutex<Option<Vec<String>>>,
    /// The connection id that owns the current transient working set (the active
    /// watch/`dev --only` session). The working set is documented transient — "for this
    /// session" (DESIGN §3.3) — so when that connection drops we reset the scope back to
    /// the full enabled set (finding #3). `0` = no owner (the set is the full set).
    working_set_owner: AtomicU64,
    /// Monotonic source of per-connection ids (so the owning session is identifiable).
    next_conn_id: AtomicU64,
    /// Extensions dropped by the pre-filter, with reasons (for `status`).
    skipped: Mutex<Vec<(String, String)>>,
    /// The inspector endpoint, when enabled.
    inspect: Mutex<Option<Inspect>>,
    shutdown: AtomicBool,
}

impl DaemonState {
    fn new(target: DevTarget, sink: LogSink, ctx: Ctx) -> Self {
        Self {
            target,
            sink,
            ctx,
            host: Mutex::new(None),
            full_set: Mutex::new(Vec::new()),
            working_set: Mutex::new(None),
            working_set_owner: AtomicU64::new(0),
            next_conn_id: AtomicU64::new(1),
            skipped: Mutex::new(Vec::new()),
            inspect: Mutex::new(None),
            shutdown: AtomicBool::new(false),
        }
    }

    /// Read the registry, pre-filter to host-compatible entries, and launch the host.
    fn launch_initial(&self) {
        let entries = self.enabled_scoped();
        *self.full_set.lock().unwrap() = entries.clone();
        let host_api = self.host_api_version();
        let (kept, skipped) = Registry::prefilter(&entries, &host_api);
        *self.skipped.lock().unwrap() = skipped
            .iter()
            .map(|(e, r)| (e.name.clone(), r.clone()))
            .collect();
        // Build+deploy the kept `source = dist` entries into the User Library BEFORE the
        // host is pointed at the deployed bundle (DESIGN §3.3 build→deploy→reload). The
        // host reads `manifest.json` + `dist/extension.js` from the deployed copy and
        // aborts the whole process if a path is missing, so an initial deploy is the
        // trap-closing invariant on `dev start` / bare `dev`'s first sync, not just on
        // subsequent file-change reloads.
        self.deploy_kept(&kept);
        let inspect = self.inspect.lock().unwrap().clone();
        let cfg = resolve::host_config(&self.target, &kept, inspect, &self.ctx);
        match Host::launch(cfg, self.sink.clone()) {
            Ok(host) => *self.host.lock().unwrap() = Some(host),
            Err(_e) => {
                // Leave host = None; status will report Stopped. The supervisor does not
                // auto-respawn a never-started host (only a crashed one).
            }
        }
        self.record_host_pgid();
    }

    /// Build + deploy each kept `source = dist` registry entry into the User Library so the
    /// host's deployed-bundle path exists before launch/reload. Best-effort and quiet (a
    /// project-rooted `json` ctx keeps the reused 0.2 build/deploy services from emitting
    /// human frames into the captured daemon stdout); a per-entry failure is logged to the
    /// sink and skipped rather than aborting the whole launch — the host will then report
    /// that one extension as missing but its siblings still load.
    fn deploy_kept(&self, kept: &[RegistryEntry]) {
        for e in kept {
            if e.source != super::Source::Dist {
                continue; // Deployed-source entries are already in the User Library.
            }
            if let Err(err) = self.deploy_one(&e.path) {
                self.sink.host_stdout(&format!(
                    "rackabel: failed to deploy {} before host launch: {}",
                    e.name, err.problem
                ));
            }
        }
    }

    /// Build (if stale) + deploy a single project root, reusing the 0.2 services verbatim.
    fn deploy_one(&self, root: &Path) -> CmdResult<()> {
        let project = crate::manifest::Project::discover(root)?;
        let mut quiet = self.ctx.clone();
        quiet.cwd = project.root.clone();
        quiet.quiet = true;
        let args = crate::cli::DeployArgs {
            release: false,
            undo: false,
            fix: false,
            dry_run: false,
        };
        // `deploy` builds-if-stale then copies the deploy set into the User Library.
        crate::commands::deploy::run(&args, &quiet)
    }

    /// The enabled registry entries scoped by the working set (post-disambiguation
    /// names, §3.3). A working set listing names not in the registry is ignored.
    fn enabled_scoped(&self) -> Vec<RegistryEntry> {
        let reg = match Registry::load(&self.ctx) {
            Ok(r) => r,
            Err(_) => return Vec::new(),
        };
        let enabled: Vec<RegistryEntry> = reg.enabled().cloned().collect();
        match &*self.working_set.lock().unwrap() {
            Some(names) => enabled
                .into_iter()
                .filter(|e| names.iter().any(|n| n == &e.name))
                .collect(),
            None => enabled,
        }
    }

    /// The host's negotiated API version (from the running host, default 1.0.0).
    fn host_api_version(&self) -> semver::Version {
        let v = self
            .host
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.api_version().to_string())
            .unwrap_or_else(|| "1.0.0".to_string());
        semver::Version::parse(&v).unwrap_or_else(|_| semver::Version::new(1, 0, 0))
    }

    fn host_state_is_running(&self) -> bool {
        matches!(
            self.host.lock().unwrap().as_ref().map(|h| h.state()),
            Some(super::HostState::Running { .. })
        )
    }

    /// Reload: re-read + re-filter the registry, then whole-host respawn (SPEC H §2).
    fn do_reload(&self, only: Option<Vec<String>>, strict: bool) -> Response {
        // A scoped reload (`only`) sets the working set for this reload.
        if let Some(names) = only {
            *self.working_set.lock().unwrap() = Some(names);
        }
        let entries = self.enabled_scoped();
        *self.full_set.lock().unwrap() = entries.clone();
        let host_api = self.host_api_version();
        let (kept, skipped) = Registry::prefilter(&entries, &host_api);
        let skipped_named: Vec<(String, String)> = skipped
            .iter()
            .map(|(e, r)| (e.name.clone(), r.clone()))
            .collect();
        *self.skipped.lock().unwrap() = skipped_named.clone();

        // --strict: any pre-filtered skip is fatal (RK4006 / exit 1, §7).
        if strict && !skipped_named.is_empty() {
            return Response::ReloadResult {
                ok: false,
                reloaded: Vec::new(),
                failed: Vec::new(),
                skipped: skipped_named
                    .into_iter()
                    .map(|(name, reason)| ipc::SkippedExt { name, reason })
                    .collect(),
                reload_ms: 0,
                host_state: self.current_host_state(),
            };
        }

        // Ensure every kept `source = dist` entry is built + deployed before the host is
        // (re)pointed at its deployed bundle (DESIGN §3.3). The watch chain deploys before
        // it calls `reload`, so this is a cheap no-op there (build-if-stale skips a fresh
        // bundle); for `set_working_set`'s implicit reload, the manual `dev reload` hotkey,
        // and a crash-recovery respawn it is the step that puts the bundle on disk.
        self.deploy_kept(&kept);

        let inspect = self.inspect.lock().unwrap().clone();
        let cfg = resolve::host_config(&self.target, &kept, inspect, &self.ctx);

        // Reload (or launch) the host, then capture its state WHILE the lock is held and
        // release the guard before building the response — re-locking `self.host` from
        // `current_host_state()` under the held guard would self-deadlock.
        let (outcome, host_state) = {
            let mut guard = self.host.lock().unwrap();
            match guard.as_mut() {
                Some(host) => {
                    let o = host.reload(cfg);
                    let state = host.state();
                    (o, state)
                }
                None => {
                    // No host yet — launch fresh.
                    drop(guard);
                    match Host::launch(cfg, self.sink.clone()) {
                        Ok(host) => {
                            let loaded = kept.iter().map(|e| e.name.clone()).collect();
                            let state = host.state();
                            *self.host.lock().unwrap() = Some(host);
                            (
                                Ok(super::host::ReloadOutcome {
                                    ms: 0,
                                    loaded,
                                    failed: Vec::new(),
                                    skipped: Vec::new(),
                                }),
                                state,
                            )
                        }
                        Err(e) => (Err(e), super::HostState::Stopped),
                    }
                }
            }
        };

        // The reload respawned the host into a fresh process group — record its new pgid
        // so an orphan reaper after a `-9`'d daemon kills the right group (finding #9).
        self.record_host_pgid();

        match outcome {
            Ok(o) => Response::ReloadResult {
                ok: o.failed.is_empty(),
                reloaded: o.loaded,
                failed: o
                    .failed
                    .into_iter()
                    .map(|(name, error)| ipc::FailedExt { name, error })
                    .collect(),
                skipped: skipped_named
                    .into_iter()
                    .map(|(name, reason)| ipc::SkippedExt { name, reason })
                    .collect(),
                reload_ms: o.ms,
                host_state,
            },
            Err(e) => Response::Error {
                code: e.code.as_str().to_string(),
                msg: e.problem,
            },
        }
    }

    fn current_host_state(&self) -> super::HostState {
        self.host
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.state())
            .unwrap_or(super::HostState::Stopped)
    }

    /// Build the `Status` response (SPEC D §2).
    fn status_response(&self) -> Response {
        let host_state = self.current_host_state();
        let (last, p50) = {
            let g = self.host.lock().unwrap();
            match g.as_ref() {
                Some(h) => (h.last_reload_ms(), h.reload_p50_ms()),
                None => (None, None),
            }
        };
        let skipped = self.skipped.lock().unwrap().clone();
        let full = self.full_set.lock().unwrap().clone();
        let running_names: Vec<String> = match &host_state {
            super::HostState::Running { .. } | super::HostState::Reloading => {
                full.iter().map(|e| e.name.clone()).collect()
            }
            _ => Vec::new(),
        };

        let mut extensions: Vec<super::ExtStatus> = Vec::new();
        for e in &full {
            let skip = skipped.iter().find(|(n, _)| n == &e.name);
            let lifecycle = if skip.is_some() {
                super::Lifecycle::Skipped
            } else if running_names.contains(&e.name) {
                super::Lifecycle::Loaded
            } else {
                super::Lifecycle::Registered
            };
            extensions.push(super::ExtStatus {
                name: e.name.clone(),
                path: e.path.clone(),
                enabled: e.enabled,
                lifecycle,
                skip_reason: skip.map(|(_, r)| r.clone()),
                error: None,
            });
        }

        let inspector = self
            .inspect
            .lock()
            .unwrap()
            .clone()
            .map(|i| InspectorState {
                active: matches!(host_state, super::HostState::Running { .. }),
                host: i.host,
                port: i.port,
            });

        Response::Status {
            host: host_state,
            extensions,
            live_app: self.target.app().display().to_string(),
            host_module: self.target.eh_mod.display().to_string(),
            eh_node: self.target.eh_node.display().to_string(),
            dev_mode: super::preflight::dev_mode_on(),
            inspector,
            reload_ms_last: last,
            reload_ms_p50: p50,
        }
    }

    /// Toggle the inspector: restart the host with `--inspect` when enabling on a running
    /// host (§7 restart-with-announcement).
    fn set_inspect(&self, enable: bool, host: String, port: u16) -> Response {
        let new = if enable {
            Some(Inspect { host, port })
        } else {
            None
        };
        *self.inspect.lock().unwrap() = new.clone();
        let was_running = self.host_state_is_running();
        // Restart with the new inspector setting.
        let _ = self.do_reload(None, false);
        Response::Ack {
            working_set: None,
            restarted: Some(was_running),
            inspector: new.map(|i| InspectorState {
                active: self.host_state_is_running(),
                host: i.host,
                port: i.port,
            }),
        }
    }

    fn set_working_set(&self, names: Option<Vec<String>>, owner: u64) -> Response {
        // Record (or release) ownership so the set resets when the owning session drops
        // (finding #3): a scoped `Some` is owned by this connection; a `None` clears it.
        match &names {
            Some(_) => self.working_set_owner.store(owner, Ordering::SeqCst),
            None => self.working_set_owner.store(0, Ordering::SeqCst),
        }
        *self.working_set.lock().unwrap() = names.clone();
        // An implicit reload applies the new set (SPEC D §2).
        let _ = self.do_reload(None, false);
        Response::Ack {
            working_set: Some(
                self.enabled_scoped()
                    .iter()
                    .map(|e| e.name.clone())
                    .collect(),
            ),
            restarted: None,
            inspector: None,
        }
    }

    /// Rewrite the pidfile to record the CURRENT host child's process-group id (or clear
    /// it), so a `dev start` after a `-9`'d daemon can reap the orphaned host (finding
    /// #9/#11). Best-effort: a write failure just leaves the prior value.
    fn record_host_pgid(&self) {
        let host_pgid = self
            .host
            .lock()
            .unwrap()
            .as_ref()
            .map(|h| h.pgid().as_raw());
        let live_app = self.target.app();
        if let Some(mut pf) = read_pidfile(&self.ctx, live_app) {
            pf.host_pgid = host_pgid;
            let _ = write_pidfile(&self.ctx, &pf);
        }
    }

    /// A connection closed. If it owned the transient working set (the active
    /// watch/`dev --only` session), reset the scope back to the full enabled set and
    /// reload, so a later plain `dev reload` from another terminal operates on the full
    /// set rather than the dead session's scope (finding #3, DESIGN §3.3 "transient").
    fn connection_closed(&self, conn_id: u64) {
        if conn_id == 0 || self.shutdown.load(Ordering::SeqCst) {
            return;
        }
        // Atomically claim ownership-release: only the owner resets, and only once.
        if self
            .working_set_owner
            .compare_exchange(conn_id, 0, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let had_scope = self.working_set.lock().unwrap().is_some();
            *self.working_set.lock().unwrap() = None;
            if had_scope {
                let _ = self.do_reload(None, false);
            }
        }
    }

    /// Stop the host (killpg group, SIGKILL escalation).
    fn stop_host(&self) {
        if let Some(host) = self.host.lock().unwrap().as_mut() {
            host.stop();
        }
    }

    /// The crash-recovery supervisor loop (SPEC H §9 / DESIGN §3.5). Polls the host for
    /// an unexpected exit; auto-respawns with exponential backoff (non-TTY default) up to
    /// the bounded window, then marks `CrashLooping`.
    fn supervise(&self) {
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                return;
            }
            // Detect an unexpected exit.
            let exit = {
                let mut g = self.host.lock().unwrap();
                match g.as_mut() {
                    Some(h)
                        if matches!(
                            h.state(),
                            super::HostState::Running { .. } | super::HostState::Reloading
                        ) =>
                    {
                        h.poll_exit()
                    }
                    _ => None,
                }
            };
            if let Some(code) = exit {
                let decision = {
                    let mut g = self.host.lock().unwrap();
                    match g.as_mut() {
                        // Non-TTY (the daemon is detached): never prompt — auto-respawn.
                        Some(h) => h.on_child_exit(code, false),
                        None => RespawnDecision::GaveUpCrashLooping,
                    }
                };
                match decision {
                    RespawnDecision::Respawn { after_ms } => {
                        std::thread::sleep(Duration::from_millis(after_ms));
                        if self.shutdown.load(Ordering::SeqCst) {
                            return;
                        }
                        {
                            let mut g = self.host.lock().unwrap();
                            if let Some(h) = g.as_mut() {
                                let _ = h.respawn();
                            }
                        }
                        // The respawn made a new process group — re-record it.
                        self.record_host_pgid();
                    }
                    RespawnDecision::PromptTty => {
                        // The detached daemon has no TTY; treat as a respawn fallback.
                        {
                            let mut g = self.host.lock().unwrap();
                            if let Some(h) = g.as_mut() {
                                let _ = h.respawn();
                            }
                        }
                        self.record_host_pgid();
                    }
                    RespawnDecision::GaveUpCrashLooping => {
                        // Leave the host in CrashLooping; wait for an explicit reload.
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl ipc::Handler for DaemonState {
    fn handle(&self, req: Request, conn: &mut dyn ResponseSink) {
        let resp = match req {
            Request::Ping => Response::Pong {
                pid: getpid().as_raw(),
                pgid: getpid().as_raw(),
                daemon_version: env!("CARGO_PKG_VERSION").to_string(),
                protocol_v: DEV_PROTOCOL_VERSION,
            },
            Request::Status => self.status_response(),
            Request::Reload { only, strict } => self.do_reload(only, strict),
            Request::SetWorkingSet { names } => self.set_working_set(names, conn.conn_id()),
            Request::SetInspect { enable, host, port } => self.set_inspect(enable, host, port),
            Request::Logs {
                name,
                follow,
                level,
                ..
            } => {
                self.stream_logs(name, follow, level, conn);
                return;
            }
            Request::StopStream => Response::Ack {
                working_set: None,
                restarted: None,
                inspector: None,
            },
            Request::Subscribe => {
                self.stream_logs(None, true, None, conn);
                return;
            }
            Request::Shutdown => {
                let _ = conn.send(Response::Ack {
                    working_set: None,
                    restarted: None,
                    inspector: None,
                });
                self.shutdown.store(true, Ordering::SeqCst);
                // Nudge the accept loop awake (it polls the shutdown flag).
                return;
            }
        };
        let _ = conn.send(resp);
    }
}

impl DaemonState {
    /// Stream log lines to a connection: replay nothing (the LOGS agent owns history),
    /// then forward new lines until the stream ends or the host stops. For a
    /// non-following request we send a `LogEnd` immediately (history is the LOGS agent's
    /// to add); a following request blocks delivering broadcast lines.
    fn stream_logs(
        &self,
        name: Option<String>,
        follow: bool,
        level: Option<String>,
        conn: &mut dyn ResponseSink,
    ) {
        if !follow {
            let _ = conn.send(Response::LogEnd);
            return;
        }
        let rx = self.sink.subscribe();
        let want_level = level.as_deref();
        loop {
            if self.shutdown.load(Ordering::SeqCst) || conn.stop_requested() {
                // Terminate the client's stream iterator cleanly (a `StopStream`-driven
                // cancel, finding #13) so its blocking read returns instead of hanging.
                let _ = conn.send(Response::LogEnd);
                break;
            }
            match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(line) => {
                    if let Some(n) = &name
                        && line.ext.as_deref() != Some(n.as_str())
                    {
                        continue;
                    }
                    if let Some(lv) = want_level
                        && line.level.as_str() != lv
                    {
                        continue;
                    }
                    let resp = Response::LogLine {
                        ts_ms: line.ts_ms,
                        level: line.level.as_str().to_string(),
                        ext: line.ext.clone(),
                        kind: line.kind.as_str().to_string(),
                        text: line.text.clone(),
                        mapped: line.mapped.clone(),
                    };
                    if conn.send(resp).is_err() {
                        break;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    }
}

/// The accept loop: a nonblocking listener polled against the shutdown flag, one thread
/// per connection. (We do not use `ipc::serve` directly because it blocks forever; the
/// daemon must wake to honor `shutdown`.)
fn serve_until_shutdown(listener: UnixListener, state: Arc<DaemonState>) {
    listener
        .set_nonblocking(true)
        .expect("set listener nonblocking");
    while !state.shutdown.load(Ordering::SeqCst) {
        // A caught SIGTERM/SIGINT/SIGHUP requests shutdown too (finding #9): mirror it
        // into the state flag so the normal teardown path in run_daemon runs.
        if SIGNAL_SHUTDOWN.load(Ordering::SeqCst) {
            state.shutdown.store(true, Ordering::SeqCst);
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                // macOS (BSD) `accept(2)` inherits the listener's O_NONBLOCK onto the
                // accepted socket; the listener is non-blocking (so the accept loop can
                // poll the shutdown flag), which would make the per-connection blocking
                // `read_line` return `WouldBlock` after the first request and tear the
                // connection down — breaking the watch UI's persistent multi-request
                // connection. Force the accepted stream back to blocking. (Linux does not
                // inherit the flag, so this is a no-op there.)
                let _ = stream.set_nonblocking(false);
                let st = Arc::clone(&state);
                std::thread::spawn(move || handle_conn(stream, st));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break,
        }
    }
}

/// The maximum bytes a single request line may occupy before we reject the connection.
/// A request is a small JSON object; anything past this is malformed or hostile (a
/// never-newline-terminated stream that would otherwise buffer unboundedly and OOM the
/// daemon — finding #12). 64 KiB is generous for the largest legitimate request
/// (a `set_working_set` / `reload` with many names).
const MAX_REQUEST_LINE: u64 = 64 * 1024;

/// Handle one client connection: decode each JSON-Lines request, version-check it, and
/// dispatch to the handler. A streaming handler writes many lines then returns.
///
/// A dedicated reader thread owns the input half so a mid-stream `StopStream` (which
/// arrives as the NEXT request line while the connection's `stream_logs` is blocked
/// delivering log lines) can flip the in-flight stream's stop flag promptly instead of
/// hanging until the client disconnects (finding #13). The reader forwards every other
/// request to the main loop over a channel. Both the reader thread and the main loop
/// cap each line at `MAX_REQUEST_LINE` (finding #12).
fn handle_conn(stream: UnixStream, state: Arc<DaemonState>) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let conn_id = state.next_conn_id.fetch_add(1, Ordering::SeqCst);
    let stop = Arc::new(AtomicBool::new(false));
    let mut sink = ConnSink {
        writer,
        stop: Arc::clone(&stop),
        conn_id,
    };

    // The reader thread: parse request lines, set `stop` on a StopStream, and forward
    // everything else to the main loop. Channel send failure (main loop gone) ends it.
    let (tx, rx) = std::sync::mpsc::channel::<ReaderMsg>();
    let stop_for_reader = Arc::clone(&stop);
    let reader_handle = std::thread::spawn(move || {
        read_requests(stream, &tx, &stop_for_reader);
    });

    use ipc::Handler;
    while let Ok(msg) = rx.recv() {
        match msg {
            ReaderMsg::Request(env) => {
                if env.v != DEV_PROTOCOL_VERSION {
                    let _ = sink.send(Response::Error {
                        code: ErrorCode::ProtocolMismatch.as_str().to_string(),
                        msg: "protocol version mismatch — restart the dev host \
                              (`rackabel dev stop && rackabel dev`)"
                            .to_string(),
                    });
                    break;
                }
                state.handle(env.request, &mut sink);
                if state.shutdown.load(Ordering::SeqCst) {
                    break;
                }
            }
            ReaderMsg::Malformed => {
                let _ = sink.send(Response::Error {
                    code: ErrorCode::ProtocolMismatch.as_str().to_string(),
                    msg: "malformed request".to_string(),
                });
            }
            ReaderMsg::Oversize => {
                let _ = sink.send(Response::Error {
                    code: ErrorCode::ProtocolMismatch.as_str().to_string(),
                    msg: "request line too large".to_string(),
                });
                break;
            }
            ReaderMsg::Eof => break,
        }
    }

    // The client went away (or we broke out): reset any session-scoped state this
    // connection owned, then join the reader.
    state.connection_closed(conn_id);
    stop.store(true, Ordering::SeqCst);
    let _ = reader_handle.join();
}

/// A message from the per-connection reader thread to the dispatch loop.
enum ReaderMsg {
    Request(ipc::RequestEnvelope),
    Malformed,
    Oversize,
    Eof,
}

/// The reader half of a connection: read length-capped JSON-Lines, intercept
/// `StopStream` (flip `stop` so an in-flight `stream_logs` cancels promptly), and
/// forward everything else to the dispatch loop. Returns when the stream ends, a line
/// overflows, or the dispatch loop is gone.
fn read_requests(
    stream: UnixStream,
    tx: &std::sync::mpsc::Sender<ReaderMsg>,
    stop: &Arc<AtomicBool>,
) {
    use std::io::{BufRead, BufReader, Read};
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        // Cap the read so an unterminated stream can't buffer unboundedly (finding #12).
        let mut limited = (&mut reader).take(MAX_REQUEST_LINE + 1);
        let n = match limited.read_line(&mut line) {
            Ok(n) => n,
            Err(_) => {
                let _ = tx.send(ReaderMsg::Eof);
                return;
            }
        };
        if n == 0 {
            let _ = tx.send(ReaderMsg::Eof);
            return;
        }
        if n as u64 > MAX_REQUEST_LINE && !line.ends_with('\n') {
            let _ = tx.send(ReaderMsg::Oversize);
            return;
        }
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ipc::RequestEnvelope>(&line) {
            Ok(env) => {
                // Intercept a mid-stream cancel: flip the stop flag for the in-flight
                // stream WITHOUT routing through the (blocked) dispatch loop (finding #13).
                if matches!(env.request, ipc::Request::StopStream) {
                    stop.store(true, Ordering::SeqCst);
                    continue;
                }
                if tx.send(ReaderMsg::Request(env)).is_err() {
                    return;
                }
            }
            Err(_) => {
                if tx.send(ReaderMsg::Malformed).is_err() {
                    return;
                }
            }
        }
    }
}

/// A `ResponseSink` over a raw connection's writer.
struct ConnSink {
    writer: UnixStream,
    stop: Arc<AtomicBool>,
    conn_id: u64,
}

impl ResponseSink for ConnSink {
    fn send(&mut self, resp: Response) -> CmdResult<()> {
        let line = serde_json::to_string(&ipc::ResponseEnvelope::new(resp)).map_err(|e| {
            RkError::of(
                ErrorCode::ProtocolMismatch,
                "could not encode a dev host message",
                "restart the dev host",
            )
            .raw(e.into())
        })?;
        self.writer.write_all(line.as_bytes()).map_err(conn_io)?;
        self.writer.write_all(b"\n").map_err(conn_io)?;
        self.writer.flush().map_err(conn_io)?;
        Ok(())
    }

    fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn conn_id(&self) -> u64 {
        self.conn_id
    }
}

fn conn_io(e: std::io::Error) -> RkError {
    RkError::of(ErrorCode::NoDaemon, "lost a dev host connection", "retry").raw(e.into())
}

/// An advisory exclusive start lock (`O_CREAT|O_EXCL`) serializing concurrent `dev
/// start`s for one Live (finding #11). Held across the is-running check + re-exec +
/// wait-until-up, so a second start observes the first daemon and reuses it. A stale
/// lockfile (its recorded pid is dead) is reclaimed; on a timeout we proceed anyway
/// (degrading to the old behavior rather than failing a legitimate start). Released on
/// drop.
struct StartLock {
    path: Option<PathBuf>,
}

impl StartLock {
    fn acquire(path: &Path) -> StartLock {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut f) => {
                    let _ = write!(f, "{}", std::process::id());
                    return StartLock {
                        path: Some(path.to_path_buf()),
                    };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Self::is_stale(path) {
                        let _ = std::fs::remove_file(path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        // Couldn't take it — proceed without the guard (best-effort).
                        return StartLock { path: None };
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                // Any other error: skip the lock rather than block a start.
                Err(_) => return StartLock { path: None },
            }
        }
    }

    /// A lockfile is stale if its recorded pid is no longer alive.
    fn is_stale(path: &Path) -> bool {
        let Ok(text) = std::fs::read_to_string(path) else {
            return false;
        };
        match text.trim().parse::<i32>() {
            Ok(pid) => !pid_alive(pid),
            Err(_) => true,
        }
    }
}

impl Drop for StartLock {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}
