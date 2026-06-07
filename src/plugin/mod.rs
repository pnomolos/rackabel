//! rackabel's own third-party plugin model (DESIGN §5) — the frozen 0.4 surface.
//!
//! FOUNDATION-OWNED. This module is the contract the three parallel 0.4 feature agents
//! (PATH-subcommands, templates, plugin-management) build on. It holds:
//!   - the on-disk layout under `~/.rackabel/plugins/` (`bin/` symlink dir + store) and
//!     the `plugins.lock` serde model ([`lock`]);
//!   - the §5.2 env-contract builder ([`env_contract`]) — the exact var map a plugin
//!     gets, with the commit-unset-not-empty presence rules;
//!   - external-subcommand resolution ([`resolve`]) — managed-bin-first, then `$PATH`,
//!     built-ins always winning, with the both-locations warning;
//!   - template source resolution + the GitHub-API / git-base seams ([`source`]),
//!     the git operations wrapper ([`git`]), the sha256 helper ([`sha256`]), and the
//!     `rackabel-template.toml` / `.rackabel-template` models ([`template`]).
//!
//! The feature agents fill the *behavioral* bodies (the actual fetch, the 3-way merge,
//! the GitHub query); the foundation lands the models, the parsers, the seams, and
//! compiling stubs that return a clear framed error so the whole tree builds.

pub mod collision;
pub mod env_contract;
pub mod git;
pub mod lock;
pub mod manifest;
pub mod resolve;
pub mod sha256;
pub mod source;
pub mod store;
pub mod template;

use std::path::PathBuf;

use crate::context::Ctx;

/// The plugins root: `~/.rackabel/plugins`.
pub fn plugins_dir(ctx: &Ctx) -> PathBuf {
    ctx.rackabel_home.join("plugins")
}

/// The managed bin dir: `~/.rackabel/plugins/bin`. This is the directory §5.1 searches
/// FIRST (before `$PATH`); `plugin install` symlinks the resolved executable here, and
/// the both-locations warning ("using the managed one") distinguishes it from a
/// `rackabel-<foo>` the user dropped on `$PATH` themselves.
pub fn plugins_bin_dir(ctx: &Ctx) -> PathBuf {
    plugins_dir(ctx).join("bin")
}

/// The store dir: `~/.rackabel/plugins/store/<name>` holds an installed plugin's actual
/// files (the downloaded asset, or the clone's build output); the `bin/` entry is a
/// symlink into here. Keeping the real files in a per-name store dir means uninstall is
/// "remove the store dir + the symlink", and the symlink target is stable.
pub fn plugin_store_dir(ctx: &Ctx, name: &str) -> PathBuf {
    plugins_dir(ctx).join("store").join(name)
}

/// The lockfile path: `~/.rackabel/plugins.lock` (DESIGN §5.4). Pins every managed
/// plugin by commit/sha256; authoritative; never auto-updated silently.
pub fn lock_path(ctx: &Ctx) -> PathBuf {
    ctx.rackabel_home.join("plugins.lock")
}

/// The conventional `rackabel-<name>` executable basename for a plugin `name` (§5.1).
pub fn exe_basename(name: &str) -> String {
    format!("rackabel-{name}")
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

    #[test]
    fn layout_paths_are_under_rackabel_home() {
        let c = ctx(Path::new("/home/u"));
        assert_eq!(plugins_dir(&c), Path::new("/home/u/.rackabel/plugins"));
        assert_eq!(
            plugins_bin_dir(&c),
            Path::new("/home/u/.rackabel/plugins/bin")
        );
        assert_eq!(
            plugin_store_dir(&c, "foo"),
            Path::new("/home/u/.rackabel/plugins/store/foo")
        );
        assert_eq!(lock_path(&c), Path::new("/home/u/.rackabel/plugins.lock"));
    }

    #[test]
    fn exe_basename_is_rackabel_prefixed() {
        assert_eq!(exe_basename("notarize"), "rackabel-notarize");
    }
}
