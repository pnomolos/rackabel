//! `rackabel dev register` — add a path to the persistent registry (DESIGN §2, §3.2).
//!
//! Writes `~/.rackabel/registry.toml` (RACKABEL_HOME-aware via [`Registry`]). The
//! registry is operable with a dead daemon — this never touches the host or socket.
//!
//! Behavior (§3.2 / §4.4):
//!   - `register [PATH]` defaults to the cwd; the entry `name` is the dir basename,
//!     auto-disambiguated against existing names AND reserved dev verbs (parent-
//!     prefixed, echoed when it changes).
//!   - `--recursive` registers every member of a monorepo via `[workspace].members`
//!     (when `PATH` is a workspace root) else a manifest scan, SKIPPING library
//!     members (no `[extension]`/`[device]`).
//!   - `--name NAME` is single-path only (clap rejects `--name` + `--recursive` at
//!     parse time, exit 2). A `--name` that collides with an existing entry is
//!     auto-disambiguated interactively, or `RK0312` under `--no-input` (we can't
//!     silently rename what the user explicitly asked for). A `--name` equal to a dev
//!     verb is *forced* with a warning that only `--only`/`--` can target it.
//!   - `--disabled` registers the entry dormant (`enabled = false`).

use crate::cli::DevRegisterArgs;
use crate::context::Ctx;
use crate::dev::registry::{NameOutcome, Registry};
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::ui;

pub fn run(args: &DevRegisterArgs, ctx: &Ctx) -> CmdResult<()> {
    let path = match &args.path {
        Some(p) if p.is_absolute() => p.clone(),
        Some(p) => ctx.cwd.join(p),
        None => ctx.cwd.clone(),
    };

    let mut reg = Registry::load(ctx)?;

    if args.recursive {
        let names = reg.add_recursive(&path, args.disabled, ctx)?;
        if names.is_empty() {
            return Err(RkError::of(
                ErrorCode::NoManifest,
                "no registrable extensions found under that path",
                "check the path holds extension projects (library-only members are skipped); \
                 register a single project without --recursive if it is not a monorepo",
            )
            .at(path.display().to_string()));
        }
        reg.save()?;
        for name in &names {
            ui::frame::emit(ui::frame::Symbol::Good, &format!("registered {name}"), ctx);
        }
        ui::frame::note(
            &format!(
                "{} extension{} registered{}",
                names.len(),
                if names.len() == 1 { "" } else { "s" },
                if args.disabled { " (disabled)" } else { "" }
            ),
            ctx,
        );
        return Ok(());
    }

    // Single-path register. Resolve the candidate name + collision policy up front so
    // we can echo a disambiguation / warn on a forced verb / raise RK0312.
    let basename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("extension")
        .to_string();

    let final_name = match &args.name {
        Some(explicit) => match reg.name_outcome(explicit, &path) {
            NameOutcome::Free => explicit.clone(),
            NameOutcome::CollidesEntry { resolved } => {
                // An explicit `--name` we'd have to rename can't be silently satisfied:
                // the invocation as written is unsatisfiable, so it's a usage error
                // (RK0312, exit 2) naming the free alternative — never an auto-rename of
                // what the user explicitly asked for.
                return Err(RkError::of(
                    ErrorCode::NameCollision,
                    format!("the name `{explicit}` is already taken"),
                    format!(
                        "choose a free --name (e.g. `{resolved}`), or omit --name to \
                         auto-disambiguate from the directory name"
                    ),
                ));
            }
            NameOutcome::CollidesVerb { .. } => {
                // A name equal to a dev verb is forced with a warning: it can only ever
                // be targeted via --only/--, never the bare `dev` parse.
                ui::frame::emit(
                    ui::frame::Symbol::Warn,
                    &format!(
                        "`{explicit}` is a `dev` verb — registering it anyway, but only \
                         `rackabel dev --only {explicit}` / `rackabel dev -- {explicit}` can \
                         target it (`rackabel dev {explicit}` runs the subcommand)"
                    ),
                    ctx,
                );
                explicit.clone()
            }
        },
        None => match reg.name_outcome(&basename, &path) {
            NameOutcome::Free => basename.clone(),
            NameOutcome::CollidesEntry { resolved } | NameOutcome::CollidesVerb { resolved } => {
                ui::frame::echo_resolved(
                    "name",
                    &resolved,
                    &format!("`{basename}` was taken or reserved"),
                    ctx,
                );
                resolved
            }
        },
    };

    let stored = reg.add_named(&path, final_name, args.disabled)?;
    reg.save()?;
    ui::frame::emit(
        ui::frame::Symbol::Good,
        &format!(
            "registered {stored}{}",
            if args.disabled { " (disabled)" } else { "" }
        ),
        ctx,
    );
    Ok(())
}
