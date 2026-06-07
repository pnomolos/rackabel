//! The managed dev host (DESIGN §3) — shared surface for the five 0.3 agents.
//!
//! FOUNDATION-OWNED. This module is the frozen contract the daemon-core, watch-loop,
//! registry, logs, and dev-test agents fill against (SPEC D §3/§4). It holds:
//!   - the shared wire/state types (`RegistryEntry`, `HostState`, `ExtStatus`,
//!     `DevEvent`, `SourceLoc`, …) used across the IPC boundary and `dev status`;
//!   - `DEV_PROTOCOL_VERSION`, the control-socket protocol version;
//!   - the per-Live path helpers (`sock_path`, `pid_path`, `log_dir`) under
//!     `RACKABEL_HOME`, keyed by a short hash of the canonicalized Live app path so
//!     one daemon serves one Live install (multi-Live → distinct sockets, §3.6);
//!   - re-exports of the submodule surface.
//!
//! Every type that crosses the socket derives serde and is `#[serde]`-tagged so the
//! wire form is stable and self-describing. The submodules (`daemon`, `host`,
//! `watch`, `registry`, `logs`, `ipc`) are each *exclusively owned* by one agent; the
//! foundation lands compiling stubs that return a clear not-implemented frame so the
//! whole tree builds while the agents work in parallel.
//!
//! The module is reached only via `#[cfg(unix)] mod dev;` in `main.rs` (the daemon
//! mechanics are Unix-only), so no inner `cfg` gate is needed here.

pub mod daemon;
pub mod host;
pub mod ipc;
pub mod logs;
pub mod registry;
pub mod watch;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};

/// The newline-delimited-JSON control protocol version (SPEC D §2). Every wire
/// struct carries it (defaulting to this) and the peer rejects a value it doesn't
/// understand with `RK0308 ProtocolMismatch`.
pub const DEV_PROTOCOL_VERSION: u32 = 1;

/// The reserved `dev` verb set. A registry name may never equal one of these (a verb
/// always wins the bare-`dev` parse), so `register` auto-disambiguates a colliding
/// name. Kept here (not just in `registry`) so the clap surface and the registry
/// agree on one list. `ls` is an alias of `list` and is included so it can't be
/// taken as a name either.
pub const DEV_VERBS: &[&str] = &[
    "start",
    "stop",
    "status",
    "register",
    "unregister",
    "enable",
    "disable",
    "list",
    "ls",
    "watch",
    "reload",
    "logs",
    "test",
];

// --- shared types (SPEC D §4) ---------------------------------------------------

/// Where an entry's watched/loaded bundle lives: the repo `dist/` (the common dev
/// case) or the already-`deployed` copy in the User Library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Dist,
    Deployed,
}

fn default_source() -> Source {
    Source::Dist
}

fn default_true() -> bool {
    true
}

/// One persisted registry entry (`registry.toml` `[[extension]]`). The `name` is the
/// unique, addressable handle (`dev logs <name>`, `dev --only <name>`); `path` is the
/// project root that bears the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Unique name; how the entry is addressed everywhere.
    pub name: String,
    /// Project root (the manifest-bearing dir).
    pub path: PathBuf,
    /// Watch the repo `dist/` or the deployed User-Library copy.
    #[serde(default = "default_source")]
    pub source: Source,
    /// Registered-and-active (`true`) vs registered-but-dormant (`false`).
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// The daemon's view of the host process, as reported over IPC and by `dev status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum HostState {
    /// Launching; not yet confirmed connected to Live.
    Starting,
    /// Connected and serving. `api_version` is read from the host's startup banner.
    Running {
        pid: i32,
        since_ms: u64,
        api_version: String,
    },
    /// A reload is in flight (old host stopping / new host starting).
    Reloading,
    /// The host exited unexpectedly; awaiting a (possibly backed-off) respawn.
    Crashed { code: Option<i32>, at_ms: u64 },
    /// The host crash-looped past the bounded window; awaits explicit reload/start.
    CrashLooping { attempts: u32 },
    /// No host process (clean stop, or never started).
    Stopped,
}

/// Per-extension lifecycle stage surfaced by `dev status` / `dev list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lifecycle {
    /// In the registry, not yet acted on.
    Registered,
    /// Built + copied into the User Library.
    Deployed,
    /// `activate()` ran in the host.
    Loaded,
    /// `activate()` threw.
    Failed,
    /// Pre-filtered as host-incompatible (`minimumApiVersion` too high).
    Skipped,
}

/// Per-extension status row (the union of registry facts + live host state).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtStatus {
    pub name: String,
    pub path: PathBuf,
    pub enabled: bool,
    pub lifecycle: Lifecycle,
    /// Present when `lifecycle == Skipped` (e.g. `minimumApiVersion=2.0.0 > host 1.0.0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    /// Present when `lifecycle == Failed` (the mapped `activate()` throw).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A source location mapped back through a dist sourcemap (file:line:col).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLoc {
    pub file: String,
    pub line: u32,
    pub col: u32,
}

/// An event the daemon broadcasts to subscribed clients (the bare-`dev`/`watch` UI),
/// and that the log sink frames into the per-extension log. Reliably keyed by `ext`
/// name where applicable (free-form `console.*` is best-effort, §3.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum DevEvent {
    BuildStarted {
        ext: String,
    },
    BuildOk {
        ext: String,
        ms: u64,
        hash: String,
    },
    BuildFailed {
        ext: String,
        message: String,
    },
    Deployed {
        ext: String,
        ms: u64,
    },
    ReloadStarted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger: Option<String>,
    },
    ReloadDone {
        ms: u64,
        reloaded: Vec<String>,
        failed: Vec<String>,
        skipped: Vec<String>,
    },
    /// e.g. "harmonic-lens v0.3 active — 2 commands, 1 menu action".
    Liveness {
        ext: String,
        summary: String,
    },
    ActivateFailed {
        ext: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mapped: Option<SourceLoc>,
    },
    HostCrashed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<i32>,
    },
    HostCrashLooping {
        attempts: u32,
    },
}

/// A debugger endpoint for `--inspect[=host:port]` (default `127.0.0.1:9229`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inspect {
    pub host: String,
    pub port: u16,
}

impl Inspect {
    /// The §7 default endpoint when `--inspect` is given with no value.
    pub fn default_endpoint() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 9229,
        }
    }

    /// Parse a `--inspect=host:port` value. `host` defaults to `127.0.0.1`, `port` to
    /// `9229`; a bare `9229` or `host:9229` are both accepted.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(Self::default_endpoint());
        }
        if let Some((host, port)) = s.rsplit_once(':') {
            let port: u16 = port
                .parse()
                .map_err(|_| format!("invalid --inspect port: `{port}`"))?;
            let host = if host.is_empty() {
                "127.0.0.1".to_string()
            } else {
                host.to_string()
            };
            Ok(Self { host, port })
        } else if let Ok(port) = s.parse::<u16>() {
            Ok(Self {
                host: "127.0.0.1".to_string(),
                port,
            })
        } else {
            // A bare host with no port.
            Ok(Self {
                host: s.to_string(),
                port: 9229,
            })
        }
    }
}

// --- per-Live path helpers (SPEC D §1/§2) ---------------------------------------

/// The `~/.rackabel/daemon` dir (created on demand by the daemon). Holds the per-Live
/// pidfile + socket.
pub fn daemon_dir(ctx: &Ctx) -> PathBuf {
    ctx.rackabel_home.join("daemon")
}

/// The `~/.rackabel/logs` dir root. Per-extension session logs live under
/// `logs/<name>/<session>.log` (SPEC D §4 LogSink).
pub fn log_dir(ctx: &Ctx) -> PathBuf {
    ctx.rackabel_home.join("logs")
}

/// The global registry path `~/.rackabel/registry.toml`.
pub fn registry_path(ctx: &Ctx) -> PathBuf {
    ctx.rackabel_home.join("registry.toml")
}

/// The advisory registry lockfile `~/.rackabel/registry.lock`.
pub fn registry_lock_path(ctx: &Ctx) -> PathBuf {
    ctx.rackabel_home.join("registry.lock")
}

/// A short, stable hash of the canonicalized Live app path. One daemon serves one
/// Live install, so the pidfile + socket are keyed by this hash (§3.6 multi-Live).
/// Uses the std hasher over the canonical (or, if canonicalization fails, the raw)
/// path bytes — it only needs to be stable within a machine, not cryptographic.
pub fn live_hash(live_app: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let canonical = std::fs::canonicalize(live_app).unwrap_or_else(|_| live_app.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The control-socket path for the daemon serving `live_app`.
pub fn sock_path(ctx: &Ctx, live_app: &Path) -> PathBuf {
    daemon_dir(ctx).join(format!("{}.sock", live_hash(live_app)))
}

/// The pidfile path for the daemon serving `live_app`.
pub fn pid_path(ctx: &Ctx, live_app: &Path) -> PathBuf {
    daemon_dir(ctx).join(format!("{}.pid", live_hash(live_app)))
}

/// The captured-stdio file the daemon redirects the host child's stdout/stderr to.
pub fn host_out_path(ctx: &Ctx, live_app: &Path) -> PathBuf {
    daemon_dir(ctx).join(format!("{}.out", live_hash(live_app)))
}

/// A uniform "this dev-host surface isn't implemented yet" frame for the foundation
/// stubs. Each agent replaces its own stubs; until then every dev-host code path
/// fails with a clear, framed message (never a panic or a silent no-op).
pub(crate) fn not_implemented(code: ErrorCode, what: &str) -> RkError {
    RkError::of(
        code,
        format!("{what} is not implemented yet"),
        "this part of the managed dev host (DESIGN §3, milestone 0.3) is still being built",
    )
}

/// Convenience: the not-yet-implemented frame as a `CmdResult::Err`.
pub(crate) fn todo_err<T>(code: ErrorCode, what: &str) -> CmdResult<T> {
    Err(not_implemented(code, what))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_verbs_cover_the_table() {
        for v in [
            "start",
            "stop",
            "status",
            "register",
            "unregister",
            "enable",
            "disable",
            "list",
            "ls",
            "watch",
            "reload",
            "logs",
            "test",
        ] {
            assert!(DEV_VERBS.contains(&v), "missing verb {v}");
        }
    }

    #[test]
    fn live_hash_is_stable_and_keyed() {
        let a = live_hash(Path::new("/Applications/Ableton Live 12 Beta.app"));
        let b = live_hash(Path::new("/Applications/Ableton Live 12 Beta.app"));
        let c = live_hash(Path::new("/Applications/Ableton Live 12 Alpha.app"));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn inspect_parse_forms() {
        assert_eq!(Inspect::parse("").unwrap(), Inspect::default_endpoint());
        assert_eq!(
            Inspect::parse("9230").unwrap(),
            Inspect {
                host: "127.0.0.1".into(),
                port: 9230
            }
        );
        assert_eq!(
            Inspect::parse("0.0.0.0:9300").unwrap(),
            Inspect {
                host: "0.0.0.0".into(),
                port: 9300
            }
        );
        assert!(Inspect::parse("0.0.0.0:notaport").is_err());
    }

    #[test]
    fn registry_entry_round_trips_with_defaults() {
        // A minimal entry (no source/enabled) gets the documented defaults.
        let toml_src = r#"name = "foo"
path = "/x/foo"
"#;
        let e: RegistryEntry = toml::from_str(toml_src).unwrap();
        assert_eq!(e.source, Source::Dist);
        assert!(e.enabled);
    }

    #[test]
    fn host_state_serializes_tagged() {
        let s = serde_json::to_string(&HostState::Running {
            pid: 42,
            since_ms: 1000,
            api_version: "1.0.0".into(),
        })
        .unwrap();
        assert!(s.contains("\"state\":\"running\""));
        let back: HostState = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, HostState::Running { pid: 42, .. }));
    }
}
