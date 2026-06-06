//! `rackabel doctor` — check the local environment.

use std::path::Path;

use anyhow::Result;

use crate::max::paths;

pub fn run() -> Result<()> {
    let mut problems = 0;

    let max = paths::max_installs();
    report_list("Max", &max, &mut problems);

    let live = paths::live_installs();
    report_list("Ableton Live", &live, &mut problems);

    match paths::user_library() {
        Some(lib) if lib.is_dir() => println!("✓ User Library: {}", lib.display()),
        Some(lib) => {
            println!("✗ User Library not found at {}", lib.display());
            problems += 1;
        }
        None => {
            println!("✗ User Library location unknown on this platform");
            problems += 1;
        }
    }

    if problems == 0 {
        println!("\nAll good.");
    } else {
        println!("\n{problems} problem(s) found.");
    }
    Ok(())
}

fn report_list(label: &str, found: &[std::path::PathBuf], problems: &mut u32) {
    if found.is_empty() {
        println!("✗ {label}: not found");
        *problems += 1;
    } else {
        for path in found {
            println!("✓ {label}: {}", display_name(path));
        }
    }
}

fn display_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}
