//! `rackabel dev watch` + the bare `rackabel dev` flagship loop (DESIGN §2, §3.1/§3.3).
//!
//! OWNED BY THE WATCH-LOOP AGENT. Two entrypoints:
//!   - [`run_bare`] (no verb): the flagship loop — fast preflight + start-the-daemon-if-
//!     needed, then attach the foreground watch UI as a client of the daemon-owned host.
//!   - [`run`] (`dev watch`): the EXPLICIT form — never starts a daemon (`RK0309` if none
//!     up; it tells you to run `rackabel dev`).
//!
//! Both resolve the transient working set from the registry: bare positionals / `dev
//! watch` positionals are registry NAMEs (post-disambiguation) or a single PATH, and
//! `--only GLOB` matches registry NAMEs — both ALWAYS route through the registry name
//! matcher, NEVER the dev-verb table (§3.3). With no scope, the whole enabled set loads.

use std::path::Path;

use crate::cli::{DevArgs, DevWatchArgs};
use crate::context::Ctx;
use crate::dev::registry::Registry;
use crate::dev::watch::{self, DEFAULT_DEBOUNCE_MS, WatchOpts};
use crate::dev::{Inspect, RegistryEntry, daemon, ipc, preflight, resolve, sock_path};
use crate::error::{CmdResult, ErrorCode, RkError};

/// `rackabel dev watch` — the explicit form (no implicit daemon start).
pub fn run(args: &DevWatchArgs, ctx: &Ctx) -> CmdResult<()> {
    let opts = build_opts(!args.no_auto_reload, false, None, false, ctx);
    attach(
        &args.names,
        args.only.as_deref(),
        opts,
        ctx,
        /* start_if_needed */ false,
    )
}

/// Bare `rackabel dev` — start-if-needed + watch + tail (the flagship loop).
pub fn run_bare(args: &DevArgs, ctx: &Ctx) -> CmdResult<()> {
    let inspect = super::start::parse_inspect(args.inspect.as_deref())?;
    let opts = build_opts(
        !args.no_auto_reload,
        args.raw,
        inspect,
        args.emit_launch_config,
        ctx,
    );
    attach(
        &args.names,
        args.only.as_deref(),
        opts,
        ctx,
        /* start_if_needed */ true,
    )
}

/// Resolve the daemon target, (optionally) start it, connect, then resolve the working
/// set and run the watch loop.
///
/// Ordering note: the daemon resolve/start runs BEFORE the working-set resolution so the
/// environment errors that gate the whole loop (no Live → `RK0307`/`RK0306`, no daemon →
/// `RK0309`) surface first. `--only`/`-- <NAME…>` still route purely through the registry
/// name matcher (never the verb table, §3.3); they just can't usefully match anything
/// until the host can actually load — and an empty/non-matching set is reported then.
fn attach(
    names: &[String],
    only: Option<&str>,
    opts: WatchOpts,
    ctx: &Ctx,
    start_if_needed: bool,
) -> CmdResult<()> {
    let target = resolve::resolve(ctx)?;
    let sock = sock_path(ctx, target.app());

    if start_if_needed {
        // Bare `dev`: the fast doctor subset / block-and-wait gate (Live-running, then
        // Dev-Mode/host-reachable). Interactive runs block-and-wait; `--no-input` makes
        // each a deterministic exit 3 (RK0303/RK0306) — never a hang (§3.6/§7). Then
        // start the daemon if it isn't already up (`daemon::start` is idempotent).
        if !daemon::is_running(ctx, target.app()) {
            preflight::ensure_ready(ctx)?;
            daemon::start(ctx)?;
        }
        // Apply --inspect against the (now-running) daemon, restarting with the inspector
        // and announcing it (§7), before the watch UI loads the working set.
        if let Some(ins) = &opts.inspect {
            super::status::apply_inspect(ctx, target.app(), ins)?;
        }
    }

    // Connect. For `dev watch` with no daemon this is the clean RK0309 (the explicit form
    // never starts a host — it tells you to run `rackabel dev`).
    let client = ipc::Client::connect(&sock)?;

    // Now resolve the working set (after the env/daemon gates above).
    let working_set = resolve_working_set(names, only, ctx)?;
    watch::run(client, working_set, opts, ctx)
}

/// Build the [`WatchOpts`], reading the `[dev].debounce_ms` override from the cwd project
/// manifest when present (§3.3, default 200 ms).
fn build_opts(
    auto_reload: bool,
    raw: bool,
    inspect: Option<Inspect>,
    emit_launch_config: bool,
    ctx: &Ctx,
) -> WatchOpts {
    WatchOpts {
        auto_reload,
        debounce_ms: debounce_ms(ctx),
        raw,
        inspect,
        emit_launch_config,
    }
}

/// Resolve the transient working set (§3.3): explicit NAMEs/PATH or `--only GLOB`, both
/// through the registry name matcher; with no scope, the full enabled set.
///
/// - A single token that is an existing registry name (or a path of a registered entry)
///   resolves to that entry; otherwise an unknown token is a clear error.
/// - `--only GLOB` matches registry NAMEs by glob (the same post-disambiguation names
///   `dev logs`/`dev reload` use) — never dir basenames or the verb table.
fn resolve_working_set(
    names: &[String],
    only: Option<&str>,
    ctx: &Ctx,
) -> CmdResult<Vec<RegistryEntry>> {
    let registry = Registry::load(ctx)?;
    let enabled: Vec<RegistryEntry> = registry.enabled().cloned().collect();

    // --only GLOB: filter the enabled set by a name glob.
    if let Some(pat) = only {
        let glob = globset::Glob::new(pat)
            .map_err(|e| {
                RkError::of(
                    ErrorCode::NameCollision,
                    format!("`--only {pat}` is not a valid name glob"),
                    "use a glob over registry names, e.g. --only 'harmonic-*' \
                     (see `rackabel dev list` for the matchable names)",
                )
                .raw(e.into())
            })?
            .compile_matcher();
        let matched: Vec<RegistryEntry> = enabled
            .into_iter()
            .filter(|e| glob.is_match(&e.name))
            .collect();
        if matched.is_empty() {
            return Err(no_match_error(pat));
        }
        return Ok(matched);
    }

    // Explicit NAMEs / a single PATH.
    if !names.is_empty() {
        let mut set = Vec::new();
        for token in names {
            match registry.find(token) {
                Some(e) if e.enabled => set.push(e.clone()),
                Some(e) => {
                    // A named-but-disabled entry: include it transiently (the working set
                    // is a session scope, not a registry edit — the user asked for it).
                    set.push(e.clone());
                }
                None => return Err(unknown_name_error(token)),
            }
        }
        return Ok(set);
    }

    // No scope: the whole enabled set.
    Ok(enabled)
}

/// Read `[dev].debounce_ms` from the cwd project manifest (best-effort; the foundation's
/// `ManifestRaw` has no `[dev]` table, so we parse the raw TOML for the optional value
/// rather than editing the shared manifest model — INTEGRATOR NOTE in the summary).
fn debounce_ms(ctx: &Ctx) -> u64 {
    read_debounce_from(&ctx.cwd).unwrap_or(DEFAULT_DEBOUNCE_MS)
}

fn read_debounce_from(start: &Path) -> Option<u64> {
    // Walk up for rackabel.toml (mirrors Project::discover) and read [dev].debounce_ms.
    for dir in start.ancestors() {
        let candidate = dir.join("rackabel.toml");
        if candidate.is_file() {
            let text = std::fs::read_to_string(&candidate).ok()?;
            let value: toml::Value = toml::from_str(&text).ok()?;
            return value
                .get("dev")
                .and_then(|d| d.get("debounce_ms"))
                .and_then(|v| v.as_integer())
                .filter(|n| *n > 0)
                .map(|n| n as u64);
        }
    }
    None
}

fn unknown_name_error(token: &str) -> RkError {
    RkError::of(
        ErrorCode::NameCollision,
        format!("no registered extension named `{token}`"),
        "run `rackabel dev list` to see the registered names, or \
         `rackabel dev register <path>` to add one",
    )
}

fn no_match_error(pat: &str) -> RkError {
    RkError::of(
        ErrorCode::NameCollision,
        format!("`--only {pat}` matched no enabled extensions"),
        "run `rackabel dev list` to see the matchable names",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dev::Source;

    fn ctx_for(home: &Path) -> Ctx {
        Ctx {
            no_input: true,
            json: false,
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

    /// Write a registry.toml with the given entries under `home/.rackabel`.
    fn seed_registry(home: &Path, entries: &[(&str, bool)]) {
        let dir = home.join(".rackabel");
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = String::new();
        for (name, enabled) in entries {
            body.push_str(&format!(
                "[[extension]]\nname = \"{name}\"\npath = \"/proj/{name}\"\nsource = \"dist\"\nenabled = {enabled}\n\n"
            ));
        }
        std::fs::write(dir.join("registry.toml"), body).unwrap();
        let _ = Source::Dist; // keep the import meaningful
    }

    #[test]
    fn empty_scope_loads_full_enabled_set() {
        let tmp = tempfile::tempdir().unwrap();
        seed_registry(tmp.path(), &[("a", true), ("b", true), ("c", false)]);
        let ctx = ctx_for(tmp.path());
        let set = resolve_working_set(&[], None, &ctx).unwrap();
        let names: Vec<&str> = set.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"], "only enabled entries, no disabled");
    }

    #[test]
    fn only_glob_matches_names_not_verbs() {
        let tmp = tempfile::tempdir().unwrap();
        // An extension literally named `test` (a dev verb) must stay reachable via --only.
        seed_registry(tmp.path(), &[("test", true), ("harmonic-lens", true)]);
        let ctx = ctx_for(tmp.path());
        let set = resolve_working_set(&[], Some("test"), &ctx).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].name, "test");
    }

    #[test]
    fn only_glob_supports_wildcards() {
        let tmp = tempfile::tempdir().unwrap();
        seed_registry(
            tmp.path(),
            &[
                ("harmonic-lens", true),
                ("harmonic-x", true),
                ("groove", true),
            ],
        );
        let ctx = ctx_for(tmp.path());
        let set = resolve_working_set(&[], Some("harmonic-*"), &ctx).unwrap();
        let mut names: Vec<&str> = set.iter().map(|e| e.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["harmonic-lens", "harmonic-x"]);
    }

    #[test]
    fn only_no_match_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        seed_registry(tmp.path(), &[("a", true)]);
        let ctx = ctx_for(tmp.path());
        let err = resolve_working_set(&[], Some("zzz*"), &ctx).unwrap_err();
        assert_eq!(err.code, ErrorCode::NameCollision);
    }

    #[test]
    fn explicit_unknown_name_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        seed_registry(tmp.path(), &[("a", true)]);
        let ctx = ctx_for(tmp.path());
        let err = resolve_working_set(&["nope".to_string()], None, &ctx).unwrap_err();
        assert_eq!(err.code, ErrorCode::NameCollision);
    }

    #[test]
    fn debounce_reads_dev_table_else_default() {
        let tmp = tempfile::tempdir().unwrap();
        // No manifest → default.
        assert_eq!(read_debounce_from(tmp.path()), None);
        // With a [dev].debounce_ms.
        std::fs::write(
            tmp.path().join("rackabel.toml"),
            "[extension]\nname = \"x\"\n\n[dev]\ndebounce_ms = 500\n",
        )
        .unwrap();
        assert_eq!(read_debounce_from(tmp.path()), Some(500));
    }
}
