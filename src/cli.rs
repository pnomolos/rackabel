//! The clap CLI surface (DESIGN §2 synopses).
//!
//! Every command appears here with its section-2 flags, plus the GLOBAL flags
//! (`--no-input`, `--json` where applicable, `--verbose`, `--raw`, the six launcher
//! `ABLETON_*` overrides exposed as flags — DESIGN §7). The `dev` group (0.3) and the
//! `plugin` group (0.4) are present; `install` is a hidden alias of `deploy`. The
//! top-level allows external subcommands so an unknown token routes to a PATH-discovered
//! `rackabel-<foo>` (§5.1) — built-ins always win by construction. Command-owners fill
//! the `commands::*::run` bodies; this file is frozen.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// rackabel — build Max for Live devices and Ableton Live extensions.
#[derive(Parser)]
#[command(name = "rackabel", version, about, long_about = None)]
// `allow_external_subcommands` routes any UNKNOWN leading token (one that matches no
// built-in) plus its trailing args into the `External` catch-all (§5.1 PATH-convention
// subcommands). Built-ins ALWAYS win by construction: clap matches a declared
// subcommand first and only falls through to the external catch-all when no built-in
// claims the token (verified by `external_routing_only_for_unknown_tokens` below). So
// `rackabel dev`/`build`/… can never be shadowed by a `rackabel-dev` on PATH (§5.6).
#[command(allow_external_subcommands = true)]
pub struct Cli {
    #[command(flatten)]
    pub globals: GlobalFlags,

    #[command(subcommand)]
    pub command: Command,
}

/// The reserved top-level namespace (DESIGN §5.6). A token naming one of these always
/// resolves to the built-in and can NEVER be shadowed by a `rackabel-<name>` on PATH
/// (git/cargo behavior). PATH lookup for an external subcommand happens only for tokens
/// that match nothing here. The list grows only deliberately — it INCLUDES the planned
/// future built-ins `publish`/`login` (§8) so the upgrade-time collision detector
/// (§5.6) predates the first release that adds them, and `install` (the hidden `deploy`
/// alias). This is the single source of truth both clap and the plugin resolver consult.
pub const RESERVED_NAMESPACE: &[&str] = &[
    // current top-level verbs
    "new", "build", "deploy", "pack", "validate", "doctor", "dev", "plugin", "explain",
    "install", // the hidden `deploy` alias
    // planned future built-ins (§8) — reserved now so the §5.6 collision detector
    // exists before the release that ships them.
    "publish", "login",
];

/// Whether `name` is a reserved built-in token (§5.6). Case-sensitive (subcommands are
/// lowercase). The plugin resolver calls this before any PATH lookup so a built-in
/// always wins.
pub fn is_reserved(name: &str) -> bool {
    RESERVED_NAMESPACE.contains(&name)
}

/// Global flags honored across commands (DESIGN §7). `--no-input` is honored by
/// every command; any branch that would prompt becomes a deterministic error.
#[derive(Args, Clone, Debug, Default)]
pub struct GlobalFlags {
    /// Never prompt; fail deterministically instead of inventing an answer.
    #[arg(long, global = true)]
    pub no_input: bool,

    /// Machine-readable JSON output (where the command supports it).
    #[arg(long, global = true)]
    pub json: bool,

    /// Show developer-facing internals.
    #[arg(long, global = true)]
    pub verbose: bool,

    /// Show raw host/Node output and the underlying error chain.
    #[arg(long, global = true)]
    pub raw: bool,

    /// Disable ANSI color regardless of TTY (NO_COLOR is also honored).
    #[arg(long, global = true)]
    pub no_color: bool,

    // --- the six launcher overrides exposed as flags (DESIGN §7) ---
    /// Ableton Live `.app` to use (overrides ABLETON_APP).
    #[arg(long, global = true, value_name = "PATH")]
    pub live: Option<PathBuf>,

    /// User Library folder (overrides ABLETON_USER_LIBRARY).
    #[arg(long, global = true, value_name = "PATH")]
    pub user_library: Option<PathBuf>,

    /// Direct path to ExtensionHostNodeModule.node (overrides ABLETON_EH_MOD).
    #[arg(long, global = true, value_name = "PATH")]
    pub eh_mod: Option<PathBuf>,

    /// Node binary to run the host (overrides ABLETON_EH_NODE).
    #[arg(long, global = true, value_name = "PATH")]
    pub eh_node: Option<PathBuf>,

    /// Extensions folder to scan (overrides ABLETON_EXTENSIONS_DIR).
    #[arg(long, global = true, value_name = "PATH")]
    pub extensions_dir: Option<PathBuf>,

    /// Base dir for per-extension storage (overrides ABLETON_STORAGE_BASE).
    #[arg(long, global = true, value_name = "PATH")]
    pub storage_base: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Scaffold a project (Extension or M4L device).
    New(NewArgs),
    /// Compile/bundle the artifact (no install).
    Build(BuildArgs),
    /// Build + copy into the Live User Library (alias: install).
    Deploy(DeployArgs),
    /// Hidden alias of `deploy` (kept for the existing M4L workflow/README).
    #[command(hide = true)]
    Install(DeployArgs),
    /// Production build → distributable .ablx / .amxd.
    Pack(PackArgs),
    /// Lint manifest + artifact against ship rules.
    Validate(ValidateArgs),
    /// Diagnose the environment.
    Doctor(DoctorArgs),
    /// Long-form help for an error code (cargo --explain).
    Explain(ExplainArgs),
    /// The managed dev host: bare `dev` runs the loop; verbs control its lifecycle.
    Dev(DevArgs),
    /// Manage rackabel's own third-party plugins.
    Plugin(PluginArgs),
    /// Hidden re-exec target: the detached daemon process (DESIGN §3.1, SPEC D §1).
    /// Never invoked by a user; `dev start`/`dev` re-exec the current binary with this
    /// subcommand to become the session-leading daemon.
    #[command(name = "__daemon", hide = true)]
    Daemon(DaemonArgs),
    /// A PATH-discovered third-party subcommand `rackabel-<foo>` (DESIGN §5.1). clap
    /// routes here ONLY for a leading token that matches no built-in (the external
    /// catch-all); the first element is the `<foo>` name, the rest are forwarded
    /// verbatim. Built-ins always win, so this never captures a reserved token.
    #[command(external_subcommand)]
    External(Vec<OsString>),
}

// --- the `plugin` group (DESIGN §2 plugin table, §5.4) -------------------------

/// `rackabel plugin <verb>` — install/list/which/run/enable/disable/search (§5.4). The
/// `migrate` verb (hook-contract codemod) is a 0.5 feature and is deliberately absent.
#[derive(Args, Debug)]
pub struct PluginArgs {
    #[command(subcommand)]
    pub command: PluginCommand,
}

/// The `plugin` verb subcommands (DESIGN §5.4).
#[derive(Subcommand, Debug)]
pub enum PluginCommand {
    /// Install OWNER/REPO (release asset, else clone+run) or a path/tarball (sideload).
    Install(PluginInstallArgs),
    /// Installed plugins + enabled state + pinned ref.
    #[command(alias = "ls")]
    List,
    /// Show which file a name would run (or "shadowed by built-in").
    Which(PluginNameArgs),
    /// Run a plugin even if a built-in shadows the name, forwarding trailing args.
    Run(PluginRunArgs),
    /// Enable a plugin (and, in 0.5, its hooks).
    Enable(PluginNameArgs),
    /// Disable a plugin (and, in 0.5, its hooks).
    Disable(PluginNameArgs),
    /// Query the `rackabel-plugin` GitHub topic.
    Search(PluginSearchArgs),
}

/// `rackabel plugin install OWNER/REPO|<path|tarball> [--yes] [--json] [--force]`
#[derive(Args, Debug)]
pub struct PluginInstallArgs {
    /// `OWNER/REPO` (gh-style), or a local path/tarball to sideload.
    #[arg(value_name = "OWNER/REPO|PATH")]
    pub source: String,

    /// Skip the install confirmation (consent to fetch+run the named source in a script).
    #[arg(long)]
    pub yes: bool,

    /// Install past a `plugins.lock` pin mismatch, announcing the change (§5.4/§5.7).
    #[arg(long)]
    pub force: bool,
}

/// `rackabel plugin which|enable|disable <name> [--json]`
#[derive(Args, Debug)]
pub struct PluginNameArgs {
    /// The plugin name (the `<foo>` of a `rackabel-<foo>` executable).
    #[arg(value_name = "NAME")]
    pub name: String,
}

/// `rackabel plugin run <name> [args…]` — the §5.6 escape hatch.
#[derive(Args, Debug)]
pub struct PluginRunArgs {
    /// The plugin name to invoke even when a built-in shadows it.
    #[arg(value_name = "NAME")]
    pub name: String,

    /// Arguments forwarded verbatim to the plugin executable.
    #[arg(
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "ARGS"
    )]
    pub args: Vec<OsString>,
}

/// `rackabel plugin search <term> [--json]`
#[derive(Args, Debug)]
pub struct PluginSearchArgs {
    /// The search term (matched against the `rackabel-plugin` GitHub topic).
    #[arg(value_name = "TERM")]
    pub term: String,
}

// --- the `dev` group (DESIGN §2 dev table, §3) ---------------------------------
//
// `rackabel dev` is a hybrid: a *bare* form (the flagship loop — start-if-needed +
// watch the registered set + tail logs) AND a verb group (start/stop/status/register/
// …). clap resolves a token equal to a verb to the verb (verbs always win the parse,
// §2 name-vs-verb precedence); anything else falls through to the bare form's
// positional `[NAME… | PATH]`. `--only GLOB` and the `-- <NAME…>` separator ALWAYS
// route through the registry name matcher, never the verb table (§3.3 scoping) — so
// `rackabel dev --only test` watches the extension named `test` while `rackabel dev
// test` is the subcommand.

/// `rackabel dev [NAME… | PATH] [--only GLOB] [--no-auto-reload] [--raw]
///  [--inspect[=host:port]] [--emit-launch-config]` + the verb subcommands.
#[derive(Args, Debug)]
pub struct DevArgs {
    /// A `dev` verb, or absent for the bare flagship loop.
    #[command(subcommand)]
    pub command: Option<DevCommand>,

    /// Bare-dev working set: registry NAMEs (post-disambiguation) or a single PATH.
    /// A token equal to a verb is parsed as that verb instead (verbs win); everything
    /// after a `--` separator is treated as NAMEs (never verbs).
    #[arg(value_name = "NAME|PATH", trailing_var_arg = true)]
    pub names: Vec<String>,

    /// Restrict the watched/loaded set to entries matching this glob (a transient
    /// working set). ALWAYS matches registry names, never the verb table (§3.3).
    #[arg(long, value_name = "GLOB")]
    pub only: Option<String>,

    /// Manual-reload mode: do not auto-reload on change ([r] / `dev reload` instead).
    #[arg(long)]
    pub no_auto_reload: bool,

    /// Show unfiltered host/Node output in the inline log tail.
    #[arg(long)]
    pub raw: bool,

    /// Attach the Node inspector to the host (default 127.0.0.1:9229). Restarts a
    /// running host with the inspector enabled, announcing what it did (§7).
    #[arg(long, value_name = "HOST:PORT", num_args = 0..=1, default_missing_value = "")]
    pub inspect: Option<String>,

    /// Write a VS Code `launch.json` for attaching the debugger and exit-or-continue.
    #[arg(long)]
    pub emit_launch_config: bool,
}

/// The `dev` verb subcommands (DESIGN §2 dev table).
#[derive(Subcommand, Debug)]
pub enum DevCommand {
    /// Launch the managed Extension Host (daemonized by default).
    Start(DevStartArgs),
    /// Stop the daemon cleanly.
    Stop,
    /// Daemon + per-extension state, Live/host paths, inspector + reload metrics.
    Status,
    /// Add a path to the persistent registry.
    Register(DevRegisterArgs),
    /// Remove an entry from the registry.
    Unregister(DevUnregisterArgs),
    /// Flip a dormant entry back to enabled.
    Enable(DevNameArg),
    /// Flip an entry to disabled (registered but not loaded).
    Disable(DevNameArg),
    /// Show the registry with status columns.
    #[command(alias = "ls")]
    List,
    /// Explicit form of bare `dev` (no implicit daemon start).
    Watch(DevWatchArgs),
    /// Force a whole-host reload now.
    Reload(DevReloadArgs),
    /// Tail/filter the host's per-extension log sink.
    Logs(DevLogsArgs),
    /// Run the project's tests / headless smoke (the CI entry point, §3.8).
    Test(DevTestArgs),
}

/// `rackabel dev start [--live PATH] [--foreground] [--inspect[=host:port]] [--emit-launch-config]`
#[derive(Args, Debug)]
pub struct DevStartArgs {
    /// Run the host in the foreground, tied to this shell (no daemonize).
    #[arg(long)]
    pub foreground: bool,

    /// Attach the Node inspector (default 127.0.0.1:9229).
    #[arg(long, value_name = "HOST:PORT", num_args = 0..=1, default_missing_value = "")]
    pub inspect: Option<String>,

    /// Write a VS Code `launch.json` and exit-or-continue.
    #[arg(long)]
    pub emit_launch_config: bool,
}

/// `rackabel dev register [PATH] [--recursive] [--name NAME] [--disabled]`
#[derive(Args, Debug)]
pub struct DevRegisterArgs {
    /// Project path to register (defaults to the current directory).
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Register every manifest-bearing subdir (the monorepo case).
    #[arg(long)]
    pub recursive: bool,

    /// Override the entry name (single-path only). Mutually exclusive with
    /// `--recursive` — one name cannot label N members (rejected at parse time,
    /// exit 2, §3.2).
    #[arg(long, value_name = "NAME", conflicts_with = "recursive")]
    pub name: Option<String>,

    /// Register but leave dormant (`enabled = false`).
    #[arg(long)]
    pub disabled: bool,
}

/// `rackabel dev unregister <NAME|PATH> [--recursive]`
#[derive(Args, Debug)]
pub struct DevUnregisterArgs {
    /// The registry name or project path to remove.
    #[arg(value_name = "NAME|PATH")]
    pub target: String,

    /// Remove every entry under the given path (the recursive-register inverse).
    #[arg(long)]
    pub recursive: bool,
}

/// `rackabel dev enable|disable <NAME|PATH>`
#[derive(Args, Debug)]
pub struct DevNameArg {
    /// The registry name or project path.
    #[arg(value_name = "NAME|PATH")]
    pub target: String,
}

/// `rackabel dev watch [NAME… | PATH] [--only GLOB] [--no-auto-reload]`
#[derive(Args, Debug)]
pub struct DevWatchArgs {
    /// Working set: registry NAMEs or a single PATH (§3.3 scoping).
    #[arg(value_name = "NAME|PATH")]
    pub names: Vec<String>,

    /// Restrict to entries matching this glob (matches registry names, §3.3).
    #[arg(long, value_name = "GLOB")]
    pub only: Option<String>,

    /// Do not auto-reload on change.
    #[arg(long)]
    pub no_auto_reload: bool,
}

/// `rackabel dev reload [NAME…] [--strict] [--json]`
#[derive(Args, Debug)]
pub struct DevReloadArgs {
    /// Reload only these registry NAMEs (default: the whole loaded set).
    #[arg(value_name = "NAME")]
    pub names: Vec<String>,

    /// Treat a pre-filtered (host-incompatible) skip as fatal (exit 1).
    #[arg(long)]
    pub strict: bool,
}

/// `rackabel dev logs [NAME] [--follow] [--since 5m] [--level LEVEL] [--json] [--raw]`
#[derive(Args, Debug)]
pub struct DevLogsArgs {
    /// The registry name to filter to (default: all extensions).
    #[arg(value_name = "NAME")]
    pub name: Option<String>,

    /// Stream new lines as they arrive (Wrangler `tail`).
    #[arg(long, short = 'f')]
    pub follow: bool,

    /// Only lines newer than this (e.g. `5m`, `1h`, `30s`).
    #[arg(long, value_name = "DURATION")]
    pub since: Option<String>,

    /// Only lines at or above this level (info|warn|error).
    #[arg(long, value_name = "LEVEL")]
    pub level: Option<String>,

    /// Show unfiltered host/Node output (internals included).
    #[arg(long)]
    pub raw: bool,
}

/// `rackabel dev test [NAME… | PATH] [--bail] [--json] [-- <runner args>]`
#[derive(Args, Debug)]
pub struct DevTestArgs {
    /// Project NAMEs/PATH to test (default: the registered set / cwd project).
    #[arg(value_name = "NAME|PATH")]
    pub names: Vec<String>,

    /// Fail fast on the first failing test.
    #[arg(long)]
    pub bail: bool,

    /// Arguments forwarded verbatim to the underlying runner (vitest), after `--`.
    #[arg(last = true, value_name = "RUNNER ARGS")]
    pub runner_args: Vec<String>,
}

/// The hidden `__daemon` re-exec target's arguments (DESIGN §3.1, SPEC D §1). Carried
/// across the re-exec so the daemon child knows which Live to serve, where to bind its
/// socket, and where its state root is.
#[derive(Args, Debug)]
pub struct DaemonArgs {
    /// The resolved Live `.app` this daemon serves.
    #[arg(long, value_name = "PATH")]
    pub live: PathBuf,

    /// The control socket path to bind.
    #[arg(long, value_name = "PATH")]
    pub sock: PathBuf,

    /// The `RACKABEL_HOME` state root.
    #[arg(long, value_name = "PATH")]
    pub state: PathBuf,
}

/// The kind of project to scaffold.
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum ProjectKind {
    Extension,
    Device,
}

/// `rackabel new [NAME] [--kind …] [--template …] [--minimal] [--yes] [--no-input] [--update]`
#[derive(Args, Debug)]
pub struct NewArgs {
    /// Project name (also the directory). Prompted if omitted.
    pub name: Option<String>,

    /// What to scaffold (Extension or M4L device).
    #[arg(long, value_enum)]
    pub kind: Option<ProjectKind>,

    /// Device type for `--kind device` (audio-effect | midi-effect | instrument).
    #[arg(long, value_enum)]
    pub device_kind: Option<DeviceKindArg>,

    /// Template: `gh:user/repo`, `@scope/name`, or a local path.
    #[arg(long)]
    pub template: Option<String>,

    /// Power-user bare skeleton (fewer files, no working example).
    #[arg(long)]
    pub minimal: bool,

    /// Accept all defaults (CI).
    #[arg(long)]
    pub yes: bool,

    /// Re-run the template's 3-way merge against the current project (§5.5).
    #[arg(long)]
    pub update: bool,

    /// With `--update`: print the merge plan (which files would apply/conflict/skip)
    /// and exit, mutating nothing (§5.5).
    #[arg(long)]
    pub dry_run: bool,

    /// Where to find the gated SDK/CLI toolkit (recursively searched).
    #[arg(long, value_name = "DIR")]
    pub sdk_dir: Option<PathBuf>,

    /// Initialize a git repository (default on for non-minimal).
    #[arg(long)]
    pub git: bool,

    /// Do not initialize a git repository.
    #[arg(long, conflicts_with = "git")]
    pub no_git: bool,
}

/// Device kind for the M4L `[device]` path (mirrors the existing enum, kept here so
/// the CLI surface is self-contained and the M4L command can map from it).
#[derive(Clone, Copy, Debug, ValueEnum, PartialEq, Eq)]
pub enum DeviceKindArg {
    AudioEffect,
    MidiEffect,
    Instrument,
}

/// `rackabel build [--release] [--clean] [--typecheck] [--print-config] [--dry-run] [--json]`
#[derive(Args, Debug)]
pub struct BuildArgs {
    /// Production build: minify on, no sourcemap, typecheck on.
    #[arg(long)]
    pub release: bool,

    /// Blow away the build dir first.
    #[arg(long)]
    pub clean: bool,

    /// Force `tsc --noEmit` typecheck on.
    #[arg(long, overrides_with = "no_typecheck")]
    pub typecheck: bool,

    /// Force typecheck off.
    #[arg(long, overrides_with = "typecheck")]
    pub no_typecheck: bool,

    /// Dump the resolved esbuild config and exit.
    #[arg(long)]
    pub print_config: bool,

    /// Print the planned steps and exit, mutating nothing.
    #[arg(long)]
    pub dry_run: bool,
}

impl BuildArgs {
    /// Resolve the tri-state typecheck flag: `Some(true)`/`Some(false)`/`None`
    /// (default = on for release). Mirrors `BuildOptions::typecheck`.
    pub fn typecheck_choice(&self) -> Option<bool> {
        if self.typecheck {
            Some(true)
        } else if self.no_typecheck {
            Some(false)
        } else {
            None
        }
    }
}

/// `rackabel deploy [--user-library PATH] [--live PATH] [--release] [--undo] [--fix] [--dry-run] [--json]`
///
/// `--user-library`/`--live` are also global flags; declaring them here documents
/// them on the deploy synopsis. They resolve through `Ctx` either way.
#[derive(Args, Debug)]
pub struct DeployArgs {
    /// Production deploy: run `validate` first; fail on any validation error.
    #[arg(long)]
    pub release: bool,

    /// Remove the deployed `<UserLibrary>/Extensions/<slug>` folder rackabel created.
    #[arg(long)]
    pub undo: bool,

    /// Build any missing native deps under the hood (no pnpm jargon).
    #[arg(long)]
    pub fix: bool,

    /// Print the planned steps and exit, mutating nothing.
    #[arg(long)]
    pub dry_run: bool,
}

/// `rackabel pack [--target os-arch …] [--include GLOB …] [--output PATH] [--no-official-cli] [--dry-run] [--json]`
#[derive(Args, Debug)]
pub struct PackArgs {
    /// Cross-build target (repeatable), e.g. `darwin-arm64`.
    #[arg(long, value_name = "OS-ARCH")]
    pub target: Vec<String>,

    /// Additional files/dirs to bundle (relative, inside the extension dir).
    #[arg(long, short = 'i', value_name = "GLOB")]
    pub include: Vec<String>,

    /// Output `.ablx` path.
    #[arg(long, short = 'o', value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Force rackabel's own packer even for the pure-JS case.
    #[arg(long)]
    pub no_official_cli: bool,

    /// Print the planned steps and exit, mutating nothing.
    #[arg(long)]
    pub dry_run: bool,
}

/// `rackabel validate [--json]`
#[derive(Args, Debug)]
pub struct ValidateArgs {
    /// Treat warnings (e.g. identifier drift) as failures.
    #[arg(long)]
    pub strict: bool,
}

/// `rackabel doctor [--verbose] [--json] [--fix]`
#[derive(Args, Debug)]
pub struct DoctorArgs {
    /// Perform safe auto-fixes (vendor SDK, redeploy a stale bundle, …).
    #[arg(long)]
    pub fix: bool,
}

/// `rackabel explain <code>`
#[derive(Args, Debug)]
pub struct ExplainArgs {
    /// The error code to explain, e.g. `RK0001`.
    pub code: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// Pins the reserved namespace (§5.6): every current built-in subcommand, the
    /// hidden `install` alias, AND the planned future built-ins `publish`/`login`. If a
    /// new built-in is added, it must be added here too (this test fails otherwise),
    /// keeping the §5.6 collision detector honest.
    #[test]
    fn reserved_namespace_is_pinned() {
        let expected = [
            "new", "build", "deploy", "pack", "validate", "doctor", "dev", "plugin", "explain",
            "install", "publish", "login",
        ];
        assert_eq!(RESERVED_NAMESPACE, &expected);
        for token in expected {
            assert!(is_reserved(token), "expected `{token}` reserved");
        }
        assert!(!is_reserved("foo"));
        // case-sensitive: subcommands are lowercase
        assert!(!is_reserved("Build"));
    }

    /// Every clap subcommand that takes the namespace (visible OR hidden, excluding the
    /// `__daemon` re-exec target and the external catch-all) must be in
    /// RESERVED_NAMESPACE, and `publish`/`login` must be reserved ahead of shipping.
    #[test]
    fn every_builtin_is_reserved() {
        let cmd = Cli::command();
        for sub in cmd.get_subcommands() {
            let name = sub.get_name();
            if name == "__daemon" {
                continue; // the hidden re-exec target is not a user namespace token
            }
            assert!(
                is_reserved(name),
                "built-in subcommand `{name}` is not in RESERVED_NAMESPACE"
            );
            // aliases (e.g. `ls` for `plugin list`) live under their parent, not here.
        }
    }

    /// Built-ins always win: clap routes a KNOWN token to its subcommand and only a
    /// truly-unknown token falls through to the `External` catch-all (§5.1/§5.6).
    #[test]
    fn external_routing_only_for_unknown_tokens() {
        // A known built-in parses as that built-in, never External.
        let cli = Cli::try_parse_from(["rackabel", "build"]).unwrap();
        assert!(matches!(cli.command, Command::Build(_)));
        // `plugin` is a built-in group.
        let cli = Cli::try_parse_from(["rackabel", "plugin", "list"]).unwrap();
        assert!(matches!(cli.command, Command::Plugin(_)));
        // An unknown token routes to External with its trailing args verbatim.
        let cli = Cli::try_parse_from(["rackabel", "frobnicate", "--wild", "x"]).unwrap();
        match cli.command {
            Command::External(argv) => {
                assert_eq!(argv[0], "frobnicate");
                assert_eq!(argv[1], "--wild");
                assert_eq!(argv[2], "x");
            }
            other => panic!("expected External, got {other:?}"),
        }
    }

    /// The `plugin` group exposes every §5.4 verb with its documented flags.
    #[test]
    fn plugin_group_has_section_5_4_verbs() {
        assert!(matches!(
            Cli::try_parse_from(["rackabel", "plugin", "install", "owner/repo", "--yes"])
                .unwrap()
                .command,
            Command::Plugin(PluginArgs {
                command: PluginCommand::Install(_)
            })
        ));
        // `ls` is the documented alias of `list`.
        assert!(matches!(
            Cli::try_parse_from(["rackabel", "plugin", "ls"])
                .unwrap()
                .command,
            Command::Plugin(PluginArgs {
                command: PluginCommand::List
            })
        ));
        // `plugin run <name> -- …` forwards hyphenated args verbatim.
        let cli =
            Cli::try_parse_from(["rackabel", "plugin", "run", "foo", "--bar", "baz"]).unwrap();
        match cli.command {
            Command::Plugin(PluginArgs {
                command: PluginCommand::Run(a),
            }) => {
                assert_eq!(a.name, "foo");
                assert_eq!(a.args, vec!["--bar", "baz"]);
            }
            other => panic!("expected plugin run, got {other:?}"),
        }
        for verb in ["which", "enable", "disable"] {
            assert!(
                Cli::try_parse_from(["rackabel", "plugin", verb, "thing"]).is_ok(),
                "plugin {verb} <name> should parse"
            );
        }
        assert!(Cli::try_parse_from(["rackabel", "plugin", "search", "midi"]).is_ok());
    }

    /// `new --update` exists on the NewArgs surface (§5.5).
    #[test]
    fn new_has_update_flag() {
        let cli = Cli::try_parse_from(["rackabel", "new", "x", "--update"]).unwrap();
        match cli.command {
            Command::New(a) => assert!(a.update),
            other => panic!("expected new, got {other:?}"),
        }
    }
}
