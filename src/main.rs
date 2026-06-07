//! rackabel — build Max for Live devices and Ableton Live extensions.
//!
//! `main` parses the CLI (a clap parse error exits `2`), builds the resolved [`Ctx`]
//! from the global flags + environment, dispatches to the command, and maps the
//! result to a process exit code via the [`error`] taxonomy (DESIGN §7): `0` ok,
//! `1` build/runtime, `2` usage, `3` environment, `4` validation. Precedence is
//! enforced *inside* each command (it runs the environment subset first and returns
//! a single highest-severity `RkError`), so `main` only needs to read the class off
//! the returned error.

// The foundation freezes a public service/manifest/ui API that the five parallel
// command-owner branches consume. Because rackabel is a binary crate, items that are
// only called from a not-yet-landed command read as `dead_code` even though they are
// the load-bearing surface those branches compile against. Allowing it crate-wide
// keeps `clippy -D warnings` green during the parallel build-out; as command bodies
// land and exercise the surface, this can be tightened.
#![allow(dead_code)]

mod cli;
mod commands;
mod context;
// The managed dev host (milestone 0.3). The daemon/host/ipc mechanics use Unix-only
// primitives (setsid/setpgid/killpg via nix), so the whole module is #[cfg(unix)];
// the dev command surface degrades to a clean RK0307 "Unix-only for now" on Windows
// (SPEC D §5/§9.3). The module is itself #[cfg(unix)]-gated internally.
#[cfg(unix)]
mod dev;
mod error;
mod manifest;
mod max;
mod services;
mod ui;

use std::process::ExitCode;

use clap::Parser;

use cli::{Cli, Command};
use context::Ctx;
use error::CmdResult;

fn main() -> ExitCode {
    // A clap parse error is a usage error (exit 2). clap prints its own message.
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            // clap uses 2 for genuine errors and 0 for --help/--version; mirror that
            // so `--help` stays exit 0.
            return ExitCode::from(if e.use_stderr() { 2 } else { 0 });
        }
    };

    let ctx = Ctx::from_globals(&cli);

    match dispatch(cli.command, &ctx) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            ui::print_error(&err, &ctx);
            ExitCode::from(err.class as u8)
        }
    }
}

fn dispatch(command: Command, ctx: &Ctx) -> CmdResult<()> {
    match command {
        Command::New(args) => commands::new::run(&args, ctx),
        Command::Build(args) => commands::build::run(&args, ctx),
        Command::Deploy(args) => commands::deploy::run(&args, ctx),
        Command::Install(args) => commands::install::run(&args, ctx),
        Command::Pack(args) => commands::pack::run(&args, ctx),
        Command::Validate(args) => commands::validate::run(&args, ctx),
        Command::Doctor(args) => commands::doctor::run(&args, ctx),
        Command::Explain(args) => commands::explain::run(&args, ctx),
        Command::Dev(args) => dispatch_dev(args, ctx),
        Command::Daemon(args) => dispatch_daemon(args, ctx),
    }
}

/// Route `rackabel dev …`. Unix runs the dev group; other platforms return a clean
/// framed "dev host is Unix-only for now" (RK0307, SPEC D §5/§9.3) rather than a
/// missing-module compile error or a panic.
#[cfg(unix)]
fn dispatch_dev(args: cli::DevArgs, ctx: &Ctx) -> CmdResult<()> {
    commands::dev::run(args, ctx)
}

#[cfg(not(unix))]
fn dispatch_dev(_args: cli::DevArgs, _ctx: &Ctx) -> CmdResult<()> {
    Err(dev_unix_only())
}

#[cfg(unix)]
fn dispatch_daemon(args: cli::DaemonArgs, ctx: &Ctx) -> CmdResult<()> {
    commands::dev::run_daemon(args, ctx)
}

#[cfg(not(unix))]
fn dispatch_daemon(_args: cli::DaemonArgs, _ctx: &Ctx) -> CmdResult<()> {
    Err(dev_unix_only())
}

#[cfg(not(unix))]
fn dev_unix_only() -> error::RkError {
    error::RkError::of(
        error::ErrorCode::DaemonStartFailed,
        "the managed dev host is macOS/Unix-only for now",
        "the dev loop (rackabel dev) runs on macOS/Linux; build/deploy/pack work here",
    )
}
