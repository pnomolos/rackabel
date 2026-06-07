//! The `[hooks]` + `[hooks.timeouts]` manifest surfaces (DESIGN §5.3, §5.5).
//!
//! FOUNDATION-OWNED, FROZEN. One [`HooksTable`] type models BOTH:
//!   - the PROJECT-local `[hooks]` table in `rackabel.toml` (§5.5 — no manifest, no enable
//!     step, implicit trust; paths are relative to the PROJECT ROOT); and
//!   - the `[hooks]` table in a third-party `rackabel-plugin.toml` (§5.3 — gated by the
//!     `enabled` flag, versioned by `hook_api`; paths are relative to the PLUGIN ROOT).
//!
//! A hook entry is a single command path (relative to the owning root). The
//! `[hooks.timeouts]` table overrides the per-hook wall-clock timeout in MILLISECONDS
//! (§5.3; default [`crate::hooks::DEFAULT_TIMEOUT_MS`]). The mapping is keyed by the
//! frozen [`HookKind`] strings; an unknown key is preserved-but-ignored (forward
//! compatibility) the same way the 0.4 inert plugin manifest tolerated extra tables.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{DEFAULT_TIMEOUT_MS, HookKind};

/// A parsed `[hooks]` table plus its `[hooks.timeouts]` overrides (§5.3/§5.5).
///
/// TOML shape:
/// ```toml
/// [hooks]
/// post_build = ".rackabel/hooks/post-build"   # a command path, relative to the root
/// pre_deploy = "bin/pre-deploy"
///
/// [hooks.timeouts]
/// post_build = 120000                          # per-hook timeout in MILLISECONDS
/// ```
///
/// We deserialize `[hooks]` entries as raw `toml::Value` (not bare strings) so a future
/// table-form entry (`post_build = { command = "…", … }`) still PARSES on an older binary —
/// the same forward-compat leniency the 0.4 inert manifest used — while [`commands`]
/// extracts the string form this milestone supports.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct HooksTable {
    /// `<hook_name> = <command>` entries. Stored as `toml::Value` for forward-compat;
    /// `timeouts` is split out as its own field so it is NOT mistaken for a hook entry.
    #[serde(flatten)]
    entries: BTreeMap<String, toml::Value>,
}

impl HooksTable {
    /// The command path declared for `kind`, if any (the string form). A non-string entry
    /// (a forward table form) yields `None` for THIS milestone — the engine then treats the
    /// hook as undeclared rather than guessing its command.
    pub fn command(&self, kind: HookKind) -> Option<&str> {
        self.entries.get(kind.as_str()).and_then(|v| v.as_str())
    }

    /// Every declared `(kind, command)` pair this build understands, in [`HookKind::ALL`]
    /// order (deterministic for discovery + transcripts). The `timeouts` sub-table and any
    /// unknown/forward keys are excluded.
    pub fn commands(&self) -> Vec<(HookKind, &str)> {
        HookKind::ALL
            .iter()
            .filter_map(|&k| self.command(k).map(|c| (k, c)))
            .collect()
    }

    /// The hook kinds this table declares (the names only) — the inert list the lockfile
    /// records for a plugin manifest. Sorted, deterministic.
    pub fn declared_kinds(&self) -> Vec<HookKind> {
        HookKind::ALL
            .iter()
            .copied()
            .filter(|&k| self.command(k).is_some())
            .collect()
    }

    /// The resolved wall-clock timeout for `kind` in MILLISECONDS (§5.3): the
    /// `[hooks.timeouts]` override if present and positive, else
    /// [`crate::hooks::DEFAULT_TIMEOUT_MS`].
    pub fn timeout_ms(&self, kind: HookKind) -> u64 {
        self.timeouts()
            .get(kind.as_str())
            .and_then(|v| v.as_integer())
            .filter(|&n| n > 0)
            .map(|n| n as u64)
            .unwrap_or(DEFAULT_TIMEOUT_MS)
    }

    /// The raw `[hooks.timeouts]` sub-table as a key→value map (empty if absent).
    fn timeouts(&self) -> BTreeMap<String, toml::Value> {
        self.entries
            .get("timeouts")
            .and_then(|v| v.as_table())
            .map(|t| t.clone().into_iter().collect())
            .unwrap_or_default()
    }

    /// Whether the table declares ANY hook this build understands (used to decide if a
    /// plugin "carried hooks" for the lockfile).
    pub fn is_empty(&self) -> bool {
        self.declared_kinds().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize, Serialize)]
    struct Wrapper {
        hooks: HooksTable,
    }

    fn parse(src: &str) -> HooksTable {
        toml::from_str::<Wrapper>(src).unwrap().hooks
    }

    #[test]
    fn parses_string_commands() {
        let t = parse(
            "[hooks]\n\
             post_build = \".rackabel/hooks/post-build\"\n\
             pre_deploy = \"bin/pre-deploy\"\n",
        );
        assert_eq!(
            t.command(HookKind::PostBuild),
            Some(".rackabel/hooks/post-build")
        );
        assert_eq!(t.command(HookKind::PreDeploy), Some("bin/pre-deploy"));
        assert_eq!(t.command(HookKind::OnReload), None);
        assert_eq!(
            t.declared_kinds(),
            vec![HookKind::PostBuild, HookKind::PreDeploy]
        );
    }

    #[test]
    fn commands_are_in_hookkind_order() {
        // Declared out of canonical order; commands() returns HookKind::ALL order.
        let t = parse(
            "[hooks]\n\
             on_reload = \"r\"\n\
             post_build = \"p\"\n\
             doctor_check = \"d\"\n",
        );
        let kinds: Vec<HookKind> = t.commands().into_iter().map(|(k, _)| k).collect();
        assert_eq!(
            kinds,
            vec![
                HookKind::PostBuild,
                HookKind::OnReload,
                HookKind::DoctorCheck
            ]
        );
    }

    #[test]
    fn timeouts_default_and_override() {
        let t = parse(
            "[hooks]\n\
             post_build = \"p\"\n\
             pre_deploy = \"d\"\n\
             [hooks.timeouts]\n\
             post_build = 120000\n",
        );
        assert_eq!(t.timeout_ms(HookKind::PostBuild), 120_000);
        // No override ⇒ the 30s default in ms.
        assert_eq!(t.timeout_ms(HookKind::PreDeploy), DEFAULT_TIMEOUT_MS);
        assert_eq!(t.timeout_ms(HookKind::PreDeploy), 30_000);
        // timeouts is NOT itself a hook entry.
        assert!(HookKind::from_str("timeouts").is_none());
        assert_eq!(t.declared_kinds().len(), 2);
    }

    #[test]
    fn non_positive_timeout_falls_back_to_default() {
        let t = parse(
            "[hooks]\n\
             post_build = \"p\"\n\
             [hooks.timeouts]\n\
             post_build = 0\n",
        );
        assert_eq!(t.timeout_ms(HookKind::PostBuild), DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn forward_table_form_entry_is_not_a_string_command() {
        // A future `post_build = { command = "x" }` parses but yields no string command
        // on this build (treated as undeclared rather than guessed).
        let t = parse(
            "[hooks]\n\
             post_build = { command = \"x\", retries = 3 }\n",
        );
        assert_eq!(t.command(HookKind::PostBuild), None);
        assert!(t.is_empty());
    }

    #[test]
    fn unknown_hook_key_is_ignored() {
        let t = parse(
            "[hooks]\n\
             post_build = \"p\"\n\
             some_future_hook = \"f\"\n",
        );
        // Only the known kind is surfaced.
        assert_eq!(t.declared_kinds(), vec![HookKind::PostBuild]);
    }

    #[test]
    fn empty_table_is_empty() {
        let t = parse("[hooks]\n[hooks.timeouts]\npost_build = 1000\n");
        assert!(t.is_empty());
        assert!(t.declared_kinds().is_empty());
    }

    #[test]
    fn round_trips_through_toml() {
        let t = parse("[hooks]\npost_build = \"p\"\n[hooks.timeouts]\npost_build = 5000\n");
        let ser = toml::to_string(&Wrapper { hooks: t.clone() }).unwrap();
        let back = toml::from_str::<Wrapper>(&ser).unwrap().hooks;
        assert_eq!(back, t);
        assert_eq!(back.timeout_ms(HookKind::PostBuild), 5000);
    }
}
