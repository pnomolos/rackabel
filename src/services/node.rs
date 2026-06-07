//! Node runtime resolution (DESIGN §0, §3.1; SPEC C §3.6).
//!
//! For the dev host, Live's *bundled* node is mandatory (native-ABI match). For
//! `build`, rackabel *prefers* the bundled node but falls back to a PATH node
//! (`which node`) so a musician can scaffold before installing Live (DESIGN §0).
//! The runtime floor doctor enforces is the SDK/CLI `engines` value (>=22.11.0) —
//! below it the remedy is "upgrade Live", never "install Node".

use std::path::PathBuf;
use std::process::Command;

use crate::context::Ctx;
use crate::services::live::LiveInstall;

/// Where a resolved node came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeSource {
    LiveBundled,
    Path,
}

/// A resolved node runtime.
#[derive(Debug, Clone)]
pub struct NodeRuntime {
    pub bin: PathBuf,
    pub version: semver::Version,
    pub source: NodeSource,
}

/// Resolve a node runtime, preferring Live's bundled node (ABI match) over a PATH
/// node. The `--eh-node`/`$ABLETON_EH_NODE` override (in `ctx`) wins over both, so
/// tests can point at a stub. `None` if neither is usable.
pub fn resolve(live: Option<&LiveInstall>, ctx: &Ctx) -> Option<NodeRuntime> {
    // 1. Explicit override (testability seam + power-user flag).
    if let Some(bin) = &ctx.ableton_eh_node
        && let Some(version) = probe_version(bin)
    {
        return Some(NodeRuntime {
            bin: bin.clone(),
            version,
            source: NodeSource::LiveBundled,
        });
    }
    // 2. Live's bundled node.
    if let Some(install) = live
        && let Some(bin) = &install.bundled_node
        && let Some(version) = probe_version(bin)
    {
        return Some(NodeRuntime {
            bin: bin.clone(),
            version,
            source: NodeSource::LiveBundled,
        });
    }
    // 3. PATH node fallback.
    if let Ok(bin) = which::which("node")
        && let Some(version) = probe_version(&bin)
    {
        return Some(NodeRuntime {
            bin,
            version,
            source: NodeSource::Path,
        });
    }
    None
}

/// For `new`'s auto-build gating: any usable node (override > bundled-via-detected
/// Live > PATH), else `None` (skip build, don't error — DESIGN §0/§6.2).
pub fn any_usable(ctx: &Ctx) -> Option<NodeRuntime> {
    let live = crate::services::live::detect(ctx);
    let primary = live.iter().find(|i| i.bundled_node.is_some());
    resolve(primary, ctx)
}

/// Whether a runtime meets the floor (default `>=22.11.0`, DESIGN §4.2).
pub fn meets_runtime_floor(rt: &NodeRuntime, floor: &semver::VersionReq) -> bool {
    floor.matches(&rt.version)
}

/// The default runtime floor (`>=22.11.0`).
pub fn default_runtime_floor() -> semver::VersionReq {
    semver::VersionReq::parse(">=22.11.0").expect("static req parses")
}

/// Run `<bin> --version`, parse the `vX.Y.Z` output to semver.
fn probe_version(bin: &std::path::Path) -> Option<semver::Version> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse_node_version(text.trim())
}

/// Parse a node `--version` string like `v24.14.1` to semver.
fn parse_node_version(s: &str) -> Option<semver::Version> {
    let trimmed = s.trim().trim_start_matches('v');
    semver::Version::parse(trimmed).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_node_version() {
        assert_eq!(
            parse_node_version("v24.14.1"),
            Some(semver::Version::new(24, 14, 1))
        );
        assert_eq!(
            parse_node_version("22.11.0"),
            Some(semver::Version::new(22, 11, 0))
        );
        assert_eq!(parse_node_version("garbage"), None);
    }

    #[test]
    fn floor_check() {
        let floor = default_runtime_floor();
        let ok = NodeRuntime {
            bin: PathBuf::from("/x"),
            version: semver::Version::new(24, 14, 1),
            source: NodeSource::LiveBundled,
        };
        let below = NodeRuntime {
            bin: PathBuf::from("/x"),
            version: semver::Version::new(20, 0, 0),
            source: NodeSource::Path,
        };
        assert!(meets_runtime_floor(&ok, &floor));
        assert!(!meets_runtime_floor(&below, &floor));
    }
}
