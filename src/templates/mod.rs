//! Tier-1 templates (DESIGN §5.5): `new --template gh:…/@scope/…/<path>` rendering and
//! `new --update`'s copier-style 3-way merge.
//!
//! TEMPLATES-AGENT-OWNED. The on-disk MODELS (`rackabel-template.toml` + the
//! `.rackabel-template` lockfile) and the source classification / git wrapper / network
//! seams are FOUNDATION-owned ([`crate::plugin::template`], [`crate::plugin::source`],
//! [`crate::plugin::git`]); this module is the behavioral layer on top:
//!   - [`render`] — resolve a source to a dir (local verbatim, remote clone behind the
//!     `RACKABEL_TEMPLATE_GIT_BASE` seam with the §5.7 confirmation), run the declared
//!     prompts as the wizard, copy the tree with `{{ key }}` substitution, write the
//!     lockfile;
//!   - [`update`] — re-render old@oldcommit + new@newcommit and 3-way-merge against the
//!     user tree, honoring `[merge].exclude`;
//!   - [`placeholder`] — the deterministic `{{ key }}` substitution syntax;
//!   - [`exclude`] — the `[merge].exclude` glob set (∪ always-excluded binaries).
//!
//! A template is declarative data only — it never depends on rackabel internals, so it
//! can't bit-rot when rackabel changes (the Yeoman-generator-decline lesson, §5.5).

pub mod exclude;
pub mod placeholder;
pub mod render;
pub mod update;

use std::collections::BTreeMap;
use std::path::Path;

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::plugin::source::TemplateSource;

use exclude::ExcludeSet;

/// The outcome of rendering a `--template` into a fresh project directory: the resolved
/// answers (so the caller can echo them) and the resolved display name for next-steps.
pub struct RenderOutcome {
    /// The answers used, by prompt key (already persisted into `.rackabel-template`).
    pub answers: BTreeMap<String, String>,
}

/// Render a `--template <ref>` into `dest` (which must NOT already exist — the caller
/// guards that). This is the full tier-1 path: resolve + (remote) confirm + prompt +
/// render + write the lockfile.
///
/// `accept_yes` is the `--yes` consent (also true under `--no-input` *only* for accepting
/// prompt defaults — the remote confirmation under `--no-input` still refuses unless
/// `--yes` is explicitly set, which the caller passes as `accept_yes`).
pub fn render_into(
    raw: &str,
    dest: &Path,
    accept_yes: bool,
    ctx: &Ctx,
) -> CmdResult<RenderOutcome> {
    let source = TemplateSource::parse(raw).ok_or_else(|| invalid_ref(raw))?;
    let resolved = render::resolve(&source, raw, accept_yes, ctx)?;

    // Run the prompts (no seed — a fresh scaffold).
    let answers = render::run_prompts(&resolved.manifest, &BTreeMap::new(), accept_yes, ctx)?;

    let exclude = ExcludeSet::new(&resolved.manifest.merge.exclude);
    std::fs::create_dir_all(dest).map_err(|e| {
        RkError::new(
            ErrorCode::DeployCopyFailed,
            ExitClass::BuildRuntime,
            "could not create the project directory",
            "check write permissions for the parent directory, then retry",
        )
        .at(dest.display().to_string())
        .raw(e.into())
    })?;
    render::render_tree(&resolved.dir, dest, &answers, &exclude)?;
    render::write_lock(&resolved, dest, &answers)?;

    Ok(RenderOutcome { answers })
}

/// Whether a `--template` ref is a REMOTE source (so the caller knows it will hit the
/// §5.7 confirmation). Used by `new` to decide messaging; classification is the frozen
/// foundation parser.
pub fn is_remote_ref(raw: &str) -> bool {
    TemplateSource::parse(raw)
        .map(|s| s.is_remote())
        .unwrap_or(false)
}

fn invalid_ref(raw: &str) -> RkError {
    RkError::of(
        ErrorCode::UsageError,
        format!("`{raw}` is not a valid template reference"),
        "use gh:owner/repo[@ref], @scope/name, or a local path",
    )
    .at(raw.to_string())
}
