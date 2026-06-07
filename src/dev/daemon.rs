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
use std::sync::atomic::{AtomicBool, Ordering};
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
use super::{DEV_PROTOCOL_VERSION, Inspect, RegistryEntry, host_out_path, pid_path, sock_path};

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

    // Already up? Reuse it (idempotent double-start).
    if is_running(ctx, target.app()) {
        return Ok(target);
    }
    // Reclaim a stale pidfile/socket from a previous run.
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

/// Whether a live daemon for `live_app` is up: a parseable pidfile + `kill(pid, None)`
/// alive + an understood `version` (SPEC D §1).
pub fn is_running(ctx: &Ctx, live_app: &Path) -> bool {
    match read_pidfile(ctx, live_app) {
        Some(pf) => pf.version == DEV_PROTOCOL_VERSION && pid_alive(pf.pid),
        None => false,
    }
}

/// Reclaim a stale pidfile + socket (process gone, or socket without a live pid).
fn reclaim_stale(ctx: &Ctx, live_app: &Path) {
    let pidf = pid_path(ctx, live_app);
    let sockf = sock_path(ctx, live_app);
    let alive = read_pidfile(ctx, live_app).is_some_and(|pf| pid_alive(pf.pid));
    if !alive {
        let _ = std::fs::remove_file(&pidf);
        let _ = std::fs::remove_file(&sockf);
    }
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

    // Build the shared daemon state and launch the initial host.
    let state = Arc::new(DaemonState::new(target, sink, ctx.clone()));
    state.launch_initial();

    // Spawn the crash-recovery supervisor.
    let sup_state = Arc::clone(&state);
    let supervisor = std::thread::spawn(move || sup_state.supervise());

    // Run the accept loop until shutdown.
    serve_until_shutdown(listener, Arc::clone(&state));

    // Shutdown: stop the host, join the supervisor, unlink socket + pidfile.
    state.stop_host();
    let _ = supervisor.join();
    let _ = std::fs::remove_file(&params.sock);
    let _ = std::fs::remove_file(pid_path(ctx, pf.live_app.as_path()));
    Ok(())
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
        let inspect = self.inspect.lock().unwrap().clone();
        let cfg = resolve::host_config(&self.target, &kept, inspect, &self.ctx);
        match Host::launch(cfg, self.sink.clone()) {
            Ok(host) => *self.host.lock().unwrap() = Some(host),
            Err(_e) => {
                // Leave host = None; status will report Stopped. The supervisor does not
                // auto-respawn a never-started host (only a crashed one).
            }
        }
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

    fn set_working_set(&self, names: Option<Vec<String>>) -> Response {
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
                        let mut g = self.host.lock().unwrap();
                        if let Some(h) = g.as_mut() {
                            let _ = h.respawn();
                        }
                    }
                    RespawnDecision::PromptTty => {
                        // The detached daemon has no TTY; treat as a respawn fallback.
                        let mut g = self.host.lock().unwrap();
                        if let Some(h) = g.as_mut() {
                            let _ = h.respawn();
                        }
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
            Request::SetWorkingSet { names } => self.set_working_set(names),
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
        match listener.accept() {
            Ok((stream, _)) => {
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

/// Handle one client connection: decode each JSON-Lines request, version-check it, and
/// dispatch to the handler. A streaming handler writes many lines then returns.
fn handle_conn(stream: UnixStream, state: Arc<DaemonState>) {
    use std::io::{BufRead, BufReader};
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let mut sink = ConnSink {
        writer,
        stop: Arc::new(AtomicBool::new(false)),
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<ipc::RequestEnvelope>(&line) {
            Ok(env) => {
                if env.v != DEV_PROTOCOL_VERSION {
                    let _ = sink.send(Response::Error {
                        code: ErrorCode::ProtocolMismatch.as_str().to_string(),
                        msg: "protocol version mismatch — restart the dev host \
                              (`rackabel dev stop && rackabel dev`)"
                            .to_string(),
                    });
                    break;
                }
                use ipc::Handler;
                state.handle(env.request, &mut sink);
                if state.shutdown.load(Ordering::SeqCst) {
                    break;
                }
            }
            Err(_) => {
                let _ = sink.send(Response::Error {
                    code: ErrorCode::ProtocolMismatch.as_str().to_string(),
                    msg: "malformed request".to_string(),
                });
            }
        }
    }
}

/// A `ResponseSink` over a raw connection's writer.
struct ConnSink {
    writer: UnixStream,
    stop: Arc<AtomicBool>,
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
}

fn conn_io(e: std::io::Error) -> RkError {
    RkError::of(ErrorCode::NoDaemon, "lost a dev host connection", "retry").raw(e.into())
}
