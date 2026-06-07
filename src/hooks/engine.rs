//! The hook-execution engine — FROZEN SIGNATURE, stub body (DESIGN §5.3, §5.7).
//!
//! FOUNDATION-OWNED SIGNATURE; the BODY is a feature agent's. [`run_hook`] is the single
//! entry point that the build/deploy/dev/doctor/new call sites will use to execute one
//! resolved hook. The foundation freezes its shape and lands a compiling stub that
//! returns a clear framed error (`RK1309`) so the whole tree builds; the feature agent
//! replaces the stub with the real subprocess machinery WITHOUT changing the signature.
//!
//! ## What the real body must do (the §5.3 execution contract, recorded here so the
//! signature is unambiguous)
//!   - Spawn the resolved command ([`super::discovery::ResolvedHook::command_path`]) as a
//!     subprocess with the FULL §5.2 env contract
//!     ([`crate::plugin::env_contract::build`]) PLUS `RACKABEL_HOOK_API` =
//!     [`super::HOOK_API`] ([`super::RACKABEL_HOOK_API_ENV`]).
//!   - Write EXACTLY the one JSON payload object ([`super::payload::HookPayload::to_json`])
//!     to the child's stdin, then CLOSE stdin (EOF framing): a hook that reads to EOF
//!     terminates naturally; one that blocks for more input hits the timeout.
//!   - Enforce the per-hook wall-clock timeout (`ResolvedHook::timeout_ms`, default
//!     [`super::DEFAULT_TIMEOUT_MS`]): on overrun send SIGTERM, then SIGKILL after a
//!     [`super::TIMEOUT_GRACE_MS`] grace, and treat the hook EXACTLY like a nonzero exit.
//!   - Map `(stdout, exit_code, timed_out)` to a [`HookOutcome`] per the hook's row:
//!       * `post_build`/`on_reload` ⇒ [`HookOutcome::Informational`] (stdout ignored;
//!         failure logged + skipped, never fatal);
//!       * `pre_deploy` ⇒ [`HookOutcome::Veto`] (`Allow` on exit 0, `Veto` on nonzero OR
//!         timeout — the ONE veto);
//!       * `doctor_check` ⇒ [`HookOutcome::Doctor`] via
//!         [`super::outcome::DoctorLine::resolve`] (the a-d precedence);
//!       * `new_template` ⇒ [`HookOutcome::Template`] via
//!         [`super::outcome::TemplateChoice::parse`] (the choice, or `None`).
//!   - A hanging hook MUST be reaped by the timeout machinery itself (no orphaned child).

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};

use super::discovery::ResolvedHook;
use super::outcome::HookOutcome;
use super::payload::HookPayload;

/// Run ONE resolved hook with its payload (DESIGN §5.3). FROZEN SIGNATURE.
///
/// `hook` carries the source (project-local vs an enabled plugin — for trust + framing),
/// the resolved command, and the wall-clock timeout. `payload` is the typed §5.3 stdin
/// object for the hook's kind; the kind MUST match `hook.kind` (debug-asserted).
///
/// Returns the [`HookOutcome`] the caller interprets per the phase (informational hooks
/// are swallowed; a `pre_deploy` `Veto` aborts the deploy; a `doctor_check` produces a
/// row; a `new_template` contributes a wizard choice). A framed `RkError` is reserved for
/// an engine-level failure that even an informational hook can't swallow (e.g. the command
/// path does not exist) — the feature body decides that boundary; the STUB returns the
/// not-implemented frame below.
pub fn run_hook(hook: &ResolvedHook, payload: &HookPayload, ctx: &Ctx) -> CmdResult<HookOutcome> {
    debug_assert_eq!(
        hook.kind,
        payload.kind(),
        "run_hook: payload kind must match the resolved hook's kind"
    );
    let _ = (hook, payload, ctx);
    // FOUNDATION STUB — the real subprocess/timeout body is a 0.5 feature agent's. It must
    // not be reachable from a shipped command path until that body lands; until then this
    // returns a clear framed error rather than silently pretending a hook ran.
    Err(RkError::of(
        ErrorCode::HookFailed,
        "the lifecycle-hook engine is not yet implemented in this build",
        "this is a foundation stub; the 0.5 feature agent lands run_hook's body",
    )
    .at(format!("{} hook from {}", hook.kind, hook.source.label())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::payload::NewTemplatePayload;
    use crate::hooks::{HookKind, HookSource};

    fn ctx() -> Ctx {
        Ctx {
            no_input: true,
            json: false,
            quiet: false,
            verbose: false,
            raw: false,
            color: crate::ui::color::ColorMode::Never,
            color_err: crate::ui::color::ColorMode::Never,
            cwd: std::path::PathBuf::from("/tmp"),
            rackabel_home: std::path::PathBuf::from("/tmp/.rackabel"),
            home: std::path::PathBuf::from("/tmp"),
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
    fn stub_returns_a_clear_frame_not_a_fake_success() {
        let hook = ResolvedHook {
            source: HookSource::Project {
                project_root: std::path::PathBuf::from("/p"),
            },
            kind: HookKind::NewTemplate,
            command: "bin/template".to_string(),
            timeout_ms: 30_000,
        };
        let payload = HookPayload::NewTemplate(NewTemplatePayload {
            kind: "extension".to_string(),
        });
        let err = run_hook(&hook, &payload, &ctx()).unwrap_err();
        assert_eq!(err.code, ErrorCode::HookFailed);
    }
}
