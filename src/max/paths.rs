//! Platform-specific locations for Max and Ableton Live.

use std::path::PathBuf;

/// Ableton's User Library, where installed M4L devices live.
pub fn user_library() -> Option<PathBuf> {
    let home = std::env::home_dir()?;
    if cfg!(target_os = "macos") {
        Some(home.join("Music/Ableton/User Library"))
    } else if cfg!(target_os = "windows") {
        Some(home.join("Documents").join("Ableton").join("User Library"))
    } else {
        None
    }
}

/// Directory inside the User Library where M4L audio effects belong.
pub fn m4l_presets_dir() -> Option<PathBuf> {
    Some(user_library()?.join("Presets"))
}

/// Installed Max applications, best-effort.
pub fn max_installs() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        glob_apps("/Applications", "Max")
    } else {
        Vec::new()
    }
}

/// Installed Ableton Live applications, best-effort.
pub fn live_installs() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        glob_apps("/Applications", "Ableton Live")
    } else {
        Vec::new()
    }
}

/// List `<dir>/<prefix>*.app` entries.
fn glob_apps(dir: &str, prefix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|ext| ext == "app")
                && p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with(prefix))
        })
        .collect();
    found.sort();
    found
}
