//! Upgrade-time collision detection (DESIGN §5.6/§8) — OWNED BY THE PLUGIN-MGMT AGENT.
//!
//! Built-ins always win the namespace (§5.1). When a NEW release adds a built-in whose
//! name an already-installed plugin provides (the textbook case is the planned
//! `publish`/`login`, reserved ahead of shipping in [`crate::cli::RESERVED_NAMESPACE`]),
//! the plugin is suddenly shadowed. rackabel must surface that **loudly, once, on
//! upgrade** — never silently drop the plugin (cargo-#6507 lesson).
//!
//! This runs on every `plugin` command and on the bare external dispatch. It compares the
//! installed plugin names (`plugins.lock`) against the reserved set; any installed name
//! that is now reserved is a collision. To warn "once" (not on every invocation), the
//! warned names are persisted under `~/.rackabel/plugins.collisions` — a plain newline
//! list. The reserved-name source is injected so a unit test can simulate "a future
//! release added a built-in" without editing the global const.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::context::Ctx;
use crate::plugin::lock::LockFile;
use crate::ui;

/// The persisted record of collisions we've already warned about (so the loud warning is
/// once-per-collision, not once-per-command). `~/.rackabel/plugins.collisions`.
fn warned_path(ctx: &Ctx) -> PathBuf {
    ctx.rackabel_home.join("plugins.collisions")
}

/// The set of installed plugin names that a (now-)reserved built-in shadows. `reserved`
/// is the predicate "is this name a built-in" — production passes [`crate::cli::is_reserved`];
/// a test injects a fake set so it can simulate a future built-in without touching the
/// global const.
pub fn detect(lock: &LockFile, reserved: impl Fn(&str) -> bool) -> Vec<String> {
    lock.plugins
        .iter()
        .map(|p| p.name.clone())
        .filter(|n| reserved(n))
        .collect()
}

/// The exact §5.6 warning line for a single collision.
pub fn message(name: &str) -> String {
    format!(
        "built-in '{name}' now shadows your plugin rackabel-{name}; \
         invoke it as 'rackabel plugin run {name}' or rename the plugin"
    )
}

/// Run the collision check and emit the loud, once-per-collision warning for any NEW
/// collision (one not already in the warned record). Returns the names freshly warned
/// about (for tests). Best-effort persistence: a failure to write the record is ignored
/// (we'd rather re-warn than crash a plugin command on a state-write hiccup).
///
/// `reserved` is injected (see [`detect`]). Production callers pass [`crate::cli::is_reserved`].
pub fn check_and_warn(ctx: &Ctx, reserved: impl Fn(&str) -> bool) -> Vec<String> {
    // A missing/unreadable lockfile means nothing is installed → no collisions.
    let lock = match LockFile::load(ctx) {
        Ok(l) => l,
        Err(_) => return Vec::new(),
    };
    let collisions = detect(&lock, reserved);
    if collisions.is_empty() {
        return Vec::new();
    }

    let already = load_warned(&warned_path(ctx));
    let fresh: Vec<String> = collisions
        .iter()
        .filter(|c| !already.contains(*c))
        .cloned()
        .collect();

    for name in &fresh {
        // Loud: a warn-symbol frame on stdout (suppressed only under --json/quiet so
        // machine output stays clean — the JSON surface carries collisions separately).
        if ctx.echo_on() {
            ui::frame::emit(ui::frame::Symbol::Warn, &message(name), ctx);
        }
    }

    if !fresh.is_empty() {
        // Persist the union so each collision warns once. Best-effort.
        let mut all = already;
        all.extend(collisions.iter().cloned());
        let _ = save_warned(&warned_path(ctx), &all);
    }

    fresh
}

fn load_warned(path: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(path)
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn save_warned(path: &Path, names: &BTreeSet<String>) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = names.iter().cloned().collect::<Vec<_>>().join("\n");
    std::fs::write(path, format!("{body}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::lock::{PluginLockEntry, SourceKind};
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn ctx(home: &Path) -> Ctx {
        crate::context::Ctx {
            no_input: true,
            json: false,
            quiet: false,
            verbose: false,
            raw: false,
            color: crate::ui::color::ColorMode::Never,
            color_err: crate::ui::color::ColorMode::Never,
            cwd: home.to_path_buf(),
            rackabel_home: home.join(".rackabel"),
            home: home.to_path_buf(),
            ableton_app: None,
            ableton_user_library: None,
            ableton_eh_mod: None,
            ableton_eh_node: None,
            ableton_extensions_dir: None,
            ableton_storage_base: None,
            rackabel_host_cmd: None,
        }
    }

    fn entry(name: &str) -> PluginLockEntry {
        PluginLockEntry {
            name: name.to_string(),
            source: SourceKind::Path,
            origin: format!("/x/rackabel-{name}"),
            commit: None,
            sha256: Some("ab".to_string()),
            installed_at: "2026-06-07T00:00:00Z".to_string(),
            executable: PathBuf::from(format!("/h/.rackabel/plugins/bin/rackabel-{name}")),
            has_plugin_manifest: false,
            hooks: Vec::new(),
            hooks_digest: None,
            enabled: false,
        }
    }

    #[test]
    fn detect_finds_a_now_reserved_plugin() {
        let mut lock = LockFile::default();
        lock.upsert(entry("publish")); // installed before `publish` became a built-in
        lock.upsert(entry("notarize")); // a plain plugin, never reserved
        // Inject a fake reserved set simulating "a future release added `publish`".
        let fake_reserved = |n: &str| n == "publish";
        let hits = detect(&lock, fake_reserved);
        assert_eq!(hits, vec!["publish".to_string()]);
    }

    #[test]
    fn message_has_the_section_5_6_shape() {
        let m = message("publish");
        assert!(m.contains("built-in 'publish' now shadows your plugin rackabel-publish"));
        assert!(m.contains("rackabel plugin run publish"));
        assert!(m.contains("rename the plugin"));
    }

    #[test]
    fn check_and_warn_is_once_per_collision() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        let mut lock = LockFile::default();
        lock.upsert(entry("publish"));
        lock.save(&c).unwrap();

        let fake_reserved = |n: &str| n == "publish";
        // First check warns; the record is written.
        let first = check_and_warn(&c, fake_reserved);
        assert_eq!(first, vec!["publish".to_string()]);
        // Second check is silent (already warned).
        let second = check_and_warn(&c, fake_reserved);
        assert!(second.is_empty());
    }

    #[test]
    fn no_collision_when_nothing_reserved() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        let mut lock = LockFile::default();
        lock.upsert(entry("notarize"));
        lock.save(&c).unwrap();
        assert!(check_and_warn(&c, |_| false).is_empty());
    }
}
