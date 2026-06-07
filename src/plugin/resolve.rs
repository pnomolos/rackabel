//! External-subcommand resolution (DESIGN §5.1/§5.6).
//!
//! FOUNDATION-OWNED. `rackabel <foo>` (when `foo` is not a built-in) and
//! `plugin which/run <foo>` resolve `foo` to an executable `rackabel-<foo>`, searching
//! `~/.rackabel/plugins/bin` FIRST, then `$PATH`. Built-ins ALWAYS win — `resolve`
//! refuses a reserved token. When a name resolves from BOTH locations, a one-time
//! warning surfaces the shadowing proactively (cargo-#6507 lesson).
//!
//! This module is pure-ish: it takes the search-path inputs explicitly (so tests don't
//! depend on the real `$PATH`/home) and returns a [`Resolution`]; the command files turn
//! that into an exec or a framed error.

use std::path::{Path, PathBuf};

use crate::cli::is_reserved;
use crate::context::Ctx;

use super::{exe_basename, plugins_bin_dir};

/// How a `rackabel-<name>` token resolves (§5.1/§5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// A built-in subcommand claims the name; the external lookup never runs. If a
    /// `rackabel-<name>` ALSO exists somewhere, `shadowed_plugin` names it (so
    /// `plugin which` can report "shadowed by built-in" and point at `plugin run`).
    Builtin { shadowed_plugin: Option<PathBuf> },
    /// Found a managed plugin (under `~/.rackabel/plugins/bin`). `also_on_path` is set
    /// when the same name is ALSO on `$PATH` (the both-locations case → one-time
    /// warning, "using the managed one").
    Managed { path: PathBuf, also_on_path: bool },
    /// Found only on `$PATH` (an unmanaged `rackabel-<name>` the user dropped there).
    Path { path: PathBuf },
    /// No built-in and no executable anywhere.
    NotFound,
}

impl Resolution {
    /// The executable to run for a found resolution, ignoring built-in shadowing (used
    /// by `plugin run`, the §5.6 escape hatch). `None` for `Builtin`-without-plugin or
    /// `NotFound`.
    pub fn plugin_path(&self) -> Option<&Path> {
        match self {
            Self::Builtin {
                shadowed_plugin: Some(p),
            } => Some(p),
            Self::Managed { path, .. } | Self::Path { path } => Some(path),
            _ => None,
        }
    }

    /// Whether running this name as a BARE `rackabel <name>` would hit a built-in (so a
    /// plugin of the same name is shadowed).
    pub fn is_shadowed_by_builtin(&self) -> bool {
        matches!(self, Self::Builtin { .. })
    }

    /// Whether the resolution warrants the one-time both-locations warning.
    pub fn both_locations(&self) -> bool {
        matches!(
            self,
            Self::Managed {
                also_on_path: true,
                ..
            }
        )
    }
}

/// Resolve `name` against the managed bin dir + a `$PATH` lookup, applying the built-in
/// precedence rule. `path_lookup` finds a `rackabel-<name>` on `$PATH` (injected so tests
/// don't touch the real PATH; production passes [`path_lookup_real`]).
pub fn resolve(ctx: &Ctx, name: &str, path_lookup: impl Fn(&str) -> Option<PathBuf>) -> Resolution {
    let exe = exe_basename(name);
    let managed = managed_candidate(ctx, &exe);
    let on_path = path_lookup(&exe);

    // Built-ins always win: even if a plugin exists, the bare name resolves to the
    // built-in. We still record the shadowed plugin (managed first, then PATH) so
    // `plugin which` can report it and point at `plugin run`.
    if is_reserved(name) {
        let shadowed = managed.clone().or(on_path);
        return Resolution::Builtin {
            shadowed_plugin: shadowed,
        };
    }

    match (managed, on_path) {
        (Some(m), other) => Resolution::Managed {
            path: m,
            also_on_path: other.is_some(),
        },
        (None, Some(p)) => Resolution::Path { path: p },
        (None, None) => Resolution::NotFound,
    }
}

/// Resolve using the real `$PATH` (production). A thin wrapper over [`resolve`] with
/// [`path_lookup_real`].
pub fn resolve_real(ctx: &Ctx, name: &str) -> Resolution {
    resolve(ctx, name, path_lookup_real)
}

/// The managed candidate: `~/.rackabel/plugins/bin/rackabel-<name>` if it exists and is a
/// file (or symlink to one).
fn managed_candidate(ctx: &Ctx, exe: &str) -> Option<PathBuf> {
    let p = plugins_bin_dir(ctx).join(exe);
    // A symlink whose target is missing still "exists" via metadata-following; require
    // the resolved path to be a file so a dangling symlink isn't treated as installed.
    if p.exists() && std::fs::metadata(&p).map(|m| m.is_file()).unwrap_or(false) {
        Some(p)
    } else {
        None
    }
}

/// Find `exe` on the real `$PATH` (production `path_lookup`). Uses the `which` crate
/// already in the dependency set.
pub fn path_lookup_real(exe: &str) -> Option<PathBuf> {
    which::which(exe).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn ctx(home: &Path) -> Ctx {
        crate::context::Ctx {
            no_input: true,
            json: false,
            quiet: false,
            verbose: false,
            raw: false,
            color: crate::ui::color::ColorMode::Never,
            color_err: crate::ui::color::ColorMode::Never,
            cwd: home.to_path_buf(),
            rackabel_home: home.join(".rackabel"),
            home: home.to_path_buf(),
            ableton_app: None,
            ableton_user_library: None,
            ableton_eh_mod: None,
            ableton_eh_node: None,
            ableton_extensions_dir: None,
            ableton_storage_base: None,
            rackabel_host_cmd: None,
        }
    }

    /// Create a managed `rackabel-<name>` and return its path.
    fn install_managed(c: &Ctx, name: &str) -> PathBuf {
        let dir = plugins_bin_dir(c);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(exe_basename(name));
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        p
    }

    #[test]
    fn builtin_always_wins_even_with_a_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let shadow = install_managed(&c, "dev"); // a plugin named like a built-in
        let r = resolve(&c, "dev", |_| None);
        assert_eq!(
            r,
            Resolution::Builtin {
                shadowed_plugin: Some(shadow)
            }
        );
        assert!(r.is_shadowed_by_builtin());
        // The escape hatch (`plugin run`) can still reach the shadowed plugin.
        assert!(r.plugin_path().is_some());
    }

    #[test]
    fn managed_is_searched_before_path() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let managed = install_managed(&c, "foo");
        // PATH also has one → both-locations; the managed one wins + warning flag set.
        let on_path = PathBuf::from("/usr/local/bin/rackabel-foo");
        let r = resolve(&c, "foo", |_| Some(on_path.clone()));
        assert_eq!(
            r,
            Resolution::Managed {
                path: managed,
                also_on_path: true
            }
        );
        assert!(r.both_locations());
    }

    #[test]
    fn path_only_when_not_managed() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let on_path = PathBuf::from("/usr/local/bin/rackabel-bar");
        let r = resolve(&c, "bar", |_| Some(on_path.clone()));
        assert_eq!(r, Resolution::Path { path: on_path });
        assert!(!r.both_locations());
    }

    #[test]
    fn not_found_when_absent_everywhere() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        assert_eq!(resolve(&c, "nope", |_| None), Resolution::NotFound);
    }

    #[test]
    fn builtin_with_no_plugin_reports_no_shadow() {
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        let r = resolve(&c, "build", |_| None);
        assert_eq!(
            r,
            Resolution::Builtin {
                shadowed_plugin: None
            }
        );
        // No plugin to run via the escape hatch.
        assert!(r.plugin_path().is_none());
    }

    #[test]
    fn reserved_future_builtin_publish_shadows() {
        // `publish` is reserved ahead of shipping (§5.6); a `rackabel-publish` plugin is
        // shadowed NOW so the collision detector predates the built-in.
        let tmp = tempfile::tempdir().unwrap();
        let c = ctx(tmp.path());
        install_managed(&c, "publish");
        let r = resolve(&c, "publish", |_| None);
        assert!(r.is_shadowed_by_builtin());
    }
}
