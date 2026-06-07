//! Native-dependency service (DESIGN §3.7; SPEC B §3; SPEC C §3.8).
//!
//! This file FREEZES the report types and the `audit`/`copy_native_modules`/`fix`
//! signatures. The full graph-walk + `.node` assertion + copy logic lands with
//! `deploy` (the deploy-owner fills the bodies). `fix` is a STUB returning `RK0304`
//! with a plain-English help line (no pnpm jargon at the user) so `deploy --fix`
//! compiles today; the deploy-owner implements as far as 0.2 allows and logs a
//! DEVIATIONS.md entry if full pnpm automation slips.

use std::path::{Path, PathBuf};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::manifest::{Project, ResolvedExtension};

/// The result of auditing declared native deps.
#[derive(Debug, Default)]
pub struct NativeDepReport {
    pub deps: Vec<NativeDep>,
    /// Declared deps that could not be located on disk.
    pub missing_node: Vec<String>,
}

/// One resolved native dependency.
#[derive(Debug)]
pub struct NativeDep {
    pub name: String,
    pub dir: PathBuf,
    /// The compiled `.node` binary, if found.
    pub dot_node: Option<PathBuf>,
}

/// Walk the runtime graph (deps + optionalDeps, NOT peer), follow pnpm symlinks,
/// assert each declared native dep has a compiled `.node`. Read-only.
///
/// 0.2 FOUNDATION STUB — deploy-owner fills this in. The signature is frozen.
pub fn audit(project: &Project, ext: &ResolvedExtension) -> CmdResult<NativeDepReport> {
    let _ = (project, ext);
    Err(crate::services::esbuild::not_implemented(
        "native-dep audit",
    ))
}

/// Copy declared native deps' `node_modules` subtrees into `<install_dir>/node_modules`.
///
/// 0.2 FOUNDATION STUB — deploy-owner fills this in. The signature is frozen.
pub fn copy_native_modules(report: &NativeDepReport, install_dir: &Path) -> CmdResult<()> {
    let _ = (report, install_dir);
    Err(crate::services::esbuild::not_implemented("native-dep copy"))
}

/// `--fix`: locate/use pnpm under the hood (no pnpm jargon at the user).
///
/// 0.2 STUB — returns `RK0304` with a plain-English help line. Frozen signature so
/// `deploy --fix` compiles; deploy-owner implements as far as 0.2 allows.
pub fn fix(project: &Project, ext: &ResolvedExtension, _ctx: &Ctx) -> CmdResult<()> {
    let _ = (project, ext);
    Err(RkError::of(
        ErrorCode::NativeDepNotCompiled,
        "this extension uses a compiled component that needs to be built",
        "automated native builds aren't wired up yet in this milestone —\n\
         for now, build the native dependency in your project, then rerun `rackabel deploy`.",
    ))
}
