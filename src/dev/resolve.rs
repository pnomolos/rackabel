//! Resolving the Live install + host paths + the per-extension `initialize()` specs the
//! daemon launches (DESIGN §3.1/§3.6, SPEC H §0/§8, SPEC D §7).
//!
//! OWNED BY THE DAEMON-CORE AGENT. Centralizes:
//!   - which Live `.app` this dev session serves (`--live` → `[host].live` →
//!     `$ABLETON_APP` → persisted `.rackabel/state.toml` → newest detected), persisting
//!     the choice so `rackabel dev` recalls it (§3.6 multi-Live);
//!   - the host binaries (`EH_NODE` bundled-node preferred for ABI, `EH_MOD` host
//!     module — both overridable via `--eh-node`/`--eh-mod`/`ABLETON_*`);
//!   - building each `ExtensionSpec` with the §3.6 storage/temp layout
//!     (`Extension Data/<slug>` + `/tmp/<slug>`), `mkdir -p`'d by the host launcher.
//!
//! The per-Live daemon socket/pidfile are keyed off the resolved `.app` path hash
//! (`super::sock_path`/`pid_path`), so distinct Live installs get distinct daemons.

use std::path::{Path, PathBuf};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::manifest::{self, Project};
use crate::services::live::{self, LiveInstall};
use crate::services::node;

use super::host::{ExtensionSpec, HostConfig};
use super::{Inspect, RegistryEntry};

/// The resolved dev target: the chosen Live install + the host binaries.
#[derive(Debug, Clone)]
pub struct DevTarget {
    pub live: LiveInstall,
    pub eh_node: PathBuf,
    pub eh_mod: PathBuf,
}

impl DevTarget {
    /// The chosen Live `.app` path.
    pub fn app(&self) -> &Path {
        &self.live.app
    }
}

/// Resolve the Live install for this dev session and the host binaries, persisting the
/// choice into the project's `.rackabel/state.toml` when a project is present (§3.6).
///
/// Order: `--live`/`$ABLETON_APP` (already merged into `ctx.ableton_app`) →
/// `[host].live` → persisted `state.dev_live` → newest detected (pick-list if
/// ambiguous + interactive; newest-and-echo under `--no-input`).
pub fn resolve(ctx: &Ctx) -> CmdResult<DevTarget> {
    // The project is optional: `dev` can run from a registered set without a cwd
    // project, but a cwd project lets us read `[host].live` and persist the choice.
    let project = Project::discover_cwd(ctx).ok();

    // 1. Explicit override (flag/env) → ctx.ableton_app.
    // 2. [host].live from the cwd project.
    // 3. Persisted state.dev_live.
    let explicit: Option<PathBuf> = ctx
        .ableton_app
        .clone()
        .or_else(|| {
            project
                .as_ref()
                .and_then(|p| p.raw.host.as_ref())
                .and_then(|h| h.live.clone())
        })
        .or_else(|| {
            project
                .as_ref()
                .and_then(|p| manifest::state::load(&p.root).ok())
                .and_then(|s| s.dev_live.map(PathBuf::from))
        });

    let install = match explicit {
        Some(app) => {
            let inspected = live::inspect(&app);
            if inspected.host_module.is_none() {
                return Err(no_host_module_error(&app));
            }
            inspected
        }
        None => live::primary(ctx)?,
    };

    let eh_mod = resolve_eh_mod(ctx, &install)?;
    let eh_node = resolve_eh_node(ctx, &install)?;

    // Persist the choice for next time (best-effort; never fail the run on it).
    if let Some(p) = &project
        && let Ok(mut state) = manifest::state::load(&p.root)
    {
        let chosen = install.app.display().to_string();
        if state.dev_live.as_deref() != Some(chosen.as_str()) {
            state.dev_live = Some(chosen);
            let _ = manifest::state::save(&p.root, &state);
        }
    }

    Ok(DevTarget {
        live: install,
        eh_node,
        eh_mod,
    })
}

/// Resolve the host module: `--eh-mod`/`$ABLETON_EH_MOD` override, else the detected
/// install's probed module (both layouts already tried, SPEC H §0).
fn resolve_eh_mod(ctx: &Ctx, install: &LiveInstall) -> CmdResult<PathBuf> {
    if let Some(p) = &ctx.ableton_eh_mod {
        return Ok(p.clone());
    }
    install
        .host_module
        .clone()
        .ok_or_else(|| no_host_module_error(&install.app))
}

/// Resolve the host node: `--eh-node`/`$ABLETON_EH_NODE` override → Live's bundled node
/// (ABI match) → PATH node fallback (SPEC H §0 / DESIGN §3.1).
fn resolve_eh_node(ctx: &Ctx, install: &LiveInstall) -> CmdResult<PathBuf> {
    if let Some(rt) = node::resolve(Some(install), ctx) {
        return Ok(rt.bin);
    }
    Err(RkError::of(
        ErrorCode::HostLaunchFailed,
        "no usable Node runtime to run the Extension Host",
        "install Ableton Live 12.4.5+ (it bundles the right Node), then retry; \
         or point at one with --eh-node",
    )
    .at(install.app.display().to_string()))
}

fn no_host_module_error(app: &Path) -> RkError {
    RkError::of(
        ErrorCode::HostLaunchFailed,
        "couldn't find Live's Extension Host module",
        "confirm this Live supports Extensions (12.4.5+) with `rackabel doctor`, \
         or point at the module with --eh-mod",
    )
    .at(app.display().to_string())
}

/// Build a [`HostConfig`] for `extensions` against the resolved target. Honors the
/// `RACKABEL_HOST_CMD` test seam (so hermetic tests run the FakeHost) and the §3.6
/// storage/temp layout. `extensions` should already be the host-compatible
/// (pre-filtered) set — see [`super::registry::Registry::prefilter`].
pub fn host_config(
    target: &DevTarget,
    extensions: &[RegistryEntry],
    inspect: Option<Inspect>,
    ctx: &Ctx,
) -> HostConfig {
    let specs = extensions
        .iter()
        .map(|e| ext_spec(e, ctx))
        .collect::<Vec<_>>();
    HostConfig {
        eh_node: target.eh_node.clone(),
        eh_mod: target.eh_mod.clone(),
        extensions: specs,
        inspect,
        host_cmd_override: ctx.rackabel_host_cmd.clone(),
    }
}

/// The deployed-extension dir the host loads + the §3.6 storage/temp layout for an
/// entry. `path` is the deployed `<UserLibrary>/Extensions/<slug>` bundle when known;
/// for the dev `source = dist` case the registry stores the project root, and the watch
/// agent deploys before reload — so here we point at the deployed copy by slug under the
/// resolved User Library, falling back to the project root if the library can't be
/// resolved (the host reads `manifest.json` + `dist/extension.js` from whichever).
fn ext_spec(entry: &RegistryEntry, ctx: &Ctx) -> ExtensionSpec {
    let slug = slug_for(&entry.path);
    let deployed = deployed_dir(&slug, ctx).unwrap_or_else(|| entry.path.clone());
    ExtensionSpec {
        name: entry.name.clone(),
        path: deployed,
        storage_directory: storage_dir(&slug, ctx),
        temp_directory: temp_dir(&slug),
    }
}

/// The deployed bundle dir `<UserLibrary>/Extensions/<slug>`, if the User Library
/// resolves (best-effort; the watch agent deploys there before reload).
fn deployed_dir(slug: &str, ctx: &Ctx) -> Option<PathBuf> {
    let ul = crate::services::user_library::resolve_newest(None, ctx).ok()?;
    Some(ul.path.join("Extensions").join(slug))
}

/// `~/Library/Application Support/Ableton/Extension Data/<slug>` (DESIGN §3.6), or the
/// `--storage-base`/`$ABLETON_STORAGE_BASE` override base when set.
fn storage_dir(slug: &str, ctx: &Ctx) -> PathBuf {
    if let Some(base) = &ctx.ableton_storage_base {
        return base.join(slug);
    }
    ctx.home
        .join("Library/Application Support/Ableton/Extension Data")
        .join(slug)
}

/// `/tmp/<slug>` (DESIGN §3.6).
fn temp_dir(slug: &str) -> PathBuf {
    std::env::temp_dir().join(slug)
}

/// The slug = the project-root dir basename (existing `Project::slug()` convention).
fn slug_for(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("extension")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with(home: &Path) -> Ctx {
        Ctx {
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
    fn storage_and_temp_follow_slug_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with(tmp.path());
        let s = storage_dir("clip-renamer", &ctx);
        assert!(s.ends_with("Extension Data/clip-renamer"));
        let t = temp_dir("clip-renamer");
        assert!(t.ends_with("clip-renamer"));
    }

    #[test]
    fn storage_base_override_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = ctx_with(tmp.path());
        ctx.ableton_storage_base = Some(PathBuf::from("/custom/base"));
        assert_eq!(storage_dir("foo", &ctx), PathBuf::from("/custom/base/foo"));
    }

    #[test]
    fn host_config_carries_override_seam() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = ctx_with(tmp.path());
        ctx.rackabel_host_cmd = Some(vec!["/bin/fake".into()]);
        let target = DevTarget {
            live: live::inspect(Path::new("/nope.app")),
            eh_node: PathBuf::from("/node"),
            eh_mod: PathBuf::from("/mod.node"),
        };
        let cfg = host_config(&target, &[], None, &ctx);
        assert_eq!(cfg.host_cmd_override, Some(vec!["/bin/fake".to_string()]));
    }
}
