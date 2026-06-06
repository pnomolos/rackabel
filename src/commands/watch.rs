//! `rackabel watch` — rebuild whenever source files change.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{EventKind, RecursiveMode, Watcher};

use crate::commands::build;
use crate::project::Project;

/// Coalesce bursts of filesystem events within this window into one rebuild.
const DEBOUNCE: Duration = Duration::from_millis(250);

pub fn run() -> Result<()> {
    let project = Project::discover_cwd()?;
    let src = project.root.join("src");
    let watch_root: &Path = if src.is_dir() { &src } else { &project.root };

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx)?;
    watcher
        .watch(watch_root, RecursiveMode::Recursive)
        .with_context(|| format!("watching {}", watch_root.display()))?;

    println!("Watching {} — ctrl-c to stop", watch_root.display());
    let mut last_build = Instant::now() - DEBOUNCE;

    for event in rx {
        let event = event?;
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
        if let Err(err) = build::run() {
            eprintln!("build failed: {err:#}");
        }
    }
    Ok(())
}
