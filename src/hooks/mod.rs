//! Lifecycle hooks — the tier-3 extensibility surface (DESIGN §5.3, §5.5, §5.7).
//!
//! FOUNDATION-OWNED (milestone 0.5). This module freezes the **contract** the 0.5
//! feature agents build the engine body on:
//!   - [`HookKind`] — the five named lifecycle hooks (`post_build`/`pre_deploy`/
//!     `on_reload`/`doctor_check`/`new_template`);
//!   - the per-hook stdin payload structs ([`payload`]) serializing EXACTLY the §5.3
//!     field names/types, with `skip_serializing_if` honoring the omitted-not-empty
//!     rule (a field absent in a context is dropped, never sent as `""`);
//!   - the per-hook outcome enums ([`outcome`]) — informational logged+skipped, the
//!     `pre_deploy` veto, the `doctor_check` line with its a-d precedence, and the
//!     `new_template` enumerate choice;
//!   - the [`HOOK_API`] integer (the tier-3 contract version, separate from the tier-2
//!     [`crate::plugin::env_contract::RACKABEL_PLUGIN_API`]);
//!   - hook discovery resolution ([`discovery`]) — the ordered list of sources for a
//!     given kind (project-local `[hooks]` first, then every ENABLED installed plugin);
//!   - the frozen hook-engine signature ([`engine::run_hook`]) with a compiling stub.
//!
//! The engine BODY (spawning the subprocess, the timeout/SIGTERM/SIGKILL machinery, the
//! stdin framing, parsing stdout) is a feature agent's; the foundation lands the types,
//! the signatures, and a stub that returns a clear framed error so the tree builds.

pub mod discovery;
pub mod engine;
pub mod lifecycle;
pub mod manifest;
pub mod outcome;
pub mod payload;
pub mod run;

use serde::{Deserialize, Serialize};

/// The tier-3 **hook** contract version (DESIGN §5.2/§5.3). This is a SEPARATE integer
/// from the tier-2 [`crate::plugin::env_contract::RACKABEL_PLUGIN_API`] — different
/// surfaces (a runtime-only env read vs a manifest-declared, codemoddable contract) that
/// evolve independently and share no number. It is exposed to a hook subprocess as the
/// `RACKABEL_HOOK_API` env var, and a plugin declares the version it targets via the
/// `hook_api` key in `rackabel-plugin.toml`.
///
/// Currently 1, and no migrations exist: a `rackabel-plugin.toml` declaring `hook_api = 1`
/// needs nothing migrated; a declared `hook_api > 1` is unsupported by this build.
pub const HOOK_API: u32 = 1;

/// The env var name carrying [`HOOK_API`] to a hook subprocess (DESIGN §5.2). Set
/// IN ADDITION to the full §5.2 env contract a PATH subcommand also gets, so a hook can
/// gate optional newer payload fields on it the same way tier-2 uses `RACKABEL_PLUGIN_API`.
pub const RACKABEL_HOOK_API_ENV: &str = "RACKABEL_HOOK_API";

/// The default per-hook wall-clock timeout (DESIGN §5.3): 30 seconds. Overridable
/// per hook via the `[hooks.timeouts]` table (in milliseconds) in `rackabel-plugin.toml`
/// or next to a project-local `[hooks]`. On timeout rackabel sends SIGTERM, then SIGKILL
/// after a [`TIMEOUT_GRACE`] grace, and treats the hook exactly like a nonzero exit.
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// The grace period (DESIGN §5.3) between SIGTERM and SIGKILL when a hook exceeds its
/// timeout: 5 seconds.
pub const TIMEOUT_GRACE_MS: u64 = 5_000;

/// The five named lifecycle hooks (DESIGN §5.3). The string form is the EXACT key a
/// plugin uses under `[hooks]` in `rackabel-plugin.toml` (and a project uses under
/// `[hooks]` in its own `rackabel.toml`); it is frozen as part of the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookKind {
    /// Invoked after a successful build; informational only (never aborts the build).
    PostBuild,
    /// Invoked before a deploy; the ONLY hook allowed to VETO (nonzero ⇒ abort the deploy).
    PreDeploy,
    /// Invoked after a dev-loop reload; informational only.
    OnReload,
    /// Contributes a `doctor` row via a one-JSON-line stdout contract (precedence a-d).
    DoctorCheck,
    /// Enumerate-only, pre-wizard: contributes a CHOICE to the `new` wizard's template list.
    NewTemplate,
}

impl HookKind {
    /// The contract key string (the `[hooks]` table key). Frozen.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PostBuild => "post_build",
            Self::PreDeploy => "pre_deploy",
            Self::OnReload => "on_reload",
            Self::DoctorCheck => "doctor_check",
            Self::NewTemplate => "new_template",
        }
    }

    /// Parse a `[hooks]` key back to a [`HookKind`]. `None` for an unknown key — a
    /// forward-compatible manifest may declare a hook this build doesn't know; the caller
    /// decides whether to ignore it (discovery) or surface it.
    pub fn from_str(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }

    /// Every hook kind, in a stable order (used to enumerate the discovery surface and
    /// for deterministic transcripts/tests).
    pub const ALL: &'static [HookKind] = &[
        Self::PostBuild,
        Self::PreDeploy,
        Self::OnReload,
        Self::DoctorCheck,
        Self::NewTemplate,
    ];

    /// Whether this hook is allowed to VETO its phase (DESIGN §5.3). Only `pre_deploy`
    /// can: every other hook is informational (nonzero/timeout ⇒ logged + skipped). The
    /// engine uses this to decide whether a failure aborts or is swallowed; freezing it
    /// here keeps the one-veto rule in a single place.
    pub fn is_veto(self) -> bool {
        matches!(self, Self::PreDeploy)
    }
}

impl std::fmt::Display for HookKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a hook came from (DESIGN §5.5/§5.7). Drives trust + the engine's framing: a
/// project-local hook is the user's OWN code (implicit trust, no enable step); a plugin
/// hook is installed third-party code gated by the `enabled` flag (enabling is consent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookSource {
    /// A `[hooks]` entry in the project's own `rackabel.toml` (DESIGN §5.5). No manifest,
    /// no enable step — implicit trust. Carries the project root for relative-path
    /// resolution (paths are relative to the project root).
    Project { project_root: std::path::PathBuf },
    /// A hook declared by an ENABLED installed plugin's `rackabel-plugin.toml` (§5.3).
    /// Carries the plugin name for framing (`<name>`'s `pre_deploy` aborted …) and the
    /// plugin store dir so a manifest-relative command resolves.
    Plugin {
        name: String,
        store_dir: std::path::PathBuf,
    },
}

impl HookSource {
    /// A short label for error frames / transcripts.
    pub fn label(&self) -> String {
        match self {
            Self::Project { .. } => "project".to_string(),
            Self::Plugin { name, .. } => format!("plugin {name}"),
        }
    }

    /// The directory a hook's (possibly relative) command is resolved against.
    pub fn base_dir(&self) -> &std::path::Path {
        match self {
            Self::Project { project_root } => project_root,
            Self::Plugin { store_dir, .. } => store_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hookkind_strings_are_frozen_contract_keys() {
        assert_eq!(HookKind::PostBuild.as_str(), "post_build");
        assert_eq!(HookKind::PreDeploy.as_str(), "pre_deploy");
        assert_eq!(HookKind::OnReload.as_str(), "on_reload");
        assert_eq!(HookKind::DoctorCheck.as_str(), "doctor_check");
        assert_eq!(HookKind::NewTemplate.as_str(), "new_template");
    }

    #[test]
    fn hookkind_roundtrips() {
        for &k in HookKind::ALL {
            assert_eq!(HookKind::from_str(k.as_str()), Some(k));
        }
        assert_eq!(HookKind::from_str("unknown"), None);
    }

    #[test]
    fn only_pre_deploy_can_veto() {
        for &k in HookKind::ALL {
            assert_eq!(k.is_veto(), k == HookKind::PreDeploy, "veto rule for {k}");
        }
    }

    #[test]
    fn serde_uses_snake_case() {
        let s = serde_json::to_string(&HookKind::DoctorCheck).unwrap();
        assert_eq!(s, "\"doctor_check\"");
        let k: HookKind = serde_json::from_str("\"new_template\"").unwrap();
        assert_eq!(k, HookKind::NewTemplate);
    }

    #[test]
    fn hook_api_is_one() {
        assert_eq!(HOOK_API, 1);
        // It is a SEPARATE integer from the tier-2 contract.
        assert_eq!(
            HOOK_API,
            crate::plugin::env_contract::RACKABEL_PLUGIN_API,
            "both happen to be 1 today, but they are independent constants"
        );
    }
}
