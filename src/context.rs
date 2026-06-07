//! The resolved global context (`Ctx`) every command receives.
//!
//! `Ctx` holds the resolved global flags plus the *environment-override seams* that
//! make the tool testable: the home dir, the `RACKABEL_HOME` state root, the cwd,
//! and the six launcher `ABLETON_*` overrides. Services consult `Ctx` for these
//! before ever scanning a real machine path — that is the testability contract
//! (SPEC C §5): no service hardcodes `/Applications` or `~/Music` without an env
//! hook routed through `Ctx`.

use std::path::PathBuf;

use crate::cli::Cli;
use crate::ui::color::ColorMode;

/// Resolved globals, shared (by reference) with every command.
///
/// `Clone` so the detached daemon (DESIGN §3.1) can own a snapshot of the resolved
/// context across the `__daemon` re-exec / supervisor thread boundary (the daemon
/// reuses `Ctx`-taking services — `user_library::resolve_newest`, `host_config` — from
/// a background thread, where a borrow would not outlive the request).
#[derive(Clone)]
pub struct Ctx {
    /// `--no-input`: forbid every prompt; a branch that would prompt becomes a
    /// deterministic error (DESIGN §7).
    pub no_input: bool,
    /// `--json`: structured output requested (commands that support it honor it).
    pub json: bool,
    /// `--verbose`: show developer-facing internals.
    pub verbose: bool,
    /// `--raw`: show raw host/Node output and the error chain behind frames.
    pub raw: bool,
    /// Resolved color mode for stdout-style output.
    pub color: ColorMode,
    /// Resolved color mode for the stderr error frame.
    pub color_err: ColorMode,
    /// Current working directory (resolved once).
    pub cwd: PathBuf,
    /// The `~/.rackabel`-style state root, overridable via `RACKABEL_HOME`.
    pub rackabel_home: PathBuf,
    /// Resolved home directory (respects `$HOME`).
    pub home: PathBuf,
    /// Launcher env overrides (DESIGN §7 / SPEC B §5). Resolved here so services
    /// never read these env vars directly.
    pub ableton_app: Option<PathBuf>,
    pub ableton_user_library: Option<PathBuf>,
    pub ableton_eh_mod: Option<PathBuf>,
    pub ableton_eh_node: Option<PathBuf>,
    pub ableton_extensions_dir: Option<PathBuf>,
    pub ableton_storage_base: Option<PathBuf>,
    /// The dev-host launch-command test seam (SPEC D §6). When set (from
    /// `RACKABEL_HOST_CMD`, a space-or-`\x1f`-separated argv), the daemon runs this
    /// command verbatim as the host child instead of
    /// `EH_NODE -e require(EH_MOD).initialize(...)`, so the hermetic daemon-lifecycle
    /// tests point the daemon at the FakeHost fixture and never launch real node/Live.
    pub rackabel_host_cmd: Option<Vec<String>>,
}

impl Ctx {
    /// Build the context from parsed global flags + the environment.
    pub fn from_globals(cli: &Cli) -> Self {
        let g = &cli.globals;

        let home = home::home_dir().unwrap_or_else(|| PathBuf::from("."));
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        let rackabel_home = std::env::var_os("RACKABEL_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".rackabel"));

        // Color: NO_COLOR / non-TTY suppress. Probe the relevant streams.
        let color = if g.no_color {
            ColorMode::Never
        } else {
            ColorMode::detect(is_tty(atty_stdout()))
        };
        let color_err = if g.no_color {
            ColorMode::Never
        } else {
            ColorMode::detect(is_tty(atty_stderr()))
        };

        let env_path = |k: &str| std::env::var_os(k).map(PathBuf::from);

        Self {
            no_input: g.no_input,
            json: g.json,
            verbose: g.verbose,
            raw: g.raw,
            color,
            color_err,
            cwd,
            rackabel_home,
            home,
            ableton_app: g.live.clone().or_else(|| env_path("ABLETON_APP")),
            ableton_user_library: g
                .user_library
                .clone()
                .or_else(|| env_path("ABLETON_USER_LIBRARY")),
            ableton_eh_mod: g.eh_mod.clone().or_else(|| env_path("ABLETON_EH_MOD")),
            ableton_eh_node: g.eh_node.clone().or_else(|| env_path("ABLETON_EH_NODE")),
            ableton_extensions_dir: g
                .extensions_dir
                .clone()
                .or_else(|| env_path("ABLETON_EXTENSIONS_DIR")),
            ableton_storage_base: g
                .storage_base
                .clone()
                .or_else(|| env_path("ABLETON_STORAGE_BASE")),
            rackabel_host_cmd: parse_host_cmd(std::env::var_os("RACKABEL_HOST_CMD")),
        }
    }

    /// Whether informational echoes (inference notes, resolution lines) should be
    /// printed. Suppressed under `--json` so machine output stays clean.
    pub fn echo_on(&self) -> bool {
        !self.json
    }
}

/// Parse a `RACKABEL_HOST_CMD` value into an argv. The fields are split on the ASCII
/// unit separator (`\x1f`) if present (so a path with spaces survives), else on
/// whitespace. An empty/absent value yields `None`.
fn parse_host_cmd(raw: Option<std::ffi::OsString>) -> Option<Vec<String>> {
    let raw = raw?;
    let s = raw.to_string_lossy();
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let parts: Vec<String> = if s.contains('\u{1f}') {
        s.split('\u{1f}').map(|p| p.to_string()).collect()
    } else {
        s.split_whitespace().map(|p| p.to_string()).collect()
    };
    if parts.is_empty() { None } else { Some(parts) }
}

// --- TTY probing -----------------------------------------------------------
//
// We avoid an extra `atty`/`is-terminal` dep by going through `std::io::IsTerminal`
// (stable since 1.70). These tiny shims keep the call sites readable.

fn atty_stdout() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

fn atty_stderr() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

fn is_tty(b: bool) -> bool {
    b
}
