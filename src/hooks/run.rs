//! High-level hook runners for the `doctor_check` and `new_template` verbs (DESIGN §5.3).
//!
//! HOOK-VERBS-OWNED (milestone 0.5). The foundation froze the contract (kinds, payloads,
//! outcomes), discovery (the ordered enabled-plugin + project-local source list), and the
//! engine ([`super::engine::run_hook`]). This module wires those together for the two
//! ENUMERATE-style verbs this agent owns:
//!   - [`doctor_check_rows`] runs every discovered `doctor_check` hook and returns one
//!     resolved row per hook (the a-d precedence is applied in the engine/outcome layer);
//!   - [`new_template_choices`] runs every discovered `new_template` hook BEFORE the wizard
//!     and returns the wizard choices they contributed (nonzero/timeout ⇒ omitted, logged).
//!
//! Both honor the §5.7 consent gate transparently: discovery only yields project-local
//! `[hooks]` (implicit trust) and ENABLED plugins, so an installed-but-unenabled plugin
//! contributes nothing here without this module checking the flag itself.

use std::path::Path;

use crate::context::Ctx;
use crate::error::CmdResult;

use super::HookKind;
use super::discovery::{self, ResolvedHook};
use super::engine;
use super::manifest::HooksTable;
use super::outcome::{DoctorResolution, HookOutcome, TemplateChoice};
use super::payload::{DoctorCheckPayload, HookPayload, NewTemplatePayload, manifest_toml_object};

/// One resolved `doctor_check` row: the contributing plugin/source label plus the resolved
/// [`DoctorResolution`]. `doctor` renders it in the standard checklist alongside the
/// built-in rows, attributing it to the plugin by name.
#[derive(Debug, Clone, PartialEq)]
pub struct DoctorCheckRow {
    /// The hook source label (e.g. `"plugin notarize"` / `"project"`) — used to name the
    /// row and its generic-fail message (`doctor_check <name> failed`).
    pub source_label: String,
    /// The plugin name when this came from a plugin (for the `doctor_check <name> failed`
    /// generic row + the row id); `None` for a project-local hook.
    pub plugin_name: Option<String>,
    /// The resolved row (the a-d precedence already applied by the engine).
    pub resolution: DoctorResolution,
}

/// Run every discovered, enabled `doctor_check` hook (project-local first, then enabled
/// plugins in lock order) and return one [`DoctorCheckRow`] each (§5.3).
///
/// `project` is the discovered project root + parsed `[hooks]` table when `doctor` runs
/// INSIDE a project, else `None` — and in the no-project case BOTH payload fields are absent
/// (`doctor` is an environment command that runs outside a project, §5.2/§6.2; a
/// `doctor_check` hook MUST tolerate that). A spawn failure or any non-line outcome is folded
/// into a row (never propagated as a command error): `doctor`'s own checklist IS the output.
pub fn doctor_check_rows(
    ctx: &Ctx,
    project: Option<(&Path, &HooksTable)>,
) -> CmdResult<Vec<DoctorCheckRow>> {
    let hooks = discovery::resolve(ctx, HookKind::DoctorCheck, project)?;
    if hooks.is_empty() {
        return Ok(Vec::new());
    }

    // Build the stdin payload ONCE: {project_dir?, manifest_toml?} — both absent outside a
    // project (omitted-not-empty, never "" / null). The same object goes to every hook.
    let payload = doctor_check_payload(project);

    let mut rows = Vec::with_capacity(hooks.len());
    for hook in &hooks {
        let resolution = match engine::run_hook(hook, &payload, ctx) {
            Ok(HookOutcome::Doctor(res)) => res,
            // A spawn failure (RK1309) for an informational/enumerate hook is logged + folded
            // into a generic-fail row, not a fatal `doctor` exit — the contract says these
            // hooks never abort their phase.
            Ok(_) | Err(_) => DoctorResolution::GenericFail,
        };
        rows.push(DoctorCheckRow {
            source_label: hook.source.label(),
            plugin_name: plugin_name_of(hook),
            resolution,
        });
    }
    Ok(rows)
}

/// Build the `doctor_check` stdin payload: `{project_dir?, manifest_toml?}`. Both fields are
/// ABSENT outside a project; inside one, `manifest_toml` is the PARSED `rackabel.toml` as a
/// JSON object (not a path, §5.3). A manifest that no longer parses degrades to no
/// `manifest_toml` (the hook still gets `project_dir` and must tolerate a partial payload).
fn doctor_check_payload(project: Option<(&Path, &HooksTable)>) -> HookPayload {
    let (project_dir, manifest_toml) = match project {
        Some((root, _)) => {
            let manifest_path = root.join(crate::manifest::MANIFEST_NAME);
            let manifest_toml = std::fs::read_to_string(&manifest_path)
                .ok()
                .and_then(|text| manifest_toml_object(&text).ok());
            (Some(root.display().to_string()), manifest_toml)
        }
        None => (None, None),
    };
    HookPayload::DoctorCheck(DoctorCheckPayload {
        project_dir,
        manifest_toml,
    })
}

/// One `new_template` wizard choice contributed by an enumerate hook (§5.3): the choice
/// itself plus the source label for the wizard's pick-list line + a logged-omission note.
#[derive(Debug, Clone, PartialEq)]
pub struct TemplateHookChoice {
    /// The source label (e.g. `"plugin starter-kit"`), shown in the wizard pick-list so the
    /// user knows which plugin offered the template.
    pub source_label: String,
    /// The classified choice (an absolute path or a `gh:`/`@scope` ref).
    pub choice: TemplateChoice,
}

/// Run every discovered, enabled `new_template` hook BEFORE the wizard's template prompt and
/// return the wizard choices they contributed (§5.3). Passes `{kind}` ONLY (no
/// `wizard_answers`, no `project_dir` — neither exists pre-wizard). A hook that prints
/// nothing, exits nonzero, or times out contributes NO choice (omitted, logged). An
/// installed-but-unenabled plugin contributes nothing (discovery never yields it).
///
/// Returns `(choices, omitted)` — `omitted` is the list of source labels whose hook ran but
/// produced no usable choice, so the caller can log a single honest note. The choice, if
/// picked, renders through ordinary tier-1 machinery — there is NO second call to the hook.
pub fn new_template_choices(
    ctx: &Ctx,
    kind: &str,
) -> CmdResult<(Vec<TemplateHookChoice>, Vec<String>)> {
    // new_template runs PRE-wizard, before any project exists — so there is no project source
    // (a project-local [hooks] new_template would have no project to anchor to either; the
    // discovery resolver takes `None` here, yielding only enabled plugins).
    let hooks = discovery::resolve(ctx, HookKind::NewTemplate, None)?;
    if hooks.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    let payload = HookPayload::NewTemplate(NewTemplatePayload {
        kind: kind.to_string(),
    });

    let mut choices = Vec::new();
    let mut omitted = Vec::new();
    for hook in &hooks {
        let label = hook.source.label();
        match engine::run_hook(hook, &payload, ctx) {
            Ok(HookOutcome::Template(Some(choice))) => {
                choices.push(TemplateHookChoice {
                    source_label: label,
                    choice,
                });
            }
            // No usable choice (empty/nonzero/timeout) OR a spawn failure ⇒ omitted + logged.
            Ok(_) | Err(_) => omitted.push(label),
        }
    }
    Ok((choices, omitted))
}

/// The plugin name behind a resolved hook, if it came from a plugin (vs project-local).
fn plugin_name_of(hook: &ResolvedHook) -> Option<String> {
    match &hook.source {
        super::HookSource::Plugin { name, .. } => Some(name.clone()),
        super::HookSource::Project { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::lock::{LockFile, PluginLockEntry, SourceKind};
    use crate::plugin::plugin_store_dir;
    use std::path::PathBuf;
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

    /// Install a fake plugin whose `doctor_check`/`new_template` hook is a shell script.
    #[cfg(unix)]
    fn install_script_plugin(
        ctx: &Ctx,
        name: &str,
        enabled: bool,
        kind: HookKind,
        script_body: &str,
    ) {
        use std::os::unix::fs::PermissionsExt;
        let store = plugin_store_dir(ctx, name);
        std::fs::create_dir_all(&store).unwrap();
        let hook_rel = "hook.sh";
        let hook_path = store.join(hook_rel);
        std::fs::write(&hook_path, format!("#!/bin/sh\n{script_body}\n")).unwrap();
        let mut perms = std::fs::metadata(&hook_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&hook_path, perms).unwrap();
        std::fs::write(
            store.join("rackabel-plugin.toml"),
            format!("[hooks]\n{} = \"{hook_rel}\"\n", kind.as_str()),
        )
        .unwrap();

        let mut lock = LockFile::load(ctx).unwrap();
        lock.upsert(PluginLockEntry {
            name: name.to_string(),
            source: SourceKind::Path,
            origin: format!("/x/rackabel-{name}"),
            commit: None,
            sha256: Some("ab".to_string()),
            installed_at: "2026-06-07T00:00:00Z".to_string(),
            executable: PathBuf::from(format!(".rackabel/plugins/bin/rackabel-{name}")),
            has_plugin_manifest: true,
            hooks: vec![kind.as_str().to_string()],
            enabled,
        });
        lock.save(ctx).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn doctor_check_rows_apply_precedence_and_attribute_to_plugin() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        install_script_plugin(
            &c,
            "notarize",
            true,
            HookKind::DoctorCheck,
            r#"echo '{"symbol":"warn","message":"creds missing"}'; exit 1"#,
        );
        let rows = doctor_check_rows(&c, None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source_label, "plugin notarize");
        assert_eq!(rows[0].plugin_name.as_deref(), Some("notarize"));
        match &rows[0].resolution {
            DoctorResolution::Line(l) => {
                assert_eq!(l.message, "creds missing");
            }
            other => panic!("expected the stdout line to win, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn doctor_check_unenabled_plugin_contributes_nothing() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        install_script_plugin(
            &c,
            "notarize",
            false, // NOT enabled.
            HookKind::DoctorCheck,
            r#"echo '{"symbol":"ok","message":"x"}'"#,
        );
        assert!(doctor_check_rows(&c, None).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn doctor_check_outside_project_sends_no_project_fields() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        // The hook fails (fail row) ONLY if it sees a project_dir; with no project it passes.
        install_script_plugin(
            &c,
            "p",
            true,
            HookKind::DoctorCheck,
            r#"in=$(cat); case "$in" in *project_dir*) echo '{"symbol":"fail","message":"leaked"}';; *) echo '{"symbol":"ok","message":"no-project ok"}';; esac"#,
        );
        let rows = doctor_check_rows(&c, None).unwrap();
        match &rows[0].resolution {
            DoctorResolution::Line(l) => assert_eq!(l.message, "no-project ok"),
            other => panic!("expected ok line, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn new_template_choice_appears_and_unenabled_is_silent() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        install_script_plugin(
            &c,
            "starter",
            true,
            HookKind::NewTemplate,
            "echo gh:acme/house-starter@v1",
        );
        install_script_plugin(
            &c,
            "hidden",
            false, // unenabled — contributes nothing.
            HookKind::NewTemplate,
            "echo /opt/templates/secret",
        );
        let (choices, omitted) = new_template_choices(&c, "extension").unwrap();
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].source_label, "plugin starter");
        assert_eq!(
            choices[0].choice,
            TemplateChoice::Ref("gh:acme/house-starter@v1".to_string())
        );
        assert!(omitted.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn new_template_nonzero_is_omitted_and_logged() {
        let tmp = tempdir().unwrap();
        let c = ctx(tmp.path());
        install_script_plugin(
            &c,
            "broken",
            true,
            HookKind::NewTemplate,
            "echo /opt/x; exit 2",
        );
        let (choices, omitted) = new_template_choices(&c, "extension").unwrap();
        assert!(choices.is_empty());
        assert_eq!(omitted, vec!["plugin broken".to_string()]);
    }
}
