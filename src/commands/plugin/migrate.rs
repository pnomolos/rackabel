//! `rackabel plugin migrate <name>` (DESIGN §5.3) — the hook-contract codemod SURFACE.
//!
//! OWNED BY THE HOOK-VERBS AGENT. Hook-contract changes ship ONE at a time, each with a
//! `plugin migrate` codemod (the ESLint-v9 lesson: never batch breaking plugin-contract
//! changes; tooling, not docs, drives adoption). Today the supported tier-3 contract is
//! [`crate::hooks::HOOK_API`] = 1 and NO migrations exist yet, so this verb ships the
//! detect-and-frame SURFACE only — the codemod machinery is deferred (DEVIATIONS D-100):
//!   - a plugin declaring `hook_api == HOOK_API` ⇒ "nothing to migrate" (success, exit 0);
//!   - a plugin declaring `hook_api > HOOK_API` ⇒ the clear "unsupported" frame
//!     (`RK0104 MigrateUnsupported`, exit 2) — no codemod to run, NOT a crash;
//!   - a plugin declaring `hook_api < HOOK_API` would be the case a real codemod handles;
//!     with no migrations shipped, a lower version is already compatible with this build's
//!     additive-floor reading, so it is also "nothing to migrate".
//!
//! `--json` emits the detected vs supported versions + the decision, so a CI gate can read
//! the state machine-readably (§7).

use crate::cli::PluginMigrateArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::hooks::HOOK_API;
use crate::plugin::lock::LockFile;
use crate::plugin::{plugin_store_dir, store};
use crate::ui;

pub fn run(args: &PluginMigrateArgs, ctx: &Ctx) -> CmdResult<()> {
    // Announce any upgrade-time collision loudly once (§5.6) on every plugin command.
    crate::plugin::collision::check_and_warn(ctx, crate::cli::is_reserved);

    let lock = LockFile::load(ctx)?;
    let entry = lock
        .find(&args.name)
        .ok_or_else(|| not_installed(&args.name))?;

    // The declared hook_api: read from the plugin's store-dir manifest if present; a plugin
    // with no `rackabel-plugin.toml` (a plain tier-2 PATH subcommand) targets nothing — it
    // has no hooks to migrate, so it is trivially "nothing to migrate".
    let declared = declared_hook_api(ctx, &args.name, entry.has_plugin_manifest);
    decide_and_report(ctx, &args.name, declared)
}

/// The `hook_api` the plugin declares. `None` when the plugin carries no hook manifest (a
/// tier-2-only plugin) — there is nothing to migrate. A manifest with no explicit `hook_api`
/// key defaults to the v1 floor (see [`crate::plugin::manifest::PluginManifest`]).
fn declared_hook_api(ctx: &Ctx, name: &str, has_manifest: bool) -> Option<u32> {
    if !has_manifest {
        return None;
    }
    let store_dir = plugin_store_dir(ctx, name);
    match store::load_plugin_manifest(&store_dir) {
        Ok(Some(m)) => Some(m.declared_hook_api()),
        // No manifest on disk ⇒ nothing to migrate.
        Ok(None) => None,
        // A corrupt/unreadable manifest is treated as "nothing to migrate" too (this is a
        // best-effort lookup feeding the migrate decision — a parse error here is not fatal
        // and is deliberately swallowed; the authoritative parse error surfaces at install
        // or at hook-run time, not from this read).
        Err(_) => None,
    }
}

/// Apply the §5.3 decision for a `(declared, supported)` pair and report it (human or JSON).
fn decide_and_report(ctx: &Ctx, name: &str, declared: Option<u32>) -> CmdResult<()> {
    let supported = HOOK_API;

    // The decision string is shared by the JSON + human surfaces so they never drift.
    let (decision, unsupported) = match declared {
        // No hook manifest at all: nothing to migrate.
        None => ("nothing-to-migrate", false),
        Some(v) if v == supported => ("nothing-to-migrate", false),
        // A LOWER declared version is already compatible with this build (additive-floor) and
        // no down-migration ships — nothing to do.
        Some(v) if v < supported => ("nothing-to-migrate", false),
        // A HIGHER declared version needs a codemod this build does not ship.
        Some(_) => ("unsupported", true),
    };

    if ctx.json {
        let obj = serde_json::json!({
            "plugin": name,
            "declared_hook_api": declared,
            "supported_hook_api": supported,
            "decision": decision,
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap());
        // JSON callers still get the non-zero EXIT for an unsupported migration (so a CI gate
        // reads both the body AND the status), matching the human path. We already printed the
        // complete decision object above, so mark the error `json_handled()` — otherwise `main`
        // would render a SECOND JSON envelope on stdout (the double-print §7 forbids).
        if unsupported {
            return Err(
                unsupported_frame(name, declared.unwrap_or(supported), supported).json_handled(),
            );
        }
        return Ok(());
    }

    if unsupported {
        return Err(unsupported_frame(
            name,
            declared.unwrap_or(supported),
            supported,
        ));
    }

    if ctx.echo_on() {
        let detail = match declared {
            Some(v) => format!("declares hook_api {v}; this build supports {supported}"),
            None => format!(
                "carries no lifecycle-hook manifest; this build supports hook_api {supported}"
            ),
        };
        ui::frame::emit(
            ui::frame::Symbol::Good,
            &format!("`{name}`: nothing to migrate ({detail})"),
            ctx,
        );
    }
    Ok(())
}

/// The clear "unsupported migration" frame (RK0104, exit 2): the plugin targets a NEWER
/// contract than this build, and no codemod for that bump ships (D-100). Distinct from
/// `HookApiUnsupported` (RK0405), which fires at hook-RUN time.
fn unsupported_frame(name: &str, declared: u32, supported: u32) -> RkError {
    RkError::of(
        ErrorCode::MigrateUnsupported,
        format!(
            "`{name}` targets hook_api {declared}, but this build supports {supported} and \
             ships no codemod for that bump"
        ),
        "upgrade rackabel to a build that supports the plugin's hook_api (its release ships \
         the migration), then rerun `plugin migrate`; or, if you authored the plugin, target \
         the hook_api this rackabel supports",
    )
    .at(format!(
        "declared hook_api {declared}, supported {supported}"
    ))
}

fn not_installed(name: &str) -> RkError {
    RkError::of(
        ErrorCode::PluginNotFound,
        format!("no plugin named `{name}` is installed"),
        "run `rackabel plugin list`, or `rackabel plugin install OWNER/REPO`",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::lock::{PluginLockEntry, SourceKind};
    use std::path::{Path, PathBuf};
    use tempfile::tempdir;

    fn ctx(home: &Path) -> Ctx {
        Ctx {
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

    fn install(ctx: &Ctx, name: &str, has_manifest: bool, manifest: Option<&str>) {
        let store = plugin_store_dir(ctx, name);
        std::fs::create_dir_all(&store).unwrap();
        if let Some(m) = manifest {
            std::fs::write(store.join("rackabel-plugin.toml"), m).unwrap();
        }
        let mut lock = LockFile::load(ctx).unwrap();
        lock.upsert(PluginLockEntry {
            name: name.to_string(),
            source: SourceKind::Path,
            origin: format!("/x/rackabel-{name}"),
            commit: None,
            sha256: Some("ab".to_string()),
            installed_at: "2026-06-07T00:00:00Z".to_string(),
            executable: PathBuf::from(format!(".rackabel/plugins/bin/rackabel-{name}")),
            has_plugin_manifest: has_manifest,
            hooks: vec![],
            hooks_digest: None,
            enabled: false,
        });
        lock.save(ctx).unwrap();
    }

    #[test]
    fn hook_api_one_is_nothing_to_migrate() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        install(
            &c,
            "p",
            true,
            Some("hook_api = 1\n[hooks]\npost_build = \"bin/pb\"\n"),
        );
        assert!(run(&PluginMigrateArgs { name: "p".into() }, &c).is_ok());
    }

    #[test]
    fn no_manifest_is_nothing_to_migrate() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        install(&c, "plain", false, None);
        assert!(
            run(
                &PluginMigrateArgs {
                    name: "plain".into()
                },
                &c
            )
            .is_ok()
        );
    }

    #[test]
    fn higher_hook_api_is_unsupported_frame() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        install(
            &c,
            "future",
            true,
            Some("hook_api = 2\n[hooks]\npost_build = \"bin/pb\"\n"),
        );
        let err = run(
            &PluginMigrateArgs {
                name: "future".into(),
            },
            &c,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::MigrateUnsupported);
        assert_eq!(err.class, crate::error::ExitClass::Usage);
    }

    #[test]
    fn unknown_plugin_is_not_found() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        let err = run(
            &PluginMigrateArgs {
                name: "nope".into(),
            },
            &c,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::PluginNotFound);
    }

    #[test]
    fn absent_hook_api_defaults_to_floor_nothing_to_migrate() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        // A manifest with hooks but no explicit hook_api ⇒ floor v1 ⇒ nothing to migrate.
        install(
            &c,
            "implicit",
            true,
            Some("[hooks]\npost_build = \"bin/pb\"\n"),
        );
        assert!(
            run(
                &PluginMigrateArgs {
                    name: "implicit".into()
                },
                &c
            )
            .is_ok()
        );
    }
}
