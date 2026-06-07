//! `rackabel validate` — lint manifest + artifact against ship rules (DESIGN §2).
//!
//! A pass/fail checklist printed in the §2 output shape. Each rule is a [`Check`]
//! with a [`Status`] (pass / fail / warning / skipped) and a one-line message; a
//! tail summary reports `N failed, M warning`. Exit `4` (validation) on any failure,
//! `0` on warnings-only. `--strict` promotes every warning to a failure. The global
//! `--json` flag emits the same results as a machine-readable array.
//!
//! The rules (DESIGN §2 validate):
//!   1. manifest completeness — name/author/entry/version/minimumApiVersion present.
//!   2. `minimumApiVersion ≤ detected host apiVersion` (skip-with-note when no Live).
//!   3. version bumped vs the last packed version (`.rackabel/state.toml`).
//!   4. CHANGELOG entry present for the current version.
//!   5. native `.node` present + matching target (when native deps are declared).
//!   6. stable-identifier drift (warning-tier; see the DEVIATIONS note below).
//!
//! ## What is *checkable* on disk (SPEC A §2)
//! The SDK manifest holds exactly five fields — commands and context-menu actions
//! are registered in *code* at runtime, never declared on disk. So:
//!   - The **host's** real apiVersion is only knowable at runtime
//!     (`ActivationContext.hostApiVersion`); the only static source is the SDK
//!     bundle's `EXTENSIONS_API_VERSIONS` (`["1.0.0"]`). We treat a detected Live
//!     install as evidence the host supports that known apiVersion and otherwise
//!     skip-with-note. (DEVIATIONS D-11.)
//!   - **Stable-identifier drift** cannot diff command ids that never appear on disk.
//!     What the manifest DOES carry is the extension `name` (the identifier Live keys
//!     saved state on), so the finalized 0.5 rule diffs the current manifest `name`
//!     against the last-packed snapshot in `.rackabel/state.toml` and warns on a
//!     rename; the command-id surface stays deferred. (DEVIATIONS D-12/D-102.)

use std::path::Path;

use serde_json::json;

use crate::cli::ValidateArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::{self, Project, ResolvedExtension};
use crate::services::live;
use crate::ui;

/// The apiVersion the SDK bundle advertises on disk (SPEC A §2,
/// `EXTENSIONS_API_VERSIONS[0]`). The host's real version is only knowable at
/// runtime; this constant is the static stand-in used when a Live install is found.
const HOST_API_VERSION: &str = "1.0.0";

/// The result class of a single check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Rule satisfied (`✓`).
    Pass,
    /// Rule violated (`✗`) — a validation failure (exit 4).
    Fail,
    /// A compatibility risk (`warning:`) — non-fatal unless `--strict`.
    Warn,
    /// Not applicable on this project / could not be checked (note, never fatal).
    Skip,
}

/// One check result: a status plus the line printed after the symbol.
#[derive(Debug, Clone)]
pub struct Check {
    /// A stable identifier for the rule (used in `--json`).
    pub id: &'static str,
    pub status: Status,
    /// The human-readable line (the §2 output shape).
    pub message: String,
}

impl Check {
    fn pass(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: Status::Pass,
            message: message.into(),
        }
    }
    fn fail(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: Status::Fail,
            message: message.into(),
        }
    }
    fn warn(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: Status::Warn,
            message: message.into(),
        }
    }
    fn skip(id: &'static str, message: impl Into<String>) -> Self {
        Self {
            id,
            status: Status::Skip,
            message: message.into(),
        }
    }
}

pub fn run(args: &ValidateArgs, ctx: &Ctx) -> CmdResult<()> {
    let project = Project::discover_cwd(ctx)?;
    // Resolve with inference. Under --json the inference echoes are suppressed by
    // `resolved_extension` (it honors `ctx.echo_on()`), so the JSON stays clean.
    let ext = project.resolved_extension(ctx)?;

    let checks = collect_checks(&project, &ext, ctx);
    report(&checks, args.strict, ctx)
}

/// Run every rule and gather the results, in §2 display order.
fn collect_checks(project: &Project, ext: &ResolvedExtension, ctx: &Ctx) -> Vec<Check> {
    let mut checks = vec![
        check_manifest_complete(project, ext),
        check_api_version(ext, ctx),
        check_version_bumped(project, ext),
        check_changelog(project, ext),
    ];
    checks.extend(check_native_node(project, ext));
    checks.push(check_identifier_drift(project, ext));
    checks
}

// --- individual rules -------------------------------------------------------

/// Rule 1: every required SDK manifest field is present after inference.
///
/// The five fields are name/author/entry/version/minimumApiVersion (SPEC A §2). A
/// field that resolved to empty (an author with no `[extension].author` and no git
/// `user.name`) is a failure: a distributable manifest must name a real author.
/// `version`, `entry`, and `minimum_api_version` always resolve to a concrete value
/// via inference, so the only field that can be *empty* after inference is `author`.
fn check_manifest_complete(_project: &Project, ext: &ResolvedExtension) -> Check {
    let mut missing: Vec<&str> = Vec::new();
    if ext.name.trim().is_empty() {
        missing.push("name");
    }
    if ext.author.trim().is_empty() {
        missing.push("author");
    }
    if ext.entry.as_os_str().is_empty() {
        missing.push("entry");
    }
    // version and minimum_api_version are semver-typed and always concrete here.

    if missing.is_empty() {
        Check::pass("manifest-complete", "manifest complete")
    } else {
        Check::fail(
            "manifest-complete",
            format!(
                "manifest incomplete — missing {} (set {} in {})",
                missing.join(", "),
                missing
                    .iter()
                    .map(|m| format!("[extension].{m}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                manifest::MANIFEST_NAME,
            ),
        )
    }
}

/// Rule 2: `minimumApiVersion ≤ detected host apiVersion`.
///
/// The host's real apiVersion is only knowable at runtime; statically we treat a
/// detected Live install (with a host module) as supporting the SDK's known
/// `EXTENSIONS_API_VERSIONS[0]`. When no Live is found we cannot compare, so we
/// skip-with-note (DESIGN §2: "skip-with-note when no Live found").
fn check_api_version(ext: &ResolvedExtension, ctx: &Ctx) -> Check {
    let host_present = live::detect(ctx).iter().any(|i| i.host_module.is_some());
    if !host_present {
        return Check::skip(
            "api-version",
            format!(
                "minimumApiVersion {} — skipped (no Ableton Live found to compare against)",
                ext.minimum_api_version
            ),
        );
    }
    let host = semver::Version::parse(HOST_API_VERSION).expect("HOST_API_VERSION is valid semver");
    if ext.minimum_api_version <= host {
        Check::pass(
            "api-version",
            format!(
                "minimumApiVersion {} ≤ host {host}",
                ext.minimum_api_version
            ),
        )
    } else {
        Check::fail(
            "api-version",
            format!(
                "minimumApiVersion {} > host {host} — the host cannot load this extension \
                 (lower [extension].minimum_api_version or upgrade Live)",
                ext.minimum_api_version
            ),
        )
    }
}

/// Rule 3: the current version is strictly newer than the last packed version.
///
/// The last packed version lives in `.rackabel/state.toml` (DESIGN §4.3). With no
/// recorded pack (a first release) there is nothing to bump against, so we
/// skip-with-note. Equal-to or older-than the last pack is a failure (existing users
/// would not receive an update — DESIGN §2).
fn check_version_bumped(project: &Project, ext: &ResolvedExtension) -> Check {
    // A malformed state file should not crash validate; treat as "no record".
    let state = manifest::state::load(&project.root).unwrap_or_default();
    let Some(last_raw) = state.last_packed_version else {
        return Check::skip(
            "version-bumped",
            format!(
                "version {} — skipped (no previous pack recorded; this is the first release)",
                ext.version
            ),
        );
    };
    let Ok(last) = semver::Version::parse(&last_raw) else {
        return Check::skip(
            "version-bumped",
            format!(
                "version {} — skipped (recorded last-packed version `{last_raw}` is not valid semver)",
                ext.version
            ),
        );
    };
    if ext.version > last {
        Check::pass(
            "version-bumped",
            format!("version {} bumped (last packed {last})", ext.version),
        )
    } else {
        Check::fail(
            "version-bumped",
            format!(
                "version {} is not newer than the last packed {last} — bump \
                 [extension].version so existing users receive an update",
                ext.version
            ),
        )
    }
}

/// Rule 4: `CHANGELOG.md` has an entry for the current version.
///
/// A missing changelog file, or a file without a line mentioning the version, is a
/// failure (the §2 example: `✗ CHANGELOG.md has no entry for 1.2.0`).
fn check_changelog(project: &Project, ext: &ResolvedExtension) -> Check {
    let path = project.root.join("CHANGELOG.md");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Check::fail(
            "changelog",
            format!(
                "CHANGELOG.md not found — add one with an entry for {} (e.g. `## {}`)",
                ext.version, ext.version
            ),
        );
    };
    if changelog_mentions(&body, &ext.version) {
        Check::pass(
            "changelog",
            format!("CHANGELOG.md has an entry for {}", ext.version),
        )
    } else {
        Check::fail(
            "changelog",
            format!("CHANGELOG.md has no entry for {}", ext.version),
        )
    }
}

/// Whether the changelog text contains an entry for `version`. We look for the
/// version string not surrounded by version chars (so `1.2.0` does not match inside
/// `11.2.0` or `1.2.01`), which tolerates a leading `## `, `## v`, `[1.2.0]`, etc.
fn changelog_mentions(body: &str, version: &semver::Version) -> bool {
    let needle = version.to_string();
    let mut from = 0;
    while let Some(pos) = body[from..].find(&needle) {
        let abs = from + pos;
        let before_ok = body[..abs]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_ascii_digit() && c != '.');
        let after = abs + needle.len();
        // The match is a distinct version unless what follows extends it into a
        // longer version: another digit (1.2.01) or a dot-then-digit (1.2.0.4). A
        // trailing sentence period ("Released 1.2.0.") is fine.
        let rest = &body[after..];
        let mut rest_chars = rest.chars();
        let after_ok = match rest_chars.next() {
            None => true,
            Some(c) if c.is_ascii_digit() => false,
            Some('.') => !rest_chars.next().is_some_and(|c| c.is_ascii_digit()),
            Some(_) => true,
        };
        if before_ok && after_ok {
            return true;
        }
        from = abs + needle.len();
    }
    false
}

/// Rule 5: each declared native dep has a compiled `.node` matching the target.
///
/// SPEC C §3.8's full graph walk (`native_dep::audit`) is owned by `deploy` and is
/// still a stub. Validate must not depend on it, so it performs a *lightweight,
/// read-only* check here: for each declared `[extension.build].native_deps`, find
/// `node_modules/<dep>` and assert at least one `*.node` exists under it (not
/// descending into nested `node_modules`). Returns one check per dep. With no native
/// deps declared, returns a single skip line. (DEVIATIONS D-13.)
fn check_native_node(project: &Project, ext: &ResolvedExtension) -> Vec<Check> {
    if ext.native_deps.is_empty() {
        return vec![Check::skip(
            "native-node",
            "native dependencies — none declared",
        )];
    }
    let nm = project.root.join("node_modules");
    ext.native_deps
        .iter()
        .map(|dep| {
            let dep_dir = nm.join(dep);
            if !dep_dir.is_dir() {
                return Check::fail(
                    "native-node",
                    format!(
                        "native dep `{dep}` is not installed — run the project's install, \
                         then `rackabel deploy --fix` to build it"
                    ),
                );
            }
            if find_dot_node(&dep_dir) {
                Check::pass(
                    "native-node",
                    format!("native dep `{dep}` has a compiled .node"),
                )
            } else {
                Check::fail(
                    "native-node",
                    format!(
                        "native dep `{dep}` has no compiled .node binary — \
                         run `rackabel deploy --fix` to build it"
                    ),
                )
            }
        })
        .collect()
}

/// Whether a `*.node` file exists anywhere under `dir`, NOT descending into nested
/// `node_modules` (mirrors the ground-truth `hasNativeBinary`, SPEC B §3).
fn find_dot_node(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "node_modules") {
                continue;
            }
            if find_dot_node(&path) {
                return true;
            }
        } else if path.extension().is_some_and(|e| e == "node") {
            return true;
        }
    }
    false
}

/// Rule 6: stable-identifier drift (DESIGN §2 finalized in 0.5).
///
/// DESIGN §2 treats a removed/renamed *command id* as a compatibility break ("existing
/// setups may break — keep the old id or provide a migration"). But commands are
/// registered in *code* at runtime and never appear in the SDK manifest (SPEC A §2),
/// so there is no on-disk command-id list to diff (the command-id surface stays
/// deferred — DEVIATIONS D-12). What the manifest DOES carry is the extension `name`,
/// which is the identifier Live keys an extension's saved state on — so a `name` change
/// between the last shipped pack and now is the on-disk stable-identifier drift this
/// rule warns about (DEVIATIONS D-102).
///
/// We compare the CURRENTLY resolved manifest `name` (what the next build/pack would
/// write) against `state.last_packed_manifest` (the snapshot `pack` persisted). With no
/// prior pack there is nothing to diff (skip-with-note). A changed `name` is a
/// **warning** (the §2 example shape) — non-fatal unless `--strict`.
fn check_identifier_drift(project: &Project, ext: &ResolvedExtension) -> Check {
    let state = manifest::state::load(&project.root).unwrap_or_default();
    let Some(snapshot) = state.last_packed_manifest else {
        return Check::skip(
            "identifier-drift",
            "stable-identifier drift — skipped (no previous pack recorded to compare against)",
        );
    };
    if snapshot.name == ext.name {
        Check::pass(
            "identifier-drift",
            format!(
                "stable identifier `{}` unchanged since the last pack",
                ext.name
            ),
        )
    } else {
        // The §2 warning shape, adapted to the on-disk identifier (the extension name):
        // `name `Old` was renamed to `New` (present in 1.1.0) — existing setups may break`.
        Check::warn(
            "identifier-drift",
            format!(
                "name `{}` was renamed to `{}` (present in {}) — existing setups may break; \
                 keep the old name or provide a migration",
                snapshot.name, ext.name, snapshot.version
            ),
        )
    }
}

// --- reporting --------------------------------------------------------------

/// Print the checklist in the §2 shape and return the exit-coded result.
///
/// Failures (and, under `--strict`, warnings) make this an `RK4xxx` validation error
/// (exit 4). The individual lines are printed first (to stdout, the checklist), and
/// the framed summary error — when there is one — carries the count. Warnings-only is
/// success (exit 0).
fn report(checks: &[Check], strict: bool, ctx: &Ctx) -> CmdResult<()> {
    if ctx.json {
        return report_json(checks, strict);
    }

    for c in checks {
        match c.status {
            Status::Pass => ui::emit(ui::Symbol::Good, &c.message, ctx),
            Status::Fail => ui::emit(ui::Symbol::Bad, &c.message, ctx),
            Status::Skip => ui::emit(ui::Symbol::Warn, &c.message, ctx),
            // Warnings use the literal `warning:` prefix from the §2 example.
            Status::Warn => println!("warning: {}", c.message),
        }
    }

    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warnings = checks.iter().filter(|c| c.status == Status::Warn).count();
    let strict_fails = if strict { warnings } else { 0 };
    let effective_failed = failed + strict_fails;

    // The tail summary line (§2 example: `1 failed, 1 warning`). Printed to stdout so
    // the checklist reads as one block even when validation ultimately fails.
    println!("{}", summary_line(failed, warnings));

    if effective_failed > 0 {
        // The framed validation error carries the right code + count; the checklist
        // above already showed *which* rules failed.
        Err(validation_error(checks, failed, warnings, strict))
    } else {
        Ok(())
    }
}

/// `--json`: emit the checks as an array plus a summary object, and exit-code the same.
fn report_json(checks: &[Check], strict: bool) -> CmdResult<()> {
    let items: Vec<_> = checks
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "status": status_str(c.status),
                "message": c.message,
            })
        })
        .collect();
    let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
    let warnings = checks.iter().filter(|c| c.status == Status::Warn).count();
    let strict_fails = if strict { warnings } else { 0 };
    let effective_failed = failed + strict_fails;

    let out = json!({
        "ok": effective_failed == 0,
        "failed": failed,
        "warnings": warnings,
        "strict": strict,
        "checks": items,
    });
    println!("{}", serde_json::to_string_pretty(&out).expect("json"));

    if effective_failed > 0 {
        // The checklist object above is the authoritative `--json` output (its `ok:false`
        // + per-check failures carry everything a consumer needs). Mark the error
        // `json_handled` so `main` does not print a second JSON error object on stdout.
        Err(validation_error(checks, failed, warnings, strict).json_handled())
    } else {
        Ok(())
    }
}

fn status_str(s: Status) -> &'static str {
    match s {
        Status::Pass => "pass",
        Status::Fail => "fail",
        Status::Warn => "warning",
        Status::Skip => "skipped",
    }
}

/// The `N failed, M warning` tail (§2 example). Pluralizes and omits a zero count for
/// legibility, but always prints something so success is explicit.
fn summary_line(failed: usize, warnings: usize) -> String {
    let plural = |n: usize, w: &str| {
        if n == 1 {
            w.to_string()
        } else {
            format!("{w}s")
        }
    };
    match (failed, warnings) {
        (0, 0) => "all checks passed".to_string(),
        (f, 0) => format!("{f} {}", plural(f, "failure")),
        (0, w) => format!("{w} {}", plural(w, "warning")),
        (f, w) => format!("{f} {}, {w} {}", plural(f, "failure"), plural(w, "warning")),
    }
}

/// Assemble the framed validation error. The `code` matches the single failing rule
/// when there is exactly one real failure, so `rackabel explain <code>` is precise;
/// otherwise it falls back to the generic incomplete-manifest code.
fn validation_error(checks: &[Check], failed: usize, warnings: usize, strict: bool) -> RkError {
    let fails: Vec<&Check> = checks.iter().filter(|c| c.status == Status::Fail).collect();
    let code = if fails.len() == 1 {
        code_for(fails[0].id)
    } else if failed == 0 && strict {
        // Only strict-promoted warnings failed.
        ErrorCode::IdentifierDrift
    } else {
        ErrorCode::ManifestIncomplete
    };

    let help = if strict && warnings > 0 && failed == 0 {
        "address the warning above (or drop --strict), then rerun `rackabel validate`"
    } else {
        "fix the rule(s) marked ✗ above, then rerun `rackabel validate`"
    };

    RkError::new(
        code,
        ExitClass::Validation,
        format!("validation failed — {}", summary_line(failed, warnings)),
        help,
    )
}

/// Map a check id to the error code whose `explain` entry describes that rule.
fn code_for(id: &str) -> ErrorCode {
    match id {
        "manifest-complete" => ErrorCode::ManifestIncomplete,
        "api-version" => ErrorCode::ApiVersionTooHigh,
        "version-bumped" => ErrorCode::VersionNotBumped,
        "native-node" => ErrorCode::NativeDepNotCompiled,
        "identifier-drift" => ErrorCode::IdentifierDrift,
        // changelog has no dedicated code; the incomplete-manifest code is the closest
        // "your project isn't ship-ready" bucket.
        _ => ErrorCode::ManifestIncomplete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ext(
        version: (u64, u64, u64),
        api: (u64, u64, u64),
        native: Vec<String>,
    ) -> ResolvedExtension {
        ResolvedExtension {
            name: "Cool".into(),
            author: "Jane".into(),
            version: semver::Version::new(version.0, version.1, version.2),
            entry: PathBuf::from("src/extension.ts"),
            minimum_api_version: semver::Version::new(api.0, api.1, api.2),
            extra_dist_files: vec![],
            native_deps: native,
            pack_targets: vec![],
            inferred: vec![],
        }
    }

    fn project_at(root: &Path) -> Project {
        Project {
            root: root.to_path_buf(),
            raw: manifest::ManifestRaw::default(),
        }
    }

    // --- changelog detection --------------------------------------------

    #[test]
    fn changelog_matches_heading_forms() {
        let v = semver::Version::new(1, 2, 0);
        assert!(changelog_mentions("## 1.2.0\n- thing", &v));
        assert!(changelog_mentions("## v1.2.0 (2026-06-06)", &v));
        assert!(changelog_mentions("## [1.2.0]\n", &v));
        assert!(changelog_mentions("Released 1.2.0.", &v));
    }

    #[test]
    fn changelog_does_not_match_substring_version() {
        let v = semver::Version::new(1, 2, 0);
        // 1.2.0 must not match inside 11.2.0 or 1.2.01.
        assert!(!changelog_mentions("## 11.2.0\n", &v));
        assert!(!changelog_mentions("## 1.2.01\n", &v));
        assert!(!changelog_mentions("nothing relevant here", &v));
    }

    // --- manifest completeness ------------------------------------------

    #[test]
    fn manifest_complete_passes_when_all_present() {
        let tmp = tempfile::tempdir().unwrap();
        let p = project_at(tmp.path());
        let c = check_manifest_complete(&p, &ext((1, 0, 0), (1, 0, 0), vec![]));
        assert_eq!(c.status, Status::Pass);
    }

    #[test]
    fn manifest_incomplete_fails_on_empty_author() {
        let tmp = tempfile::tempdir().unwrap();
        let p = project_at(tmp.path());
        let mut e = ext((1, 0, 0), (1, 0, 0), vec![]);
        e.author = String::new();
        let c = check_manifest_complete(&p, &e);
        assert_eq!(c.status, Status::Fail);
        assert!(c.message.contains("author"));
    }

    // --- version bump ----------------------------------------------------

    #[test]
    fn version_bump_skips_with_no_prior_pack() {
        let tmp = tempfile::tempdir().unwrap();
        let p = project_at(tmp.path());
        let c = check_version_bumped(&p, &ext((1, 0, 0), (1, 0, 0), vec![]));
        assert_eq!(c.status, Status::Skip);
    }

    #[test]
    fn version_bump_fails_when_equal_to_last_packed() {
        let tmp = tempfile::tempdir().unwrap();
        manifest::state::save(
            tmp.path(),
            &manifest::state::State {
                last_packed_version: Some("1.2.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let p = project_at(tmp.path());
        let c = check_version_bumped(&p, &ext((1, 2, 0), (1, 0, 0), vec![]));
        assert_eq!(c.status, Status::Fail);
    }

    #[test]
    fn version_bump_fails_when_older_than_last_packed() {
        let tmp = tempfile::tempdir().unwrap();
        manifest::state::save(
            tmp.path(),
            &manifest::state::State {
                last_packed_version: Some("2.0.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let p = project_at(tmp.path());
        let c = check_version_bumped(&p, &ext((1, 9, 0), (1, 0, 0), vec![]));
        assert_eq!(c.status, Status::Fail);
    }

    #[test]
    fn version_bump_passes_when_newer() {
        let tmp = tempfile::tempdir().unwrap();
        manifest::state::save(
            tmp.path(),
            &manifest::state::State {
                last_packed_version: Some("1.2.0".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let p = project_at(tmp.path());
        let c = check_version_bumped(&p, &ext((1, 3, 0), (1, 0, 0), vec![]));
        assert_eq!(c.status, Status::Pass);
    }

    // --- changelog rule --------------------------------------------------

    #[test]
    fn changelog_rule_fails_when_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = project_at(tmp.path());
        let c = check_changelog(&p, &ext((1, 2, 0), (1, 0, 0), vec![]));
        assert_eq!(c.status, Status::Fail);
        assert!(c.message.contains("not found"));
    }

    #[test]
    fn changelog_rule_fails_when_no_entry() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CHANGELOG.md"), "## 1.1.0\n- old\n").unwrap();
        let p = project_at(tmp.path());
        let c = check_changelog(&p, &ext((1, 2, 0), (1, 0, 0), vec![]));
        assert_eq!(c.status, Status::Fail);
        assert!(c.message.contains("no entry for 1.2.0"));
    }

    #[test]
    fn changelog_rule_passes_with_entry() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("CHANGELOG.md"), "## 1.2.0\n- new\n").unwrap();
        let p = project_at(tmp.path());
        let c = check_changelog(&p, &ext((1, 2, 0), (1, 0, 0), vec![]));
        assert_eq!(c.status, Status::Pass);
    }

    // --- native .node ----------------------------------------------------

    #[test]
    fn native_node_skips_when_none_declared() {
        let tmp = tempfile::tempdir().unwrap();
        let p = project_at(tmp.path());
        let cs = check_native_node(&p, &ext((1, 0, 0), (1, 0, 0), vec![]));
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].status, Status::Skip);
    }

    #[test]
    fn native_node_fails_when_not_installed() {
        let tmp = tempfile::tempdir().unwrap();
        let p = project_at(tmp.path());
        let cs = check_native_node(&p, &ext((1, 0, 0), (1, 0, 0), vec!["abletonlink".into()]));
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].status, Status::Fail);
        assert!(cs[0].message.contains("not installed"));
    }

    #[test]
    fn native_node_fails_without_dot_node_but_passes_with() {
        let tmp = tempfile::tempdir().unwrap();
        let dep = tmp.path().join("node_modules/abletonlink/build/Release");
        std::fs::create_dir_all(&dep).unwrap();
        let p = project_at(tmp.path());
        // No .node yet -> fail.
        let cs = check_native_node(&p, &ext((1, 0, 0), (1, 0, 0), vec!["abletonlink".into()]));
        assert_eq!(cs[0].status, Status::Fail);
        assert!(cs[0].message.contains("no compiled .node"));
        // Drop a .node -> pass.
        std::fs::write(dep.join("abletonlink.node"), b"\0").unwrap();
        let cs2 = check_native_node(&p, &ext((1, 0, 0), (1, 0, 0), vec!["abletonlink".into()]));
        assert_eq!(cs2[0].status, Status::Pass);
    }

    #[test]
    fn find_dot_node_skips_nested_node_modules() {
        let tmp = tempfile::tempdir().unwrap();
        // A .node only under a NESTED node_modules must not count.
        let nested = tmp.path().join("node_modules/sub");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("x.node"), b"\0").unwrap();
        assert!(!find_dot_node(tmp.path()));
    }

    // --- stable-identifier drift ----------------------------------------

    fn snapshot(name: &str, version: &str) -> manifest::state::PackedManifestSnapshot {
        manifest::state::PackedManifestSnapshot {
            name: name.into(),
            author: "Jane".into(),
            entry: "dist/extension.js".into(),
            version: version.into(),
            minimum_api_version: "1.0.0".into(),
        }
    }

    #[test]
    fn drift_skips_with_no_prior_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let p = project_at(tmp.path());
        let mut e = ext((1, 1, 0), (1, 0, 0), vec![]);
        e.name = "Clip Renamer".into();
        let c = check_identifier_drift(&p, &e);
        assert_eq!(c.status, Status::Skip);
    }

    #[test]
    fn drift_passes_when_name_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        manifest::state::save(
            tmp.path(),
            &manifest::state::State {
                last_packed_manifest: Some(snapshot("Clip Renamer", "1.0.0")),
                ..Default::default()
            },
        )
        .unwrap();
        let p = project_at(tmp.path());
        let mut e = ext((1, 1, 0), (1, 0, 0), vec![]);
        e.name = "Clip Renamer".into();
        let c = check_identifier_drift(&p, &e);
        assert_eq!(c.status, Status::Pass);
    }

    #[test]
    fn drift_warns_when_name_changed_with_section2_shape() {
        let tmp = tempfile::tempdir().unwrap();
        manifest::state::save(
            tmp.path(),
            &manifest::state::State {
                last_packed_manifest: Some(snapshot("Clip Renamer", "1.1.0")),
                ..Default::default()
            },
        )
        .unwrap();
        let p = project_at(tmp.path());
        let mut e = ext((1, 2, 0), (1, 0, 0), vec![]);
        e.name = "Clip Tool".into();
        let c = check_identifier_drift(&p, &e);
        assert_eq!(c.status, Status::Warn);
        // The §2 warning shape: names the old + new identifier and the version it
        // shipped in, and ends with the "existing setups may break" clause.
        assert!(c.message.contains("`Clip Renamer`"));
        assert!(c.message.contains("`Clip Tool`"));
        assert!(c.message.contains("present in 1.1.0"));
        assert!(c.message.contains("existing setups may break"));
    }

    /// A rename warning is non-fatal by default (exit 0) but `--strict` promotes it to a
    /// validation failure (exit 4) — the §2 "keep the old id or migrate" gate for CI.
    #[test]
    fn drift_warning_is_strict_fatal_only() {
        let checks = vec![
            Check::pass("manifest-complete", "ok"),
            Check::warn("identifier-drift", "name `A` was renamed to `B`"),
        ];
        // Non-strict: a warning is not a failure.
        let failed = checks.iter().filter(|c| c.status == Status::Fail).count();
        assert_eq!(failed, 0);
        // Strict: the warning is promoted, and the chosen code is IdentifierDrift.
        let e = validation_error(&checks, 0, 1, true);
        assert_eq!(e.code, ErrorCode::IdentifierDrift);
        assert_eq!(e.class, ExitClass::Validation);
    }

    // --- summary line ----------------------------------------------------

    #[test]
    fn summary_line_shapes() {
        assert_eq!(summary_line(0, 0), "all checks passed");
        assert_eq!(summary_line(1, 0), "1 failure");
        assert_eq!(summary_line(2, 0), "2 failures");
        assert_eq!(summary_line(0, 1), "1 warning");
        assert_eq!(summary_line(1, 1), "1 failure, 1 warning");
    }

    // --- error code selection -------------------------------------------

    #[test]
    fn single_failure_picks_specific_code() {
        let checks = vec![
            Check::pass("manifest-complete", "ok"),
            Check::fail("version-bumped", "stale"),
            Check::skip("api-version", "skip"),
        ];
        let e = validation_error(&checks, 1, 0, false);
        assert_eq!(e.code, ErrorCode::VersionNotBumped);
        assert_eq!(e.class, ExitClass::Validation);
    }

    #[test]
    fn multiple_failures_use_generic_code() {
        let checks = vec![
            Check::fail("manifest-complete", "x"),
            Check::fail("changelog", "y"),
        ];
        let e = validation_error(&checks, 2, 0, false);
        assert_eq!(e.code, ErrorCode::ManifestIncomplete);
    }
}
