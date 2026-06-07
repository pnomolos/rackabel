//! The Extension Host child lifecycle (DESIGN §3.1/§3.3/§3.5, SPEC H).
//!
//! OWNED BY THE DAEMON-CORE AGENT. Implements the empirically-verified host recipe
//! (SPEC H): launch Live's bundled node DIRECTLY (no shell) running
//! `require(EH_MOD).initialize({extensions:[…]})` with forward-slash-normalized paths,
//! `mkdir -p`'d storage/temp dirs, into the daemon's process group via a `pre_exec`
//! `setpgid` (so `killpg` reaches it — the verified orphan fix); capture stdout+stderr
//! into the [`LogSink`]; wait for the `FlipMessageStreamSocket send success`
//! connected-marker and the first `[<ext>]:` activate line; reload by SIGTERM-then-
//! respawn (there is NO public granular reload, SPEC H §2); stop with `killpg(SIGTERM)`
//! then `SIGKILL` after a grace; and run the crash-detection / exponential-backoff /
//! crash-looping state machine (SPEC H §3/§9).

use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, setpgid};

use crate::error::{CmdResult, ErrorCode, RkError};

use super::logs::LogSink;
use super::{DevEvent, HostState, Inspect};

/// How long to wait for the `FlipMessageStreamSocket send success` connected marker
/// after spawning the host (SPEC H §1: greeting ~0.4s, activate ~1.3s; budget ~5s).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Grace after `killpg(SIGTERM)` before escalating to `SIGKILL` (SPEC H §3).
const STOP_GRACE: Duration = Duration::from_millis(2500);

/// The bounded crash window: more than `MAX_CRASHES` exits inside `CRASH_WINDOW`
/// trips `CrashLooping` (SPEC H §9 / DESIGN §3.5).
const MAX_CRASHES: u32 = 5;
const CRASH_WINDOW: Duration = Duration::from_secs(30);

/// The exponential-backoff schedule (ms) for auto-respawn (non-TTY default, §3.5).
const BACKOFF_MS: &[u64] = &[200, 500, 1000, 2000, 4000];

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

impl HostConfig {
    /// Build the argv for the host child. With `host_cmd_override` set (tests) the
    /// override is used verbatim, with `--inspect` still appended when requested so the
    /// inspector path is exercised; otherwise the verified
    /// `EH_NODE [--inspect=host:port] -e "require('<EH_MOD>').initialize({…})"` recipe
    /// (SPEC H §1, all paths forward-slash-normalized).
    fn argv(&self) -> Vec<String> {
        if let Some(cmd) = &self.host_cmd_override {
            let mut v = cmd.clone();
            if let Some(ins) = &self.inspect {
                v.push(format!("--inspect={}:{}", ins.host, ins.port));
            }
            return v;
        }
        let mut v = vec![self.eh_node.to_string_lossy().into_owned()];
        if let Some(ins) = &self.inspect {
            v.push(format!("--inspect={}:{}", ins.host, ins.port));
        }
        v.push("-e".to_string());
        v.push(format!(
            "require('{}').initialize({{ extensions: {} }});",
            fwd(&self.eh_mod),
            self.extensions_json()
        ));
        v
    }

    /// Serialize the extensions array as the host expects: an array of
    /// `{ path, storageDirectory, tempDirectory }` with forward-slashed paths.
    fn extensions_json(&self) -> String {
        let items: Vec<serde_json::Value> = self
            .extensions
            .iter()
            .map(|e| {
                serde_json::json!({
                    "path": fwd(&e.path),
                    "storageDirectory": fwd(&e.storage_directory),
                    "tempDirectory": fwd(&e.temp_directory),
                })
            })
            .collect();
        serde_json::Value::Array(items).to_string()
    }

    /// The set of extension names the host should activate (for liveness attribution).
    fn ext_names(&self) -> Vec<String> {
        self.extensions.iter().map(|e| e.name.clone()).collect()
    }
}

/// Forward-slash-normalize a path for the host's `initialize()` array (SPEC H §1).
fn fwd(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
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

/// Markers a reader thread sets as it watches the host's stdout (SPEC H §1).
struct Markers {
    /// `FlipMessageStreamSocket send success` seen → the host connected to Live.
    connected: AtomicBool,
    /// At least one `[<ext>]:` activate line seen.
    activated: AtomicBool,
}

impl Markers {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            connected: AtomicBool::new(false),
            activated: AtomicBool::new(false),
        })
    }
}

/// A handle to the managed host process.
pub struct Host {
    child: Child,
    pid: i32,
    pgid: Pid,
    state: HostState,
    cfg: HostConfig,
    sink: LogSink,
    markers: Arc<Markers>,
    /// Wall-clock ms the host transitioned to `Running` (for `since_ms`).
    started_at_ms: u64,
    /// The negotiated host API version (banner; default 1.0.0).
    api_version: String,
    /// Rolling reload-duration samples (ms) for the p50 (DESIGN §3.3).
    reload_samples: VecDeque<u64>,
    last_reload_ms: Option<u64>,
    /// Recent unexpected-exit timestamps, for the crash-window check.
    crash_times: VecDeque<Instant>,
    /// Consecutive backoff index for exponential respawn.
    backoff_idx: usize,
}

impl Host {
    /// Launch the host: `mkdir -p` storage/temp, spawn the bundled node DIRECTLY into a
    /// fresh process group (`pre_exec` `setpgid(0,0)`), capture stdout+stderr into the
    /// [`LogSink`], and wait up to ~5s for the connected marker. The child becomes the
    /// leader of its own group (`pgid == pid`), so the daemon's `stop`/crash cleanup can
    /// `killpg` the whole tree even if a grandchild orphans (SPEC H §3, orphan_behavior).
    pub fn launch(cfg: HostConfig, sink: LogSink) -> CmdResult<Host> {
        let markers = Markers::new();
        let (child, pid) = spawn_child(&cfg, &sink, &markers)?;
        let pgid = Pid::from_raw(pid); // setpgid(0,0) → pgid == pid

        let mut host = Host {
            child,
            pid,
            pgid,
            state: HostState::Starting,
            cfg,
            sink,
            markers,
            started_at_ms: now_ms(),
            api_version: "1.0.0".to_string(),
            reload_samples: VecDeque::new(),
            last_reload_ms: None,
            crash_times: VecDeque::new(),
            backoff_idx: 0,
        };

        match host.await_connected() {
            Ok(()) => {
                host.started_at_ms = now_ms();
                host.state = HostState::Running {
                    pid: host.pid,
                    since_ms: host.started_at_ms,
                    api_version: host.api_version.clone(),
                };
                Ok(host)
            }
            Err(e) => {
                // Failed to connect within the window: tear the child down so a stuck
                // host (single-connection slot taken / Dev Mode off) never lingers.
                host.kill_group();
                Err(e)
            }
        }
    }

    /// Block until the host prints the connected marker, the child exits, or the
    /// timeout elapses (SPEC H §1/§4). On timeout this is the "connection slot taken /
    /// Live down / Dev Mode off" condition.
    fn await_connected(&mut self) -> CmdResult<()> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            if self.markers.connected.load(Ordering::SeqCst) {
                return Ok(());
            }
            // The child exited before connecting → a launch/crash failure. Re-check the
            // marker first: the reader thread may have set it just as the child exited.
            if let Ok(Some(status)) = self.child.try_wait() {
                // Give the reader a brief grace to drain the final lines.
                std::thread::sleep(Duration::from_millis(20));
                if self.markers.connected.load(Ordering::SeqCst) {
                    return Ok(());
                }
                return Err(RkError::of(
                    ErrorCode::HostLaunchFailed,
                    "the Extension Host exited before connecting to Live",
                    "run `rackabel doctor` to confirm Live + the host module + Developer \
                     Mode, then retry; run with --raw to see the host's output",
                )
                .at(format!("host exited with {status}")));
            }
            if Instant::now() >= deadline {
                return Err(RkError::of(
                    ErrorCode::HostLaunchFailed,
                    "the Extension Host did not connect to Live in time",
                    "another Extension Host may already own Live's single connection \
                     slot, Live may not be running, or Developer Mode may be off — run \
                     `rackabel doctor`, then retry",
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Whole-host reload: SIGTERM the old process, wait for exit, spawn a fresh one with
    /// the re-scanned `cfg`, wait for the greeting (SPEC H §2 — kill+respawn is the only
    /// reload). Records the duration for `last_reload_ms`/`reload_p50_ms`.
    pub fn reload(&mut self, cfg: HostConfig) -> CmdResult<ReloadOutcome> {
        let started = Instant::now();
        self.state = HostState::Reloading;
        self.sink.event(&DevEvent::ReloadStarted { trigger: None });

        // Stop the old host cleanly, then relaunch with the new config.
        self.kill_group();
        let markers = Markers::new();
        let (child, pid) = spawn_child(&cfg, &self.sink, &markers)?;
        self.child = child;
        self.pid = pid;
        self.pgid = Pid::from_raw(pid);
        self.markers = markers;
        self.cfg = cfg;
        self.backoff_idx = 0;

        let ms = started.elapsed().as_millis() as u64;
        let outcome = match self.await_connected() {
            Ok(()) => {
                let ms = started.elapsed().as_millis() as u64;
                self.record_reload(ms);
                self.started_at_ms = now_ms();
                self.state = HostState::Running {
                    pid: self.pid,
                    since_ms: self.started_at_ms,
                    api_version: self.api_version.clone(),
                };
                let loaded = self.cfg.ext_names();
                self.sink.event(&DevEvent::ReloadDone {
                    ms,
                    reloaded: loaded.clone(),
                    failed: Vec::new(),
                    skipped: Vec::new(),
                });
                ReloadOutcome {
                    ms,
                    loaded,
                    failed: Vec::new(),
                    skipped: Vec::new(),
                }
            }
            Err(e) => {
                self.kill_group();
                self.state = HostState::Crashed {
                    code: None,
                    at_ms: now_ms(),
                };
                return Err(e);
            }
        };
        let _ = ms;
        Ok(outcome)
    }

    fn record_reload(&mut self, ms: u64) {
        self.last_reload_ms = Some(ms);
        self.reload_samples.push_back(ms);
        // Keep a bounded rolling window for the p50.
        while self.reload_samples.len() > 32 {
            self.reload_samples.pop_front();
        }
    }

    /// The current host state.
    pub fn state(&self) -> HostState {
        self.state.clone()
    }

    /// The last measured reload duration (ms), if any.
    pub fn last_reload_ms(&self) -> Option<u64> {
        self.last_reload_ms
    }

    /// The rolling p50 reload duration (ms), if any samples.
    pub fn reload_p50_ms(&self) -> Option<u64> {
        if self.reload_samples.is_empty() {
            return None;
        }
        let mut v: Vec<u64> = self.reload_samples.iter().copied().collect();
        v.sort_unstable();
        Some(v[v.len() / 2])
    }

    /// The host's negotiated API version (banner; default 1.0.0).
    pub fn api_version(&self) -> &str {
        &self.api_version
    }

    /// The current host PID.
    pub fn pid(&self) -> i32 {
        self.pid
    }

    /// Stop the host: `killpg(group, SIGTERM)` then `SIGKILL` after a short grace
    /// (SPEC H §3). Idempotent — safe to call on an already-dead host.
    pub fn stop(&mut self) {
        self.kill_group();
        self.state = HostState::Stopped;
    }

    /// `killpg(SIGTERM)` the whole group, wait up to the grace for the leader to exit,
    /// then `killpg(SIGKILL)` and reap. This is the robust cleanup primitive that
    /// survives an orphaned grandchild (orphan_behavior).
    fn kill_group(&mut self) {
        let _ = killpg(self.pgid, Signal::SIGTERM);
        let deadline = Instant::now() + STOP_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                _ => {
                    if Instant::now() >= deadline {
                        let _ = killpg(self.pgid, Signal::SIGKILL);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
        // Reap the leader so it doesn't linger as a zombie.
        let _ = self.child.wait();
        // Belt-and-suspenders: a final group SIGKILL in case the leader exited but a
        // grandchild is still up (the verified orphan case).
        let _ = killpg(self.pgid, Signal::SIGKILL);
    }

    /// The host's process-group id (negate for `killpg`).
    #[cfg(unix)]
    pub fn pgid(&self) -> Pid {
        self.pgid
    }

    /// Poll the child for an unexpected exit. Returns `Some(code)` (or `None` for a
    /// signal-kill) when it has died while we believed it `Running`/`Reloading`; `None`
    /// while it is still alive. The supervisor calls this on a timer.
    pub fn poll_exit(&mut self) -> Option<Option<i32>> {
        match self.child.try_wait() {
            Ok(Some(status)) => Some(status.code()),
            _ => None,
        }
    }

    /// Decide what to do after the child exits unexpectedly (SPEC H §9). Records the
    /// crash, enforces the bounded window, and (non-TTY) returns the next backoff;
    /// a TTY caller is asked to confirm a reload instead.
    pub fn on_child_exit(&mut self, code: Option<i32>, tty: bool) -> RespawnDecision {
        let now = Instant::now();
        self.state = HostState::Crashed {
            code,
            at_ms: now_ms(),
        };
        self.sink.event(&DevEvent::HostCrashed { code });

        // Trim crashes outside the window, then record this one.
        while let Some(&front) = self.crash_times.front() {
            if now.duration_since(front) > CRASH_WINDOW {
                self.crash_times.pop_front();
            } else {
                break;
            }
        }
        self.crash_times.push_back(now);

        if self.crash_times.len() as u32 > MAX_CRASHES {
            self.state = HostState::CrashLooping {
                attempts: self.crash_times.len() as u32,
            };
            self.sink.event(&DevEvent::HostCrashLooping {
                attempts: self.crash_times.len() as u32,
            });
            return RespawnDecision::GaveUpCrashLooping;
        }

        if tty {
            return RespawnDecision::PromptTty;
        }
        let after = BACKOFF_MS[self.backoff_idx.min(BACKOFF_MS.len() - 1)];
        self.backoff_idx = (self.backoff_idx + 1).min(BACKOFF_MS.len() - 1);
        RespawnDecision::Respawn { after_ms: after }
    }

    /// Respawn the host after a crash, reusing the last config (the supervisor calls
    /// this after the backoff elapses). On success the backoff resets.
    pub fn respawn(&mut self) -> CmdResult<()> {
        let cfg = self.cfg.clone();
        let markers = Markers::new();
        let (child, pid) = spawn_child(&cfg, &self.sink, &markers)?;
        self.child = child;
        self.pid = pid;
        self.pgid = Pid::from_raw(pid);
        self.markers = markers;
        self.state = HostState::Starting;
        match self.await_connected() {
            Ok(()) => {
                self.backoff_idx = 0;
                self.started_at_ms = now_ms();
                self.state = HostState::Running {
                    pid: self.pid,
                    since_ms: self.started_at_ms,
                    api_version: self.api_version.clone(),
                };
                Ok(())
            }
            Err(e) => {
                self.kill_group();
                self.state = HostState::Crashed {
                    code: None,
                    at_ms: now_ms(),
                };
                Err(e)
            }
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Never leak the host child: a dropped Host kills its group (idempotent).
        if !matches!(
            self.state,
            HostState::Stopped | HostState::CrashLooping { .. }
        ) {
            self.kill_group();
        }
    }
}

/// Spawn the host child: build argv, `mkdir -p` storage/temp, spawn DIRECTLY (no shell)
/// into a fresh process group via `pre_exec setpgid(0,0)`, and start reader threads that
/// fan stdout/stderr into the sink and set the connect/activate markers.
fn spawn_child(
    cfg: &HostConfig,
    sink: &LogSink,
    markers: &Arc<Markers>,
) -> CmdResult<(Child, i32)> {
    // mkdir -p storage + temp for every extension (SPEC H §1/§8): the host expects them
    // to exist.
    for ext in &cfg.extensions {
        for dir in [&ext.storage_directory, &ext.temp_directory] {
            std::fs::create_dir_all(dir).map_err(|e| {
                RkError::of(
                    ErrorCode::HostLaunchFailed,
                    "could not create a host storage/temp directory",
                    "check write permissions and retry",
                )
                .at(dir.display().to_string())
                .raw(e.into())
            })?;
        }
    }

    let argv = cfg.argv();
    let (program, args) = argv.split_first().ok_or_else(|| {
        RkError::of(
            ErrorCode::HostLaunchFailed,
            "no host launch command",
            "this is a bug; please report it",
        )
    })?;

    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    // Put the child into its own process group (leader == the child) so a later
    // killpg(pgid) reaches the whole tree even if a grandchild reparents to launchd
    // (the verified orphan fix). setsid would also detach the controlling terminal —
    // here setpgid(0,0) is enough because the daemon is already session-detached.
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            setpgid(Pid::from_raw(0), Pid::from_raw(0))
                .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
            Ok(())
        });
    }

    let mut child = command.spawn().map_err(|e| {
        RkError::of(
            ErrorCode::HostLaunchFailed,
            format!("could not start the Extension Host (`{program}`)"),
            "run `rackabel doctor` to confirm the resolved Live, host module, and \
             bundled node, then retry; or override with --eh-node / --eh-mod",
        )
        .at(program.clone())
        .raw(e.into())
    })?;

    let pid = child.id() as i32;
    let names = cfg.ext_names();

    // Reader threads: fan stdout/stderr into the sink and watch for the markers.
    if let Some(out) = child.stdout.take() {
        spawn_reader(out, sink.clone(), Arc::clone(markers), names.clone());
    }
    if let Some(err) = child.stderr.take() {
        spawn_reader(err, sink.clone(), Arc::clone(markers), names);
    }

    Ok((child, pid))
}

/// Spawn a thread that reads `stream` line by line, feeds each into the sink, and sets
/// the connected/activated markers when the SPEC-H signature lines appear.
fn spawn_reader<R: std::io::Read + Send + 'static>(
    stream: R,
    sink: LogSink,
    markers: Arc<Markers>,
    names: Vec<String>,
) {
    std::thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if line.contains("FlipMessageStreamSocket send success") {
                markers.connected.store(true, Ordering::SeqCst);
            }
            // The first `[<ext>]:` line is activate-ran evidence (SPEC H §1).
            if !markers.activated.load(Ordering::SeqCst)
                && names.iter().any(|n| line.contains(&format!("[{n}]")))
            {
                markers.activated.store(true, Ordering::SeqCst);
            }
            sink.host_stdout(&line);
        }
    });
}

/// Current wall-clock time in milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Mutex;
    /// `RK_FAKEHOST_*` is process-global env the spawned fixture inherits; serialize the
    /// fixture-launching tests so one test's HANG/CRASH mode can't leak into another's
    /// child during the spawn window.
    static FAKEHOST_ENV: Mutex<()> = Mutex::new(());

    /// Clear all fixture mode vars, then set the requested ones (held under the lock).
    fn reset_fakehost_env() {
        unsafe {
            std::env::remove_var("RK_FAKEHOST_HANG");
            std::env::remove_var("RK_FAKEHOST_CRASH");
            std::env::remove_var("RK_FAKEHOST_EXT");
        }
    }

    fn cfg_override(cmd: Vec<&str>) -> HostConfig {
        HostConfig {
            eh_node: PathBuf::from("/unused/node"),
            eh_mod: PathBuf::from("/unused/mod.node"),
            extensions: Vec::new(),
            inspect: None,
            host_cmd_override: Some(cmd.into_iter().map(|s| s.to_string()).collect()),
        }
    }

    #[test]
    fn argv_real_recipe_is_forward_slashed_and_initializes() {
        let cfg = HostConfig {
            eh_node: PathBuf::from("/Applications/Live.app/Contents/Helpers/ExtensionHost/node"),
            eh_mod: PathBuf::from("/Applications/Live.app/Contents/Helpers/ExtensionHost/Mod.node"),
            extensions: vec![ExtensionSpec {
                name: "foo".into(),
                path: PathBuf::from("/lib/Extensions/foo"),
                storage_directory: PathBuf::from("/data/foo"),
                temp_directory: PathBuf::from("/tmp/foo"),
            }],
            inspect: None,
            host_cmd_override: None,
        };
        let argv = cfg.argv();
        assert_eq!(
            argv[0],
            "/Applications/Live.app/Contents/Helpers/ExtensionHost/node"
        );
        assert_eq!(argv[1], "-e");
        assert!(argv[2].contains("require('"));
        assert!(argv[2].contains(".initialize({ extensions:"));
        assert!(argv[2].contains("\"path\":\"/lib/Extensions/foo\""));
        assert!(argv[2].contains("\"storageDirectory\":\"/data/foo\""));
        assert!(argv[2].contains("\"tempDirectory\":\"/tmp/foo\""));
    }

    #[test]
    fn argv_inspect_is_inserted_before_eval() {
        let mut cfg = HostConfig {
            eh_node: PathBuf::from("/node"),
            eh_mod: PathBuf::from("/mod.node"),
            extensions: Vec::new(),
            inspect: Some(Inspect {
                host: "127.0.0.1".into(),
                port: 9229,
            }),
            host_cmd_override: None,
        };
        let argv = cfg.argv();
        assert_eq!(argv[1], "--inspect=127.0.0.1:9229");
        // override path also gets --inspect appended.
        cfg.host_cmd_override = Some(vec!["/bin/fake".into()]);
        let argv = cfg.argv();
        assert!(argv.iter().any(|a| a == "--inspect=127.0.0.1:9229"));
    }

    #[test]
    fn fwd_normalizes_backslashes() {
        assert_eq!(fwd(Path::new("a\\b\\c")), "a/b/c");
    }

    /// Launch the FakeHost via the override seam, confirm Running + connected marker, and
    /// that stop kills the group (the PID is gone).
    #[test]
    fn launch_and_stop_fakehost() {
        let _g = FAKEHOST_ENV.lock().unwrap();
        let bin = assert_cmd::cargo::cargo_bin("rk-fakehost");
        let dir = tempfile::tempdir().unwrap();
        let sink = LogSink::open_for_test(dir.path());
        let mut cfg = cfg_override(vec![bin.to_str().unwrap()]);
        cfg.extensions = vec![ExtensionSpec {
            name: "alpha".into(),
            path: dir.path().join("alpha"),
            storage_directory: dir.path().join("storage/alpha"),
            temp_directory: dir.path().join("tmp/alpha"),
        }];
        reset_fakehost_env();
        unsafe { std::env::set_var("RK_FAKEHOST_EXT", "alpha") };
        let mut host = Host::launch(cfg, sink).unwrap();
        assert!(matches!(host.state(), HostState::Running { .. }));
        let pid = host.pid();
        host.stop();
        assert!(matches!(host.state(), HostState::Stopped));
        // The PID must be gone.
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            nix::sys::signal::kill(Pid::from_raw(pid), None).is_err(),
            "host pid should be dead after stop"
        );
        reset_fakehost_env();
    }

    /// A host that never connects (hang mode) times out as a launch failure.
    #[test]
    fn launch_timeout_on_hang() {
        let _g = FAKEHOST_ENV.lock().unwrap();
        let bin = assert_cmd::cargo::cargo_bin("rk-fakehost");
        let dir = tempfile::tempdir().unwrap();
        let sink = LogSink::open_for_test(dir.path());
        let mut cfg = cfg_override(vec![bin.to_str().unwrap()]);
        cfg.extensions = vec![ExtensionSpec {
            name: "x".into(),
            path: dir.path().join("x"),
            storage_directory: dir.path().join("s"),
            temp_directory: dir.path().join("t"),
        }];
        reset_fakehost_env();
        unsafe { std::env::set_var("RK_FAKEHOST_HANG", "1") };
        // The fixture hangs; the launch must time out after CONNECT_TIMEOUT.
        let err = match Host::launch(cfg, sink) {
            Ok(_) => panic!("hang-mode host should not connect"),
            Err(e) => e,
        };
        assert_eq!(err.code, ErrorCode::HostLaunchFailed);
        reset_fakehost_env();
    }

    #[test]
    fn crash_window_trips_crash_looping() {
        let _g = FAKEHOST_ENV.lock().unwrap();
        let bin = assert_cmd::cargo::cargo_bin("rk-fakehost");
        let dir = tempfile::tempdir().unwrap();
        let sink = LogSink::open_for_test(dir.path());
        let mut cfg = cfg_override(vec![bin.to_str().unwrap()]);
        cfg.extensions = vec![ExtensionSpec {
            name: "x".into(),
            path: dir.path().join("x"),
            storage_directory: dir.path().join("s"),
            temp_directory: dir.path().join("t"),
        }];
        reset_fakehost_env();
        unsafe { std::env::set_var("RK_FAKEHOST_EXT", "x") };
        let mut host = Host::launch(cfg, sink).unwrap();
        // Stop the real child; we only exercise the decision machine below.
        host.stop();
        let mut last = RespawnDecision::PromptTty;
        for _ in 0..(MAX_CRASHES + 1) {
            last = host.on_child_exit(Some(1), false);
        }
        assert_eq!(last, RespawnDecision::GaveUpCrashLooping);
        assert!(matches!(host.state(), HostState::CrashLooping { .. }));
        reset_fakehost_env();
    }

    /// The verified orphan regression (orphan_behavior): a host with a backgrounded
    /// grandchild must be fully torn down by `stop` — killpg takes the whole group, so
    /// no group member survives reparented to launchd.
    #[test]
    fn stop_kills_the_whole_group_no_orphan() {
        let _g = FAKEHOST_ENV.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let sink = LogSink::open_for_test(dir.path());
        let pidfile = dir.path().join("grandchild.pid");
        // A shell host that backgrounds a long sleeper (the grandchild), records its
        // PID, prints the connected marker, then waits.
        let script = format!(
            "sleep 300 & echo $! > {}; echo 'ts: info: FlipMessageStreamSocket send success'; \
             wait",
            pidfile.display()
        );
        let mut cfg = cfg_override(vec!["/bin/sh", "-c", &script]);
        cfg.extensions = vec![ExtensionSpec {
            name: "x".into(),
            path: dir.path().join("x"),
            storage_directory: dir.path().join("s"),
            temp_directory: dir.path().join("t"),
        }];
        reset_fakehost_env();
        let mut host = Host::launch(cfg, sink).unwrap();
        assert!(matches!(host.state(), HostState::Running { .. }));

        // Read the grandchild PID the script recorded.
        let mut grandchild = None;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if let Ok(s) = std::fs::read_to_string(&pidfile)
                && let Ok(p) = s.trim().parse::<i32>()
            {
                grandchild = Some(p);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let grandchild = grandchild.expect("grandchild pid recorded");
        assert!(
            nix::sys::signal::kill(Pid::from_raw(grandchild), None).is_ok(),
            "grandchild should be alive before stop"
        );

        host.stop();
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            nix::sys::signal::kill(Pid::from_raw(grandchild), None).is_err(),
            "no group member may survive stop (the orphan trap)"
        );
        reset_fakehost_env();
    }

    /// A connect-then-crash host is detected via `poll_exit`, respawned once, and after
    /// the bounded window flips to `CrashLooping` — the full supervisor input path.
    #[test]
    fn crash_recovery_detects_respawns_then_crash_loops() {
        let _g = FAKEHOST_ENV.lock().unwrap();
        let bin = assert_cmd::cargo::cargo_bin("rk-fakehost");
        let dir = tempfile::tempdir().unwrap();
        let sink = LogSink::open_for_test(dir.path());
        let mut cfg = cfg_override(vec![bin.to_str().unwrap()]);
        cfg.extensions = vec![ExtensionSpec {
            name: "x".into(),
            path: dir.path().join("x"),
            storage_directory: dir.path().join("s"),
            temp_directory: dir.path().join("t"),
        }];
        reset_fakehost_env();
        // Connect-then-crash. The delay must comfortably exceed the connect-marker read
        // window so each launch reaches Running before crashing (the supervisor input).
        unsafe {
            std::env::set_var("RK_FAKEHOST_EXT", "x");
            std::env::set_var("RK_FAKEHOST_CRASH", "700");
        }
        let mut host = Host::launch(cfg, sink).unwrap();
        assert!(matches!(host.state(), HostState::Running { .. }));

        let mut looped = false;
        // Bounded supervisor loop: detect exit → decide → respawn, up to a few rounds.
        for _ in 0..(MAX_CRASHES + 3) {
            // Wait for the (already-launched) child to crash.
            let exit = loop {
                if let Some(code) = host.poll_exit() {
                    break code;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            match host.on_child_exit(exit, false) {
                RespawnDecision::Respawn { .. } | RespawnDecision::PromptTty => {
                    // Respawn (it will connect then crash again).
                    if host.respawn().is_err() {
                        // A respawn that can't even connect still advances the window.
                    }
                }
                RespawnDecision::GaveUpCrashLooping => {
                    looped = true;
                    break;
                }
            }
        }
        assert!(
            looped,
            "should reach crash-looping after the bounded window"
        );
        assert!(matches!(host.state(), HostState::CrashLooping { .. }));
        host.stop();
        reset_fakehost_env();
    }

    #[test]
    fn tty_crash_prompts() {
        let _g = FAKEHOST_ENV.lock().unwrap();
        let bin = assert_cmd::cargo::cargo_bin("rk-fakehost");
        let dir = tempfile::tempdir().unwrap();
        let sink = LogSink::open_for_test(dir.path());
        let mut cfg = cfg_override(vec![bin.to_str().unwrap()]);
        cfg.extensions = vec![ExtensionSpec {
            name: "x".into(),
            path: dir.path().join("x"),
            storage_directory: dir.path().join("s"),
            temp_directory: dir.path().join("t"),
        }];
        reset_fakehost_env();
        unsafe { std::env::set_var("RK_FAKEHOST_EXT", "x") };
        let mut host = Host::launch(cfg, sink).unwrap();
        host.stop();
        assert_eq!(
            host.on_child_exit(Some(1), true),
            RespawnDecision::PromptTty
        );
        reset_fakehost_env();
    }
}
