//! Lifecycle wiring: the small per-phase entry points the build/deploy/dev call sites use
//! (DESIGN §5.3). Each resolves the ordered hook list for its kind ([`super::discovery`]),
//! runs each via [`super::engine::run_hook`], and applies the phase's outcome policy:
//!
//!   - [`post_build`]  — after every successful build; informational. Every hook runs;
//!     a nonzero/timeout/spawn failure is logged + skipped and NEVER aborts the build.
//!   - [`pre_deploy`]  — before every deploy copy; the ONE veto. Hooks run in order; the
//!     FIRST veto (nonzero OR timeout) aborts with the §6.1 frame naming the hook (and the
//!     timeout when applicable). A spawn failure of a `pre_deploy` hook also aborts — a
//!     gate the user enabled that cannot run is treated as a refusal, never a silent skip.
//!   - [`on_reload`]   — after a dev reload completes; informational (same policy as
//!     `post_build`).
//!
//! Output discipline (§6.2): the engine routes a hook's stderr / failure notes to
//! rackabel's stderr, so these helpers never write to stdout — the dev chain's single
//! §6.2 line and any `--json` envelope on stdout stay clean even when an on-save hook
//! fails. These functions take the already-built typed payloads so the call sites own the
//! phase facts (build_hash, reload_ms, …) and this module owns only resolution + policy.

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::manifest::Project;

use super::HookKind;
use super::discovery::{self, ResolvedHook};
use super::engine;
use super::outcome::{HookOutcome, VetoDecision};
use super::payload::HookPayload;

/// The project source pair the discovery resolver wants: the project root + its parsed
/// project-local `[hooks]` table (§5.5). `None` when not inside a project (e.g. a
/// `doctor`/`new` run from `~`). Built from a [`Project`] when one is in hand.
fn project_source(
    project: Option<&Project>,
) -> Option<(&std::path::Path, &super::manifest::HooksTable)> {
    let p = project?;
    let table = p.hooks_table()?;
    Some((p.root.as_path(), table))
}

/// Run every `post_build` hook after a successful build (§5.3). Informational: a failing
/// hook is logged (by the engine, to stderr) and skipped; this NEVER returns an error and
/// NEVER aborts the build — a broken third-party `post_build` can't brick `build` (§5.3).
///
/// `project` is the just-built project (its `[hooks]` table is the first source);
/// `payload` is the typed `post_build` stdin object the call site assembled.
pub fn post_build(ctx: &Ctx, project: &Project, payload: &HookPayload) {
    run_informational(ctx, Some(project), HookKind::PostBuild, payload);
}

/// Run every `on_reload` hook after a dev reload completes (§5.3). Informational, same
/// policy as [`post_build`]: logged + skipped, never fatal.
pub fn on_reload(ctx: &Ctx, project: &Project, payload: &HookPayload) {
    run_informational(ctx, Some(project), HookKind::OnReload, payload);
}

/// Run the `pre_deploy` hooks before a deploy copy (§5.3) — the ONE veto. Hooks run in the
/// resolved order; the FIRST one to veto (nonzero exit OR timeout) aborts the deploy with
/// the §6.1 frame naming the hook (RK1310, or RK1311 for a timeout). A `pre_deploy` hook
/// whose command can't be spawned ALSO aborts: a deploy gate the user enabled that fails to
/// run is a refusal, not a silent pass. Returns `Ok(())` when every hook allowed the deploy
/// (including when there are no hooks).
pub fn pre_deploy(ctx: &Ctx, project: &Project, payload: &HookPayload) -> CmdResult<()> {
    let hooks = discovery::resolve(ctx, HookKind::PreDeploy, project_source(Some(project)))?;
    for hook in &hooks {
        // A pre_deploy hook that cannot even start is a refusal (the gate is broken).
        let outcome = match engine::run_hook(hook, payload, ctx) {
            Ok(o) => o,
            Err(e) => return Err(spawn_abort(hook, e)),
        };
        if let HookOutcome::Veto(VetoDecision::Veto { timed_out }) = outcome {
            return Err(veto_error(hook, timed_out));
        }
    }
    Ok(())
}

/// Shared body for the informational phases (`post_build`/`on_reload`): resolve, run each,
/// swallow every failure (the engine already logged it). Resolution failure (e.g. an
/// unreadable lockfile) is itself swallowed — an informational phase must never turn a
/// hook-infrastructure hiccup into a build/reload abort.
fn run_informational(ctx: &Ctx, project: Option<&Project>, kind: HookKind, payload: &HookPayload) {
    let hooks = match discovery::resolve(ctx, kind, project_source(project)) {
        Ok(h) => h,
        Err(_) => return,
    };
    for hook in &hooks {
        // A spawn failure returns Err; for an informational hook we log + skip it (it has
        // already been framed by `spawn_error`). The success path's failure (nonzero/
        // timeout) is logged inside the engine and returned as Informational { failed }.
        if let Err(e) = engine::run_hook(hook, payload, ctx) {
            crate::ui::frame::ewarn(
                &format!(
                    "{} hook from {} skipped: {}",
                    kind,
                    hook.source.label(),
                    e.problem
                ),
                ctx,
            );
        }
    }
}

/// The §6.1 veto frame naming the hook (and its timeout when applicable). RK1311 for a
/// timeout (the bounded-DoS path), RK1310 otherwise.
fn veto_error(hook: &ResolvedHook, timed_out: bool) -> RkError {
    if timed_out {
        RkError::of(
            ErrorCode::HookTimeout,
            format!(
                "the deploy was aborted — the {} {} hook timed out after {}ms",
                hook.source.label(),
                hook.kind,
                hook.timeout_ms
            ),
            "the pre_deploy gate did not complete in time; fix or speed up the hook, raise \
             its [hooks.timeouts] budget, or `rackabel plugin disable` it to deploy without it",
        )
        .at(hook.command_path().display().to_string())
    } else {
        RkError::of(
            ErrorCode::PreDeployVetoed,
            format!(
                "the deploy was aborted by the {} pre_deploy hook",
                hook.source.label()
            ),
            "this gate refused the deploy (e.g. a notarize/lint check); address what it \
             reported above, or `rackabel plugin disable` it to deploy without it",
        )
        .at(hook.command_path().display().to_string())
    }
}

/// A `pre_deploy` hook that cannot be spawned aborts the deploy: re-frame the engine's
/// spawn error as a veto so the exit code is the deploy-aborted class, not a generic one.
fn spawn_abort(hook: &ResolvedHook, e: RkError) -> RkError {
    RkError::of(
        ErrorCode::PreDeployVetoed,
        format!(
            "the deploy was aborted — the {} pre_deploy hook could not be started",
            hook.source.label()
        ),
        "check the hook command exists and is executable (resolved relative to its owning \
         root), or `rackabel plugin disable` it to deploy without it",
    )
    .at(hook.command_path().display().to_string())
    .raw(e.raw.unwrap_or_else(|| anyhow::anyhow!("{}", e.problem)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::payload::{PostBuildPayload, PreDeployPayload};
    use serde_json::json;

    fn ctx(home: &std::path::Path) -> Ctx {
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

    fn pb_payload() -> HookPayload {
        HookPayload::PostBuild(PostBuildPayload {
            project_dir: "/p".to_string(),
            manifest_toml: json!({}),
            bundle_path: Some("/p/dist/extension.js".to_string()),
            build_hash: "h".to_string(),
            kind: "extension".to_string(),
            release: false,
        })
    }

    fn pd_payload() -> HookPayload {
        HookPayload::PreDeploy(PreDeployPayload {
            project_dir: "/p".to_string(),
            manifest_toml: json!({}),
            bundle_path: "/p/dist/extension.js".to_string(),
            user_library: "/ul".to_string(),
            slug: "x".to_string(),
        })
    }

    /// Scaffold a project at `root` with a `[hooks]` table pointing at a script written at
    /// `<root>/<script_name>` with `body`. Returns the discovered `Project`.
    #[cfg(unix)]
    fn project_with_hook(
        root: &std::path::Path,
        hook_line: &str,
        script_name: &str,
        body: &str,
    ) -> Project {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(
            root.join("rackabel.toml"),
            format!(
                "[extension]\nname = \"x\"\nauthor = \"t\"\nversion = \"0.1.0\"\n\
                 minimum_api_version = \"1.0.0\"\n[hooks]\n{hook_line}\n"
            ),
        )
        .unwrap();
        let script = root.join(script_name);
        std::fs::write(&script, body).unwrap();
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        Project::discover(root).unwrap()
    }

    #[test]
    #[cfg(unix)]
    fn post_build_never_aborts_even_when_the_hook_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = project_with_hook(
            &tmp.path().join("proj"),
            "post_build = \"pb.sh\"",
            "pb.sh",
            "#!/bin/sh\ncat >/dev/null\nexit 9\n",
        );
        // No return value — it must simply not panic and not error.
        post_build(&ctx(tmp.path()), &proj, &pb_payload());
    }

    #[test]
    #[cfg(unix)]
    fn post_build_with_no_hooks_is_a_noop() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("proj")).unwrap();
        std::fs::write(
            tmp.path().join("proj/rackabel.toml"),
            "[extension]\nname = \"x\"\nauthor = \"t\"\nversion = \"0.1.0\"\nminimum_api_version = \"1.0.0\"\n",
        )
        .unwrap();
        let proj = Project::discover(&tmp.path().join("proj")).unwrap();
        post_build(&ctx(tmp.path()), &proj, &pb_payload());
    }

    #[test]
    #[cfg(unix)]
    fn pre_deploy_allows_on_clean_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = project_with_hook(
            &tmp.path().join("proj"),
            "pre_deploy = \"pd.sh\"",
            "pd.sh",
            "#!/bin/sh\ncat >/dev/null\nexit 0\n",
        );
        assert!(pre_deploy(&ctx(tmp.path()), &proj, &pd_payload()).is_ok());
    }

    #[test]
    #[cfg(unix)]
    fn pre_deploy_nonzero_aborts_with_rk1310() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = project_with_hook(
            &tmp.path().join("proj"),
            "pre_deploy = \"pd.sh\"",
            "pd.sh",
            "#!/bin/sh\ncat >/dev/null\nexit 1\n",
        );
        let err = pre_deploy(&ctx(tmp.path()), &proj, &pd_payload()).unwrap_err();
        assert_eq!(err.code, ErrorCode::PreDeployVetoed);
        // The frame names the source.
        assert!(
            err.problem.contains("project"),
            "names the hook: {}",
            err.problem
        );
    }

    #[test]
    #[cfg(unix)]
    fn pre_deploy_timeout_aborts_with_rk1311() {
        let tmp = tempfile::tempdir().unwrap();
        // A timeout override of 200ms in [hooks.timeouts] against a 5s sleep.
        let root = tmp.path().join("proj");
        let proj = {
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(
                root.join("rackabel.toml"),
                "[extension]\nname=\"x\"\nauthor=\"t\"\nversion=\"0.1.0\"\nminimum_api_version=\"1.0.0\"\n\
                 [hooks]\npre_deploy = \"pd.sh\"\n[hooks.timeouts]\npre_deploy = 200\n",
            )
            .unwrap();
            let s = root.join("pd.sh");
            std::fs::write(&s, "#!/bin/sh\ncat >/dev/null\nsleep 5\n").unwrap();
            let mut p = std::fs::metadata(&s).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&s, p).unwrap();
            Project::discover(&root).unwrap()
        };
        let start = std::time::Instant::now();
        let err = pre_deploy(&ctx(tmp.path()), &proj, &pd_payload()).unwrap_err();
        assert_eq!(err.code, ErrorCode::HookTimeout);
        assert!(
            start.elapsed() < std::time::Duration::from_secs(4),
            "the timeout must fire fast, not wait for the 5s sleep"
        );
    }

    #[test]
    #[cfg(unix)]
    fn pre_deploy_missing_command_aborts() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("rackabel.toml"),
            "[extension]\nname=\"x\"\nauthor=\"t\"\nversion=\"0.1.0\"\nminimum_api_version=\"1.0.0\"\n\
             [hooks]\npre_deploy = \"nope.sh\"\n",
        )
        .unwrap();
        let proj = Project::discover(&root).unwrap();
        let err = pre_deploy(&ctx(tmp.path()), &proj, &pd_payload()).unwrap_err();
        assert_eq!(err.code, ErrorCode::PreDeployVetoed);
    }
}
