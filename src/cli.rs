//! The clap CLI surface for 0.2 (DESIGN §2 synopses).
//!
//! Every 0.2 command appears here with its section-2 flags, plus the GLOBAL flags
//! (`--no-input`, `--json` where applicable, `--verbose`, `--raw`, the six launcher
//! `ABLETON_*` overrides exposed as flags — DESIGN §7). The `dev`/`plugin` groups
//! are later milestones and are deliberately absent. `install` is a hidden alias of
//! `deploy`. Command-owners fill the `commands::*::run` bodies; this file is frozen.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// rackabel — build Max for Live devices and Ableton Live extensions.
#[derive(Parser)]
#[command(name = "rackabel", version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub globals: GlobalFlags,

    #[command(subcommand)]
    pub command: Command,
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

#[derive(Subcommand)]
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
