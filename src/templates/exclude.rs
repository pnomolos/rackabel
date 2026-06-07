//! The `[merge].exclude` glob set (DESIGN §5.5).
//!
//! Files matched here are NEVER text-merged by `new --update` and are copied VERBATIM
//! (no placeholder substitution) by the initial render: they are binary/generated and a
//! marker-based 3-way merge can't reconcile their bytes. The author's declared
//! `[merge].exclude` globs are joined with an ALWAYS-EXCLUDED built-in set (vendored
//! tarballs + common binary/lock artifacts) so a template that forgets to list its
//! vendored SDK still can't get its tarballs mangled.

use globset::{Glob, GlobSet, GlobSetBuilder};

/// Glob patterns that are ALWAYS excluded regardless of the template's declaration —
/// vendored SDK/CLI tarballs and common binary/generated artifacts (§5.5).
pub const ALWAYS_EXCLUDED: &[&str] = &[
    "**/*.tgz",
    "**/*.tar.gz",
    "**/*.tar",
    "**/*.zip",
    "**/*.ablx",
    "**/*.amxd",
    "**/*.node",
    "**/*.wasm",
    "**/*.png",
    "**/*.jpg",
    "**/*.jpeg",
    "**/*.gif",
    "**/*.ico",
    "**/vendor/**",
    "**/node_modules/**",
    // Lockfiles: regenerated, not author-edited; a 3-way merge on them is noise.
    "**/package-lock.json",
    "**/pnpm-lock.yaml",
    "**/yarn.lock",
];

/// A compiled set of exclude globs (author-declared ∪ [`ALWAYS_EXCLUDED`]), matched
/// against forward-slash relative paths.
pub struct ExcludeSet {
    set: GlobSet,
}

impl ExcludeSet {
    /// Build from the template's declared `[merge].exclude` plus the always-excluded
    /// built-ins. An individual malformed glob is skipped (best-effort) rather than
    /// failing the whole render — the built-ins still apply.
    pub fn new(declared: &[String]) -> Self {
        let mut b = GlobSetBuilder::new();
        for pat in ALWAYS_EXCLUDED {
            if let Ok(g) = Glob::new(pat) {
                b.add(g);
            }
        }
        for pat in declared {
            // Match both `vendor/**` (anchored) and a bare `*.tgz` (any depth) the way an
            // author intuitively expects: a pattern with no `/` is matched at any depth.
            let candidates: Vec<String> = if pat.contains('/') {
                vec![pat.clone()]
            } else {
                vec![pat.clone(), format!("**/{pat}")]
            };
            for c in candidates {
                if let Ok(g) = Glob::new(&c) {
                    b.add(g);
                }
            }
        }
        let set = b.build().unwrap_or_else(|_| GlobSet::empty());
        Self { set }
    }

    /// Whether `rel` (a forward-slash relative path) is excluded from text merge /
    /// substitution.
    pub fn is_excluded(&self, rel: &str) -> bool {
        self.set.is_match(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_excludes_tarballs_and_vendor() {
        let s = ExcludeSet::new(&[]);
        assert!(s.is_excluded("vendor/sdk-1.0.0.tgz"));
        assert!(s.is_excluded("some/deep/asset.png"));
        assert!(s.is_excluded("vendor/anything"));
        assert!(s.is_excluded("node_modules/foo/index.js"));
        assert!(!s.is_excluded("src/extension.ts"));
        assert!(!s.is_excluded("README.md"));
    }

    #[test]
    fn declared_globs_apply() {
        let s = ExcludeSet::new(&["docs/**".to_string(), "*.lock".to_string()]);
        assert!(s.is_excluded("docs/guide.md"));
        // A no-slash pattern matches at any depth.
        assert!(s.is_excluded("a.lock"));
        assert!(s.is_excluded("deep/b.lock"));
        assert!(!s.is_excluded("src/main.ts"));
    }

    #[test]
    fn malformed_glob_is_skipped_not_fatal() {
        // `[` is an unterminated class; the set still builds (and built-ins apply).
        let s = ExcludeSet::new(&["[".to_string()]);
        assert!(s.is_excluded("x.tgz"));
    }
}
