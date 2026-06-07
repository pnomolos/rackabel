//! `rackabel plugin enable <name>` (DESIGN §5.4/§5.7) — the HOOK CONSENT GATE.
//!
//! OWNED BY THE HOOK-VERBS AGENT (consent half). Flipping the `enabled` flag does two
//! things: in 0.4 it gates DISPATCH of the MANAGED copy (a disabled managed plugin is
//! skipped in the bin search — see [`crate::plugin::resolve`]); in 0.5 the SAME flag is the
//! consent gate for the plugin's lifecycle hooks (§5.7). So enabling a plugin that carries
//! hooks is CONSENT to run third-party code at lifecycle points — including on EVERY SAVE for
//! a `post_build`/`on_reload` hook under `rackabel dev`. We therefore print exactly which
//! hooks will run at which points and require confirmation before flipping the flag:
//!   - `--yes` scripts the consent;
//!   - `--no-input` REFUSES (no implicit consent for code execution — RK0403, nothing runs);
//!   - interactively, a y/N defaulting to NO.
//!
//! A plain (no-manifest) plugin or a manifest with no runnable hooks needs no consent — it is
//! enabled directly (the dispatch-gate semantics only).

use crate::cli::PluginEnableArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::hooks::HookKind;
use crate::plugin::lock::LockFile;
use crate::plugin::{plugin_store_dir, store};
use crate::ui;

pub fn run(args: &PluginEnableArgs, ctx: &Ctx) -> CmdResult<()> {
    // Announce any upgrade-time collision loudly once (§5.6) on every plugin command.
    crate::plugin::collision::check_and_warn(ctx, crate::cli::is_reserved);

    // Read the entry first so we can decide whether consent is needed (a hook plugin) and so
    // the consent transcript can enumerate the hooks BEFORE we mutate anything.
    let lock = LockFile::load(ctx)?;
    let entry = lock
        .find(&args.name)
        .ok_or_else(|| not_installed(&args.name))?;

    let already_enabled = entry.enabled;
    let carries_hooks = entry.has_plugin_manifest && !entry.hooks.is_empty();

    // Re-read the manifest for the authoritative hook command list (the lock holds only the
    // inert hook NAMES). If it no longer parses / declares no runnable hook, treat the plugin
    // as hookless for consent purposes — there is nothing to consent to.
    let runnable = if carries_hooks {
        resolve_runnable_hooks(ctx, &args.name)
    } else {
        Vec::new()
    };

    // If the plugin carries hooks AND is not already enabled, enabling is CONSENT — gate it.
    if !runnable.is_empty() && !already_enabled {
        consent(ctx, &args.name, &runnable, args.yes)?;
    }

    // Consent obtained (or not needed): flip the flag.
    set_enabled(&args.name, true, ctx)
}

/// The list of hook KINDS this plugin will actually run if enabled (the runnable commands
/// from its store-dir `rackabel-plugin.toml`), in [`HookKind::ALL`] order. Empty when the
/// manifest is gone / unparseable / declares no string-command hook this build understands.
fn resolve_runnable_hooks(ctx: &Ctx, name: &str) -> Vec<HookKind> {
    let store_dir = plugin_store_dir(ctx, name);
    match store::load_plugin_manifest(&store_dir) {
        Ok(Some(m)) => m.declared_kinds(),
        _ => Vec::new(),
    }
}

/// Print what hooks will run at which lifecycle points (and the on-every-save implication for
/// `dev`), then require consent (§5.7). `--yes` scripts it; `--no-input` REFUSES (no implicit
/// consent for code execution); interactively a y/N defaulting to NO.
fn consent(ctx: &Ctx, name: &str, hooks: &[HookKind], yes: bool) -> CmdResult<()> {
    if ctx.echo_on() {
        ui::frame::emit(
            ui::frame::Symbol::Warn,
            &format!(
                "enabling `{name}` consents to running its third-party hook code at these \
                 lifecycle points (it runs with your full privileges, §5.7):"
            ),
            ctx,
        );
        for &k in hooks {
            println!("      - {} {}", k.as_str(), lifecycle_when(k));
        }
        if hooks
            .iter()
            .any(|&k| matches!(k, HookKind::PostBuild | HookKind::OnReload))
        {
            ui::frame::note(
                "post_build/on_reload run on EVERY SAVE under `rackabel dev` — enabling is \
                 standing consent, not a per-save prompt",
                ctx,
            );
        }
    }

    if yes {
        return Ok(()); // scripted consent.
    }
    if ctx.no_input || ctx.json {
        // No implicit consent for running third-party code. --json can't carry an interactive
        // decision either, so pair it with --yes. Nothing is enabled; no hook runs.
        return Err(declined(name));
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(declined(name));
    }
    // Interactive y/N, defaulting to NO (consent must be explicit).
    let ok = ui::prompt::confirm(
        &format!("Enable `{name}` and run its hooks at those points?"),
        false,
        ctx,
    )?;
    if ok { Ok(()) } else { Err(declined(name)) }
}

/// When (at which lifecycle phase) a hook kind runs — for the consent transcript.
fn lifecycle_when(kind: HookKind) -> &'static str {
    match kind {
        HookKind::PostBuild => "(after every build)",
        HookKind::PreDeploy => "(before every deploy — can VETO the deploy)",
        HookKind::OnReload => "(after every dev-loop reload)",
        HookKind::DoctorCheck => "(adds a row to `rackabel doctor`)",
        HookKind::NewTemplate => "(offers a template in the `rackabel new` wizard)",
    }
}

/// The declined-consent frame (RK0403, exit 3) — reuses the install/template fetch-declined
/// code: enabling unreviewed hook execution that was not confirmed. Nothing was enabled.
fn declined(name: &str) -> RkError {
    RkError::of(
        ErrorCode::TemplateFetchDeclined,
        format!("enabling `{name}` runs its third-party hooks and was not confirmed"),
        "pass --yes to consent (e.g. in a script), or run without --no-input to confirm \
         interactively. The plugin stays disabled until you do; no hook runs.",
    )
}

/// Shared enable/disable: load the lock, flip the flag (idempotent), persist, report. Used by
/// `enable` (after consent) and `disable` directly (disabling needs no consent).
pub fn set_enabled(name: &str, enabled: bool, ctx: &Ctx) -> CmdResult<()> {
    // Announce any upgrade-time collision loudly once (§5.6) — disable reaches here directly.
    crate::plugin::collision::check_and_warn(ctx, crate::cli::is_reserved);

    let mut lock = LockFile::load(ctx)?;
    let entry = lock.find_mut(name).ok_or_else(|| not_installed(name))?;

    let was = entry.enabled;
    entry.enabled = enabled;
    let has_hooks = entry.has_plugin_manifest && !entry.hooks.is_empty();
    lock.save(ctx)?;

    if ctx.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "name": name,
                "enabled": enabled,
                "changed": was != enabled,
            }))
            .unwrap()
        );
        return Ok(());
    }
    if ctx.echo_on() {
        let state = if enabled { "enabled" } else { "disabled" };
        if was == enabled {
            ui::frame::emit(
                ui::frame::Symbol::Good,
                &format!("`{name}` is already {state}"),
                ctx,
            );
        } else {
            ui::frame::emit(ui::frame::Symbol::Good, &format!("{state} `{name}`"), ctx);
            if enabled && has_hooks {
                ui::frame::note(
                    "its lifecycle hooks now run at their lifecycle points (you consented above)",
                    ctx,
                );
            }
            if !enabled {
                ui::frame::note(
                    "its managed copy is now skipped in the bin search and its hooks no longer \
                     run; `rackabel plugin run <name>` still invokes it explicitly",
                    ctx,
                );
            }
        }
    }
    Ok(())
}

fn not_installed(name: &str) -> RkError {
    RkError::of(
        ErrorCode::PluginNotFound,
        format!("no plugin named `{name}` is installed"),
        "run `rackabel plugin list`, or `rackabel plugin install OWNER/REPO`",
    )
}
