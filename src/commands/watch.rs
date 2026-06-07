//! `rackabel watch` — rebuild whenever source files change (M4L).
//!
//! OWNED BY THE BUILD AGENT (re-point only). In 0.2 there is no top-level `watch`
//! command in the CLI surface (the Extensions watch loop is `dev watch`, a 0.3
//! milestone). This module is retained, compiling against the new `Ctx`/build
//! signature, so the build-owner can re-point it without a structural change. It is
//! not wired into dispatch in 0.2.

#![allow(dead_code)]

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{EventKind, RecursiveMode, Watcher};

use crate::cli::BuildArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::Project;

/// Coalesce bursts of filesystem events within this window into one rebuild.
const DEBOUNCE: Duration = Duration::from_millis(250);

/// Watch the project's source tree and rebuild on change. Re-pointed at the new
/// build entry by the build-owner.
pub fn run(ctx: &Ctx) -> CmdResult<()> {
    let project = Project::discover_cwd(ctx)?;
    let src = project.root.join("src");
    let watch_root: &Path = if src.is_dir() { &src } else { &project.root };

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx).map_err(watch_err)?;
    watcher
        .watch(watch_root, RecursiveMode::Recursive)
        .map_err(watch_err)?;

    println!("Watching {} — ctrl-c to stop", watch_root.display());
    let mut last_build = Instant::now() - DEBOUNCE;
    let args = BuildArgs {
        release: false,
        clean: false,
        typecheck: false,
        no_typecheck: false,
        print_config: false,
        dry_run: false,
    };

    for event in rx {
        let Ok(event) = event else { continue };
        if !matches!(
            event.kind,
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
        ) {
            continue;
        }
        if last_build.elapsed() < DEBOUNCE {
            continue;
        }
        last_build = Instant::now();

        println!("Change detected, rebuilding…");
        if let Err(err) = crate::commands::build::run(&args, ctx) {
            eprintln!("{err}");
        }
    }
    Ok(())
}

fn watch_err(e: notify::Error) -> RkError {
    RkError::new(
        ErrorCode::BuildFailed,
        ExitClass::BuildRuntime,
        "could not watch the source directory",
        "check the directory exists and is readable",
    )
    .raw(e.into())
}
