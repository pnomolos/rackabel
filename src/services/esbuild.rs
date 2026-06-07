//! The build invocation model (DESIGN §4.6, SPEC B §2; SPEC C §3.3).
//!
//! This file FREEZES the build types and the `build_extension` signature that
//! `deploy`/`pack`/`new` call. The body is filled in by the build-owner (it drives
//! esbuild via node, injects the polyfill banner from [`crate::services::banner`],
//! generates `manifest.json`, and validates the bundle). The foundation provides a
//! compiling stub that returns a clear "not implemented yet" error so the rest of
//! the tree builds while command-owners work in parallel.

use std::path::PathBuf;

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::Project;

/// Options controlling a build (DESIGN §2 `build` flags).
///
/// `Default` is all-off / `None` (typecheck `None` = default-on for `--release`).
#[derive(Clone, Debug, Default)]
pub struct BuildOptions {
    /// `--release`: minify on, no sourcemap, typecheck default-on.
    pub release: bool,
    /// `--clean`: wipe `dist/` first.
    pub clean: bool,
    /// `--typecheck`/`--no-typecheck`: `None` = default (on for release).
    pub typecheck: Option<bool>,
    /// `--print-config`: dump the resolved esbuild config and exit.
    pub print_config: bool,
    /// `--dry-run`: print the planned steps, mutate nothing.
    pub dry_run: bool,
    /// `--json`.
    pub json: bool,
}

/// The result of a successful build.
#[derive(Debug)]
pub struct BuildOutcome {
    /// The built bundle (`dist/extension.js`).
    pub entry: PathBuf,
    /// The generated `manifest.json`.
    pub manifest_json: PathBuf,
    pub bytes: u64,
    pub elapsed: std::time::Duration,
    /// Short content hash, printed so "did it rebuild?" is never a mystery.
    pub hash: String,
    /// True when up-to-date and no `--clean`.
    pub skipped: bool,
    pub typechecked: bool,
}

/// THE shared build entry. Resolves node, injects the banner, runs esbuild, writes
/// `manifest.json`, validates. Returns `RK13xx` on build failure, `RK03xx` if no
/// usable node.
///
/// 0.2 FOUNDATION STUB — the build-owner replaces this body. The signature is frozen.
pub fn build_extension(
    project: &Project,
    opts: &BuildOptions,
    _ctx: &Ctx,
) -> CmdResult<BuildOutcome> {
    let _ = (project, opts);
    Err(not_implemented("build"))
}

/// A uniform "not implemented yet" error for the parallel-stub services. Uses the
/// build/runtime class so it never masquerades as an environment problem.
pub(crate) fn not_implemented(what: &str) -> RkError {
    RkError::new(
        ErrorCode::BuildFailed,
        ExitClass::BuildRuntime,
        format!("`{what}` isn't implemented yet"),
        "this command lands later in the 0.2 milestone — track its branch",
    )
}
