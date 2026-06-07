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
    }
}
