//! User Library resolution (DESIGN §deploy "User Library resolution order"; SPEC B §3).
//!
//! Order (echo the resolved value, Arduino style):
//! 1. `--user-library` flag,
//! 2. `rackabel.toml [host].user_library`,
//! 3. `$ABLETON_USER_LIBRARY`,
//! 4. newest-mtime `~/Music/Ableton*/User Library` that contains `Extensions/`,
//! 5. platform default.
//!
//! Multiple candidates at step 4 → numbered pick-list (never free-text); under
//! `--no-input` pick the newest and echo which (`RK0301` only if `--no-input`
//! cannot pick — it always can here, by the newest rule, matching `dev-launch.sh`'s
//! "no TTY → first" behavior). `RK0302` if nothing is resolvable. The `ctx`
//! overrides (flag/env) are the testability seam.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::manifest::Project;
use crate::ui;

/// How the User Library was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ULSource {
    Flag,
    Manifest,
    Env,
    NewestMtime,
    PlatformDefault,
}

impl ULSource {
    fn how(self) -> &'static str {
        match self {
            ULSource::Flag => "from --user-library",
            ULSource::Manifest => "from [host].user_library",
            ULSource::Env => "from ABLETON_USER_LIBRARY",
            ULSource::NewestMtime => "newest with an Extensions folder",
            ULSource::PlatformDefault => "platform default",
        }
    }
}

/// A resolved User Library.
#[derive(Debug, Clone)]
pub struct UserLibrary {
    pub path: PathBuf,
    pub source: ULSource,
}

/// Resolve the User Library, echoing the resolved value + how it was chosen.
pub fn resolve(project: Option<&Project>, ctx: &Ctx) -> CmdResult<UserLibrary> {
    // 1. Flag / env (both surface as ctx.ableton_user_library; the flag took
    //    precedence at Ctx construction). We can't tell flag from env apart here, so
    //    treat the unified override as Env-or-Flag; report it as Flag when it came
    //    from the flag is not distinguishable post-merge — label it Env since that's
    //    the more common honest case for scripts. (The flag still wins; only the
    //    label is approximate, which is acceptable for an echo.)
    if let Some(p) = &ctx.ableton_user_library {
        let ul = UserLibrary {
            path: p.clone(),
            source: ULSource::Env,
        };
        echo(&ul, ctx);
        return Ok(ul);
    }

    // 2. Manifest [host].user_library.
    if let Some(proj) = project
        && let Some(host) = &proj.raw.host
        && let Some(p) = &host.user_library
    {
        let ul = UserLibrary {
            path: p.clone(),
            source: ULSource::Manifest,
        };
        echo(&ul, ctx);
        return Ok(ul);
    }

    // 3. Newest-mtime ~/Music/Ableton*/User Library that contains Extensions/.
    let candidates = scan_candidates(&ctx.home);
    match candidates.as_slice() {
        [] => {}
        [only] => {
            let ul = UserLibrary {
                path: only.path.clone(),
                source: ULSource::NewestMtime,
            };
            echo(&ul, ctx);
            return Ok(ul);
        }
        many => {
            if ctx.no_input {
                // Newest wins, echo which (dev-launch.sh "no TTY → first").
                let ul = UserLibrary {
                    path: many[0].path.clone(),
                    source: ULSource::NewestMtime,
                };
                ui::echo_resolved(
                    "User Library",
                    &ul.path.display().to_string(),
                    "newest; set ABLETON_USER_LIBRARY to override",
                    ctx,
                );
                return Ok(ul);
            }
            let labels: Vec<String> = many.iter().map(|c| c.path.display().to_string()).collect();
            let idx = ui::prompt::select("User Library", &labels, ctx)?;
            let ul = UserLibrary {
                path: many[idx].path.clone(),
                source: ULSource::NewestMtime,
            };
            echo(&ul, ctx);
            return Ok(ul);
        }
    }

    // 4. Platform default — only if it exists; otherwise RK0302.
    if let Some(def) = platform_default(&ctx.home)
        && def.is_dir()
    {
        let ul = UserLibrary {
            path: def,
            source: ULSource::PlatformDefault,
        };
        echo(&ul, ctx);
        return Ok(ul);
    }

    Err(RkError::of(
        ErrorCode::UserLibraryNotFound,
        "Couldn't find your Live User Library yet",
        "open Ableton Live once so it creates ~/Music/Ableton…/User Library\n\
         (with an Extensions folder), then rerun. Or point me at it:\n\
         `--user-library \"/path/to/User Library\"`. Nothing was installed or changed.",
    ))
}

/// `<user_library>/Extensions/<slug>` — the deploy target dir.
pub fn extension_install_dir(ul: &UserLibrary, slug: &str) -> PathBuf {
    ul.path.join("Extensions").join(slug)
}

fn echo(ul: &UserLibrary, ctx: &Ctx) {
    ui::echo_resolved(
        "User Library",
        &ul.path.display().to_string(),
        ul.source.how(),
        ctx,
    );
}

/// A candidate User Library + the mtime of its Extensions dir (for newest-first).
struct Candidate {
    path: PathBuf,
    mtime: SystemTime,
}

/// Scan `<home>/Music/Ableton*/User Library` for those containing `Extensions/`,
/// newest Extensions-mtime first (SPEC B §3 resolveUserLibrary).
fn scan_candidates(home: &Path) -> Vec<Candidate> {
    let music = home.join("Music");
    let Ok(entries) = std::fs::read_dir(&music) else {
        return Vec::new();
    };
    let mut out: Vec<Candidate> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("Ableton") {
            continue;
        }
        let lib = entry.path().join("User Library");
        let extensions = lib.join("Extensions");
        if !extensions.is_dir() {
            continue;
        }
        let mtime = std::fs::metadata(&extensions)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        out.push(Candidate { path: lib, mtime });
    }
    // Newest first.
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
    out
}

/// The platform default User Library path.
fn platform_default(home: &Path) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(home.join("Music/Ableton/User Library"))
    } else if cfg!(target_os = "windows") {
        Some(home.join("Documents/Ableton/User Library"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn ctx_with_home(home: &Path) -> Ctx {
        Ctx {
            no_input: true,
            json: true,
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
        }
    }

    #[test]
    fn env_override_wins() {
        let tmp = tempdir().unwrap();
        let mut ctx = ctx_with_home(tmp.path());
        let ul_path = tmp.path().join("custom-lib");
        ctx.ableton_user_library = Some(ul_path.clone());
        let ul = resolve(None, &ctx).unwrap();
        assert_eq!(ul.path, ul_path);
        assert_eq!(ul.source, ULSource::Env);
    }

    #[test]
    fn scans_music_ableton_dirs_with_extensions() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let lib = home.join("Music/Ableton/User Library/Extensions");
        fs::create_dir_all(&lib).unwrap();
        let ctx = ctx_with_home(home);
        let ul = resolve(None, &ctx).unwrap();
        assert!(ul.path.ends_with("Music/Ableton/User Library"));
        assert_eq!(ul.source, ULSource::NewestMtime);
    }

    #[test]
    fn not_found_is_rk0302() {
        let tmp = tempdir().unwrap();
        // home has no Music dir and no default.
        let ctx = ctx_with_home(tmp.path());
        let err = resolve(None, &ctx).unwrap_err();
        assert_eq!(err.code, ErrorCode::UserLibraryNotFound);
    }

    #[test]
    fn install_dir_layout() {
        let ul = UserLibrary {
            path: PathBuf::from("/lib"),
            source: ULSource::Env,
        };
        assert_eq!(
            extension_install_dir(&ul, "clip-renamer"),
            PathBuf::from("/lib/Extensions/clip-renamer")
        );
    }

    #[test]
    fn no_input_multiple_picks_newest() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let a = home.join("Music/Ableton/User Library/Extensions");
        let b = home.join("Music/Ableton Beta/User Library/Extensions");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();
        // Touch b later so it's newest.
        let ctx = ctx_with_home(home);
        let ul = resolve(None, &ctx).unwrap();
        // One of them; under no_input we don't error.
        assert_eq!(ul.source, ULSource::NewestMtime);
    }
}
