//! Source resolution + the network/seam contracts (DESIGN §5.4/§5.5).
//!
//! FOUNDATION-OWNED. This module classifies the third-party source strings the user
//! gives and exposes the test seams the feature agents and tests rewrite:
//!   - [`TemplateSource`] — parse/classify a `new --template` ref
//!     (`gh:owner/repo[@ref]` | `@scope/name` | local path);
//!   - [`PluginSource`] — parse/classify a `plugin install` source
//!     (`OWNER/REPO` | local path | tarball);
//!   - [`git_base`] — the `RACKABEL_TEMPLATE_GIT_BASE` seam that rewrites a `gh:` ref to
//!     a local `file://` base so tests resolve against fixture repos, never the network;
//!   - [`github_api_base`] — the `RACKABEL_GITHUB_API` seam for `plugin search`, so
//!     tests stub the GitHub API base URL.
//!
//! Classification is pure and fully tested; the actual fetch/clone/query is the feature
//! agents' job (they call [`super::git`] / a network client behind these seams).

use std::path::PathBuf;

/// The default GitHub git host used to expand a `gh:owner/repo` ref when no
/// `RACKABEL_TEMPLATE_GIT_BASE` seam is set.
pub const DEFAULT_GIT_BASE: &str = "https://github.com";

/// The default GitHub REST API base used by `plugin search` when no `RACKABEL_GITHUB_API`
/// seam is set.
pub const DEFAULT_GITHUB_API: &str = "https://api.github.com";

/// The env seam for the git host: tests set `RACKABEL_TEMPLATE_GIT_BASE` to a local
/// `file://…` dir so a `gh:owner/repo` ref clones a fixture repo at
/// `<base>/owner/repo` (or `<base>/owner/repo.git`) instead of github.com — keeping the
/// test suite off the network. Production leaves it unset and uses [`DEFAULT_GIT_BASE`].
pub fn git_base() -> String {
    std::env::var("RACKABEL_TEMPLATE_GIT_BASE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_GIT_BASE.to_string())
}

/// The env seam for the GitHub API base (`plugin search`): tests set `RACKABEL_GITHUB_API`
/// to a local stub server URL. Production leaves it unset and uses [`DEFAULT_GITHUB_API`].
pub fn github_api_base() -> String {
    std::env::var("RACKABEL_GITHUB_API")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_GITHUB_API.to_string())
}

/// A classified `new --template` source (§5.5). `Local` skips the remote-confirmation
/// prompt; `Gh`/`Scope` are remote third-party code and require confirmation before any
/// fetch (§5.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSource {
    /// `gh:owner/repo[@ref]` — a GitHub repo, optionally at a ref/branch/tag.
    Gh {
        owner: String,
        repo: String,
        git_ref: Option<String>,
    },
    /// `@scope/name` — a scoped shorthand (resolved like a gh repo `scope/name`).
    Scope { scope: String, name: String },
    /// A local directory path (no network, no confirmation prompt).
    Local(PathBuf),
}

impl TemplateSource {
    /// Classify a `--template` ref. A `gh:` prefix is a GitHub repo; a leading `@` is a
    /// scoped name; anything else is treated as a local path. Returns `None` for an
    /// empty/clearly-malformed ref (the caller frames `RK0402 TemplateNotFound`).
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        if s.is_empty() {
            return None;
        }
        if let Some(rest) = s.strip_prefix("gh:") {
            // owner/repo[@ref]
            let (path, git_ref) = match rest.split_once('@') {
                Some((p, r)) if !r.is_empty() => (p, Some(r.to_string())),
                _ => (rest, None),
            };
            let (owner, repo) = path.split_once('/')?;
            if owner.is_empty() || repo.is_empty() {
                return None;
            }
            return Some(Self::Gh {
                owner: owner.to_string(),
                repo: repo.to_string(),
                git_ref,
            });
        }
        if let Some(rest) = s.strip_prefix('@') {
            let (scope, name) = rest.split_once('/')?;
            if scope.is_empty() || name.is_empty() {
                return None;
            }
            return Some(Self::Scope {
                scope: scope.to_string(),
                name: name.to_string(),
            });
        }
        Some(Self::Local(PathBuf::from(s)))
    }

    /// Whether this source is remote third-party code (requires the §5.7 confirmation
    /// before fetch + auto-build). A local path is trusted (skips the prompt).
    pub fn is_remote(&self) -> bool {
        !matches!(self, Self::Local(_))
    }

    /// The git clone URL for a remote source, built from the [`git_base`] seam.
    /// `<base>/<owner>/<repo>` for both `Gh` and `Scope` (a scope resolves like an
    /// owner). `None` for a local source. The ref is carried separately (clone
    /// `--branch`), not in the URL.
    pub fn clone_url(&self) -> Option<String> {
        let base = git_base().trim_end_matches('/').to_string();
        match self {
            Self::Gh { owner, repo, .. } => Some(format!("{base}/{owner}/{repo}")),
            Self::Scope { scope, name } => Some(format!("{base}/{scope}/{name}")),
            Self::Local(_) => None,
        }
    }

    /// The git ref to clone at, if any (only `Gh` carries one).
    pub fn git_ref(&self) -> Option<&str> {
        match self {
            Self::Gh { git_ref, .. } => git_ref.as_deref(),
            _ => None,
        }
    }

    /// A human display of the source for the confirmation prompt (§5.7), e.g.
    /// `gh:owner/repo@v1` or the local path.
    pub fn display(&self) -> String {
        match self {
            Self::Gh {
                owner,
                repo,
                git_ref,
            } => match git_ref {
                Some(r) => format!("gh:{owner}/{repo}@{r}"),
                None => format!("gh:{owner}/{repo}"),
            },
            Self::Scope { scope, name } => format!("@{scope}/{name}"),
            Self::Local(p) => p.display().to_string(),
        }
    }
}

/// A classified `plugin install` source (§5.4). `Repo` is gh-style (release asset, else
/// clone+run) and is remote third-party code; `Path`/`Tarball` are sideloads (always
/// work, no gatekeeper — but the bytes are still pinned by sha256).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSource {
    /// `OWNER/REPO` — a GitHub repo (prefer release asset `rackabel-<name>-<os>-<arch>`,
    /// else clone + run).
    Repo { owner: String, repo: String },
    /// A local directory or executable path to sideload.
    Path(PathBuf),
    /// A local (or already-downloaded) `.tgz`/`.tar.gz` tarball to sideload.
    Tarball(PathBuf),
}

impl PluginSource {
    /// Classify a `plugin install` source. A bare `OWNER/REPO` (a single `/`, no path
    /// separators that make it look local, not starting with `.`/`/`/`~`) is a gh repo;
    /// a path ending `.tgz`/`.tar.gz` is a tarball; anything else is a local path.
    pub fn parse(raw: &str) -> Option<Self> {
        let s = raw.trim();
        if s.is_empty() {
            return None;
        }
        // Tarball by extension (works whether or not the path exists yet).
        if s.ends_with(".tgz") || s.ends_with(".tar.gz") {
            return Some(Self::Tarball(PathBuf::from(s)));
        }
        // A local-looking path takes precedence over OWNER/REPO so `./a/b` is a path.
        if looks_local(s) {
            return Some(Self::Path(PathBuf::from(s)));
        }
        // OWNER/REPO: exactly one slash, both halves non-empty.
        if let Some((owner, repo)) = s.split_once('/')
            && !owner.is_empty()
            && !repo.is_empty()
            && !repo.contains('/')
        {
            return Some(Self::Repo {
                owner: owner.to_string(),
                repo: repo.to_string(),
            });
        }
        // A bare token with no slash is treated as a local path (e.g. `./rackabel-foo`
        // without the dot is unusual, but we don't guess it's a repo).
        Some(Self::Path(PathBuf::from(s)))
    }

    /// Whether this source is remote third-party code (the §5.7 install confirmation +
    /// "clone + run" applies). Sideloads are local but still pinned.
    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Repo { .. })
    }

    /// The git clone URL for a `Repo` source (via the [`git_base`] seam), else `None`.
    pub fn clone_url(&self) -> Option<String> {
        match self {
            Self::Repo { owner, repo } => {
                let base = git_base().trim_end_matches('/').to_string();
                Some(format!("{base}/{owner}/{repo}"))
            }
            _ => None,
        }
    }

    /// A human display of the source for the confirmation prompt.
    pub fn display(&self) -> String {
        match self {
            Self::Repo { owner, repo } => format!("{owner}/{repo}"),
            Self::Path(p) | Self::Tarball(p) => p.display().to_string(),
        }
    }
}

/// Whether a source string looks like a local filesystem path rather than `OWNER/REPO`.
fn looks_local(s: &str) -> bool {
    s.starts_with('.')
        || s.starts_with('/')
        || s.starts_with('~')
        || s.starts_with("file:")
        // a Windows-style drive or a backslash
        || s.contains('\\')
        // more than one path segment that isn't OWNER/REPO (e.g. a/b/c)
        || s.matches('/').count() > 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_gh_with_and_without_ref() {
        assert_eq!(
            TemplateSource::parse("gh:owner/repo"),
            Some(TemplateSource::Gh {
                owner: "owner".into(),
                repo: "repo".into(),
                git_ref: None
            })
        );
        assert_eq!(
            TemplateSource::parse("gh:owner/repo@v1.2.0"),
            Some(TemplateSource::Gh {
                owner: "owner".into(),
                repo: "repo".into(),
                git_ref: Some("v1.2.0".into())
            })
        );
    }

    #[test]
    fn template_scope_and_local() {
        assert_eq!(
            TemplateSource::parse("@acme/starter"),
            Some(TemplateSource::Scope {
                scope: "acme".into(),
                name: "starter".into()
            })
        );
        assert_eq!(
            TemplateSource::parse("./local/dir"),
            Some(TemplateSource::Local(PathBuf::from("./local/dir")))
        );
        // A bare name with no @/gh: is local.
        assert!(matches!(
            TemplateSource::parse("default"),
            Some(TemplateSource::Local(_))
        ));
    }

    #[test]
    fn template_remote_vs_local_classification() {
        assert!(TemplateSource::parse("gh:o/r").unwrap().is_remote());
        assert!(TemplateSource::parse("@s/n").unwrap().is_remote());
        assert!(!TemplateSource::parse("./x").unwrap().is_remote());
    }

    #[test]
    fn template_malformed_is_none() {
        assert_eq!(TemplateSource::parse(""), None);
        assert_eq!(TemplateSource::parse("gh:owner"), None);
        assert_eq!(TemplateSource::parse("gh:/repo"), None);
        assert_eq!(TemplateSource::parse("@scope"), None);
    }

    #[test]
    fn template_clone_url_honors_seam() {
        // Default base.
        unsafe {
            std::env::remove_var("RACKABEL_TEMPLATE_GIT_BASE");
        }
        let s = TemplateSource::parse("gh:owner/repo").unwrap();
        assert_eq!(
            s.clone_url(),
            Some("https://github.com/owner/repo".to_string())
        );
        // Seam-rewritten base (tests point at a local file:// dir).
        unsafe {
            std::env::set_var("RACKABEL_TEMPLATE_GIT_BASE", "file:///tmp/fixtures/");
        }
        assert_eq!(
            s.clone_url(),
            Some("file:///tmp/fixtures/owner/repo".to_string())
        );
        unsafe {
            std::env::remove_var("RACKABEL_TEMPLATE_GIT_BASE");
        }
    }

    #[test]
    fn plugin_repo_path_tarball() {
        assert_eq!(
            PluginSource::parse("owner/repo"),
            Some(PluginSource::Repo {
                owner: "owner".into(),
                repo: "repo".into()
            })
        );
        assert_eq!(
            PluginSource::parse("./rackabel-foo"),
            Some(PluginSource::Path(PathBuf::from("./rackabel-foo")))
        );
        assert_eq!(
            PluginSource::parse("/abs/path/rackabel-foo"),
            Some(PluginSource::Path(PathBuf::from("/abs/path/rackabel-foo")))
        );
        assert_eq!(
            PluginSource::parse("~/dl/rackabel-bar-1.0.0.tgz"),
            Some(PluginSource::Tarball(PathBuf::from(
                "~/dl/rackabel-bar-1.0.0.tgz"
            )))
        );
        // A multi-segment path is local, not OWNER/REPO.
        assert!(matches!(
            PluginSource::parse("a/b/c"),
            Some(PluginSource::Path(_))
        ));
    }

    #[test]
    fn plugin_remote_classification() {
        assert!(PluginSource::parse("owner/repo").unwrap().is_remote());
        assert!(!PluginSource::parse("./x").unwrap().is_remote());
        assert!(!PluginSource::parse("x.tgz").unwrap().is_remote());
    }

    #[test]
    fn github_api_base_honors_seam() {
        unsafe {
            std::env::remove_var("RACKABEL_GITHUB_API");
        }
        assert_eq!(github_api_base(), "https://api.github.com");
        unsafe {
            std::env::set_var("RACKABEL_GITHUB_API", "http://127.0.0.1:8080");
        }
        assert_eq!(github_api_base(), "http://127.0.0.1:8080");
        unsafe {
            std::env::remove_var("RACKABEL_GITHUB_API");
        }
    }
}
