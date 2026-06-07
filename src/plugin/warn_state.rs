//! The one-time both-locations warning + its persisted state (DESIGN §5.1/§5.6).
//!
//! When an external `rackabel-<foo>` is resolvable from BOTH the managed bin dir and
//! `$PATH`, rackabel surfaces the shadowing PROACTIVELY (the cargo-#6507 lesson) rather
//! than leaving it to `plugin which`. To avoid warning on every single invocation, the
//! warning is **one-time per name**: the first time a given name's both-locations
//! collision is hit, rackabel prints the warning and records the name in a tiny state
//! file under `RACKABEL_HOME`; subsequent invocations of that same name stay quiet.
//!
//! State lives in `~/.rackabel/plugins/warned-both-locations` — one name per line. It is
//! advisory: a missing/unreadable/unwritable file just means the warning fires again
//! (a warning is never load-bearing), so this never turns a plugin invocation into an
//! error. `plugin which <name>` (and reinstalling) remain the authoritative,
//! always-shown surfaces — this is the proactive nudge, not the record of truth.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::context::Ctx;
use crate::ui;

use super::plugins_dir;

/// The state file: `~/.rackabel/plugins/warned-both-locations`.
fn state_path(ctx: &Ctx) -> PathBuf {
    plugins_dir(ctx).join("warned-both-locations")
}

/// Load the set of names already warned about. A missing/unreadable file is an empty set
/// (the warning will then fire — advisory state, never an error).
fn load(ctx: &Ctx) -> BTreeSet<String> {
    let path = state_path(ctx);
    match std::fs::read_to_string(&path) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        Err(_) => BTreeSet::new(),
    }
}

/// Record `name` as warned (best-effort; a write failure is swallowed — the only cost is
/// the warning firing again next time, which is harmless).
fn record(ctx: &Ctx, names: &BTreeSet<String>) {
    let path = state_path(ctx);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body: String = names
        .iter()
        .map(|n| format!("{n}\n"))
        .collect::<Vec<_>>()
        .join("");
    let _ = std::fs::write(&path, body);
}

/// Emit the both-locations warning for `name` AT MOST ONCE per `RACKABEL_HOME` (§5.1).
///
/// The first call for a given name (when the warning is enabled — i.e. not under
/// `--json`/`quiet`) prints it to STDERR (so a plugin's own stdout is never polluted —
/// critical for `plugin run` in a pipe) and records the name; later calls for that name
/// are suppressed. Suppressed output (`--json`/`quiet`) does NOT record the name, so the
/// warning still surfaces the first time the user runs interactively.
pub fn warn_both_locations_once(ctx: &Ctx, name: &str) {
    // Under --json / the quiet dev-watch seam, stay silent AND do not record — a machine
    // consumer never sees it, and a later human run should still get the one-time nudge.
    if !ctx.echo_on() {
        return;
    }

    let mut warned = load(ctx);
    if warned.contains(name) {
        return;
    }

    let managed = super::plugins_bin_dir(ctx).display().to_string();
    ui::frame::ewarn(
        &format!(
            "rackabel-{name} found in both {managed} and $PATH; using the managed one \
             (see `rackabel plugin which {name}`)"
        ),
        ctx,
    );

    warned.insert(name.to_string());
    record(ctx, &warned);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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

    #[test]
    fn records_then_recognizes_a_warned_name() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        // First call records the name.
        warn_both_locations_once(&c, "foo");
        assert!(load(&c).contains("foo"));
        // A second name is independent.
        assert!(!load(&c).contains("bar"));
        warn_both_locations_once(&c, "bar");
        let warned = load(&c);
        assert!(warned.contains("foo"));
        assert!(warned.contains("bar"));
    }

    #[test]
    fn json_or_quiet_does_not_record() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = ctx(tmp.path());
        c.json = true;
        warn_both_locations_once(&c, "foo");
        // Suppressed output never records, so a later human run still gets the nudge.
        assert!(!load(&c).contains("foo"));
    }

    #[test]
    fn missing_state_file_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        assert!(load(&c).is_empty());
    }
}
