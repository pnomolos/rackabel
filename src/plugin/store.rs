//! The plugin install/store engine (DESIGN §5.4) — OWNED BY THE PLUGIN-MGMT AGENT.
//!
//! `plugin install` resolves a source to a single executable, places its files in the
//! per-name store (`~/.rackabel/plugins/store/<name>`), symlinks the executable into the
//! managed bin (`~/.rackabel/plugins/bin/rackabel-<name>`, the §5.1 first-searched dir),
//! pins the resolved bytes/commit in `plugins.lock`, and records the inert
//! `rackabel-plugin.toml` presence + hook list for 0.5 (it never runs a hook).
//!
//! Three resolution paths (§5.4):
//!   - **sideload a local PATH** (a `rackabel-<name>` executable or a dir containing one) —
//!     always works, no gatekeeper; pinned by sha256 of the executable.
//!   - **sideload a TARBALL** (`.tgz`/`.tar.gz`) — unpacked into the store; the
//!     `rackabel-<name>` inside is the executable; pinned by sha256 of that executable.
//!   - **`OWNER/REPO`** — prefer a release asset `rackabel-<name>-<os>-<arch>` via the
//!     GitHub API (sha256-pinned), else clone the repo and build it (commit-pinned). A
//!     clone with no obvious build is a clear frame, not a silent success.
//!
//! Every remote install (`OWNER/REPO`) prints what it will fetch/run and requires
//! confirmation (`--yes` scripts it; `--no-input` refuses with `RK0403`); a sideload is
//! local code the user already has, so it still prints what it will do but is not gated
//! behind the remote-fetch consent (the bytes are pinned regardless). A `plugins.lock` pin
//! mismatch at verify time is `RK4007` (exit 4); `--force` announces a deliberate update.

use std::path::{Path, PathBuf};

use crate::cli::PluginInstallArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::plugin::lock::{LockFile, PluginLockEntry, SourceKind};
use crate::plugin::manifest::PluginManifest;
use crate::plugin::source::PluginSource;
use crate::plugin::{exe_basename, git, plugin_store_dir, plugins_bin_dir, sha256};
use crate::ui;

/// The env seam for the GitHub release-asset download host. Tests point it at a local stub
/// server so the asset fetch never touches the network; production leaves it unset and uses
/// GitHub's real release-download host. (The API base — used to LIST the asset URLs — is
/// the foundation's `RACKABEL_GITHUB_API` seam.)
const DL_HOST_ENV: &str = "RACKABEL_GITHUB_DL";
const DEFAULT_DL_HOST: &str = "https://github.com";

/// The resolved outcome of fetching/locating a plugin's code, ready to record + link.
struct Resolved {
    /// The plugin name (`<foo>` of `rackabel-<foo>`).
    name: String,
    /// How it was obtained (drives which pin is authoritative).
    kind: SourceKind,
    /// The absolute path to the executable INSIDE the store dir (the symlink target).
    exe_in_store: PathBuf,
    /// The commit pin (clone) — `Some` only for a built clone.
    commit: Option<String>,
    /// The sha256 pin (asset/tarball/path) — `Some` for everything except a clone.
    sha256: Option<String>,
    /// Whether a `rackabel-plugin.toml` was present (inert 0.5 metadata).
    has_manifest: bool,
    /// The inert hook names declared (0.5 metadata; never executed in 0.4).
    hooks: Vec<String>,
}

/// `rackabel plugin install <source> [--yes] [--force]`.
pub fn install(args: &PluginInstallArgs, ctx: &Ctx) -> CmdResult<()> {
    // Surface any upgrade-time collision (§5.6) before doing work — installing is a plugin
    // command, and a now-shadowed plugin must be announced loudly once.
    super::collision::check_and_warn(ctx, crate::cli::is_reserved);

    let source = PluginSource::parse(&args.source).ok_or_else(|| {
        RkError::of(
            ErrorCode::UsageError,
            format!("`{}` is not a valid install source", args.source),
            "use OWNER/REPO, a local path, or a .tgz tarball",
        )
        .at(args.source.clone())
    })?;

    // The plugin NAME we will install under. For a sideload we derive it from the
    // executable basename (`rackabel-<name>`); for OWNER/REPO it is the repo name.
    let name = derive_name(&source)?;

    // The pin check happens AFTER we resolve the bytes (we need the new sha/commit to
    // compare), but we read the existing entry now so the confirmation can say
    // "reinstall"/"upgrade" honestly.
    let mut lock = LockFile::load(ctx)?;
    let existing = lock.find(&name).cloned();

    // Announce + confirm (§5.7). A remote OWNER/REPO is gated behind consent; a sideload
    // prints what it will do but proceeds (local code the user already has).
    announce(ctx, &source, &name, existing.as_ref());
    if source.is_remote() {
        confirm_remote(ctx, &source)?;
    }

    // Resolve the bytes into the store.
    let store = plugin_store_dir(ctx, &name);
    let resolved = resolve_into_store(ctx, &source, &name, &store)?;

    // Pin enforcement (§5.4/§5.7): if an entry exists, the new bytes must match its pin —
    // unless --force, which announces a deliberate update past the pin.
    if let Some(prev) = &existing {
        enforce_pin(ctx, prev, &resolved, args.force)?;
    }

    // Symlink the resolved executable into the managed bin (the §5.1 first-searched dir).
    let bin = plugins_bin_dir(ctx);
    std::fs::create_dir_all(&bin).map_err(|e| io_err(&bin, e))?;
    let link = bin.join(exe_basename(&name));
    link_exe(&resolved.exe_in_store, &link)?;

    // Record (or upsert) the lock entry. A re-install at the same name preserves the
    // `enabled` flag UNLESS the code changed: per §5.7, changing a hook plugin's pinned
    // code disables it (new code never runs under old consent). For a plain (no-manifest)
    // plugin `enabled` is irrelevant to dispatch, but we keep the same rule uniformly.
    let code_changed = existing.as_ref().is_some_and(|p| {
        p.commit.as_deref() != resolved.commit.as_deref()
            || p.sha256.as_deref() != resolved.sha256.as_deref()
    });
    // Default-enabled policy (§5.4 vs §5.7 reconciled — see DEVIATIONS D-88):
    //   - a PLAIN tier-2 plugin (no rackabel-plugin.toml) is immediately USABLE, so a
    //     fresh install is `enabled = true` (the musician happy path; `disable` skips it);
    //   - a HOOK plugin (carries a manifest) installs `enabled = false` — enabling is the
    //     0.5 hook consent gate (§5.7), so hooks never run under a default-on flag.
    // A reinstall preserves the prior flag UNLESS the code changed, in which case the §5.7
    // rule applies: a hook plugin is disabled (new code never runs under old consent); a
    // plain plugin is re-enabled (it was usable; the new bytes are still usable).
    let enabled = match &existing {
        Some(prev) if !code_changed => prev.enabled,
        // code changed (or first install):
        _ if resolved.has_manifest => false,
        _ => true,
    };

    let entry = PluginLockEntry {
        name: name.clone(),
        source: resolved.kind,
        origin: source.display(),
        commit: resolved.commit.clone(),
        sha256: resolved.sha256.clone(),
        installed_at: now_rfc3339(),
        executable: link.clone(),
        has_plugin_manifest: resolved.has_manifest,
        hooks: resolved.hooks.clone(),
        enabled,
    };
    lock.upsert(entry);
    lock.save(ctx)?;

    report_installed(
        ctx,
        &name,
        &resolved,
        &link,
        existing.is_some(),
        code_changed,
    );
    Ok(())
}

/// Print what the install will fetch/run and where (§5.4/§5.7) — always, before any work.
fn announce(ctx: &Ctx, source: &PluginSource, name: &str, existing: Option<&PluginLockEntry>) {
    if !ctx.echo_on() {
        return;
    }
    let verb = if existing.is_some() {
        "reinstall"
    } else {
        "install"
    };
    let store = plugin_store_dir(ctx, name).display().to_string();
    let bin = plugins_bin_dir(ctx)
        .join(exe_basename(name))
        .display()
        .to_string();
    match source {
        PluginSource::Repo { owner, repo } => {
            ui::frame::emit(
                ui::frame::Symbol::Warn,
                &format!(
                    "about to {verb} plugin `{name}` from {owner}/{repo} (third-party code): \
                     rackabel will fetch its release asset (or clone + build it), run it on \
                     install, store it under {store}, and link {bin}"
                ),
                ctx,
            );
        }
        PluginSource::Path(p) => {
            ui::frame::emit(
                ui::frame::Symbol::Good,
                &format!(
                    "{verb}ing plugin `{name}` from {} (sideload): copying into {store} \
                     and linking {bin}",
                    p.display()
                ),
                ctx,
            );
        }
        PluginSource::Tarball(p) => {
            ui::frame::emit(
                ui::frame::Symbol::Good,
                &format!(
                    "{verb}ing plugin `{name}` from {} (sideloaded tarball): unpacking into \
                     {store} and linking {bin}",
                    p.display()
                ),
                ctx,
            );
        }
    }
}

/// The remote-install consent gate (§5.7). `--yes` scripts it; `--no-input` refuses with
/// `RK0403` (nothing fetched); an interactive decline is the same `RK0403`.
fn confirm_remote(ctx: &Ctx, source: &PluginSource) -> CmdResult<()> {
    if ctx.json {
        // A remote install needs a human consent decision; --json output can't carry one,
        // so it must be paired with --yes. Treat a bare --json like --no-input here.
        return Err(declined(
            source,
            "pass --yes to consent in a script (with --json)",
        ));
    }
    if yes_flag() {
        return Ok(());
    }
    if ctx.no_input {
        return Err(declined(
            source,
            "pass --yes to consent non-interactively (running with --no-input, so I won't prompt)",
        ));
    }
    // No interactive terminal (a CI runner / pipe without --no-input): we cannot obtain
    // consent for unreviewed remote code, so refuse cleanly with RK0403 rather than letting
    // the prompt fail with a generic "no TTY" usage error. --yes is the scripted consent.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(declined(
            source,
            "no interactive terminal to confirm in — pass --yes to consent in a script",
        ));
    }
    // Interactive: a real y/N confirmation, defaulting to NO (consent must be explicit).
    let ok = ui::prompt::confirm(
        &format!("Fetch, build, and run {} now?", source.display()),
        false,
        ctx,
    )?;
    if ok {
        Ok(())
    } else {
        Err(declined(source, "rerun and confirm, or pass --yes"))
    }
}

/// Read the transient `--yes` for the current install (set by [`set_yes`] from the command
/// entry point). A thread-local avoids widening the shared, frozen `Ctx` model for one flag.
fn yes_flag() -> bool {
    YES.with(|y| y.get())
}

thread_local! {
    /// Transient `--yes` for the current install (set by [`install`] before confirming).
    /// A thread-local avoids widening `Ctx` (a shared, frozen model) just for one flag.
    static YES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The `RK0403 TemplateFetchDeclined` frame, reused for a declined PLUGIN install (the
/// foundation froze this code's explain prose to cover `plugin install` consent; it is
/// environment-class, exit 3 — see DEVIATIONS D-87).
fn declined(source: &PluginSource, help: &str) -> RkError {
    RkError::of(
        ErrorCode::TemplateFetchDeclined,
        format!(
            "installing `{}` runs unreviewed third-party code and was not confirmed",
            source.display()
        ),
        help.to_string(),
    )
}

/// Derive the plugin name from the source. For a sideload, the executable basename must be
/// `rackabel-<name>` (a dir is searched for one such file). For OWNER/REPO it is the repo.
fn derive_name(source: &PluginSource) -> CmdResult<String> {
    match source {
        PluginSource::Repo { repo, .. } => {
            // A repo may be named `rackabel-foo` or just `foo`; strip the prefix if present.
            Ok(repo.strip_prefix("rackabel-").unwrap_or(repo).to_string())
        }
        PluginSource::Path(p) => name_from_path_source(p),
        PluginSource::Tarball(p) => {
            // Best-effort name from the tarball filename `rackabel-<name>-<...>.tgz`; the
            // real executable name is confirmed after unpack. We only need a store key.
            let stem = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("plugin")
                .trim_end_matches(".tar.gz")
                .trim_end_matches(".tgz");
            let core = stem.strip_prefix("rackabel-").unwrap_or(stem);
            // Cut at the first version-looking segment (`-<digit>` or `-v<digit>`).
            let name = core
                .split('-')
                .take_while(|seg| !starts_with_versionish(seg))
                .collect::<Vec<_>>()
                .join("-");
            Ok(if name.is_empty() {
                core.to_string()
            } else {
                name
            })
        }
    }
}

fn starts_with_versionish(seg: &str) -> bool {
    let s = seg.strip_prefix('v').unwrap_or(seg);
    s.chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
}

/// The plugin name from a sideloaded PATH: either the file is `rackabel-<name>`, or it is a
/// directory holding exactly one such file.
fn name_from_path_source(p: &Path) -> CmdResult<String> {
    if p.is_file() {
        return name_from_exe(p);
    }
    if p.is_dir() {
        let exe = find_exe_in_dir(p)?;
        return name_from_exe(&exe);
    }
    Err(RkError::of(
        ErrorCode::PluginNotFound,
        format!("nothing to install at `{}`", p.display()),
        "point at a `rackabel-<name>` executable, a directory containing one, or a .tgz tarball",
    )
    .at(p.display().to_string()))
}

fn name_from_exe(p: &Path) -> CmdResult<String> {
    let base = p.file_name().and_then(|s| s.to_str()).unwrap_or_default();
    base.strip_prefix("rackabel-")
        .map(|n| n.to_string())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| {
            RkError::of(
                ErrorCode::UsageError,
                format!("`{base}` is not a `rackabel-<name>` executable"),
                "a plugin executable must be named `rackabel-<name>` (the §5.1 convention)",
            )
            .at(p.display().to_string())
        })
}

/// Resolve the source's bytes into the per-name store dir, returning the executable path +
/// pins + inert manifest metadata.
fn resolve_into_store(
    ctx: &Ctx,
    source: &PluginSource,
    name: &str,
    store: &Path,
) -> CmdResult<Resolved> {
    // A clean store dir each time (an upgrade replaces the contents). Removing first keeps
    // a stale file from a prior version from masquerading as the new one.
    if store.exists() {
        std::fs::remove_dir_all(store).map_err(|e| io_err(store, e))?;
    }
    std::fs::create_dir_all(store).map_err(|e| io_err(store, e))?;

    match source {
        PluginSource::Path(p) => resolve_path(p, name, store),
        PluginSource::Tarball(p) => resolve_tarball(p, name, store),
        PluginSource::Repo { owner, repo } => resolve_repo(ctx, owner, repo, name, store),
    }
}

/// Sideload a local path: copy the executable (and, if a dir, the whole tree incl. an
/// optional `rackabel-plugin.toml`) into the store. Pin by sha256 of the executable.
fn resolve_path(p: &Path, name: &str, store: &Path) -> CmdResult<Resolved> {
    let exe_src = if p.is_file() {
        p.to_path_buf()
    } else {
        find_exe_in_dir(p)?
    };

    // If the source is a directory, copy the whole tree (so the manifest + any carried
    // scripts come along for 0.5); else copy just the executable.
    let exe_in_store = if p.is_dir() {
        copy_tree(p, store)?;
        store.join(exe_basename(name))
    } else {
        let dest = store.join(exe_basename(name));
        std::fs::copy(&exe_src, &dest).map_err(|e| io_err(&dest, e))?;
        dest
    };
    set_executable(&exe_in_store)?;

    let (has_manifest, hooks) = read_manifest(store)?;
    let sha = sha256::hash_file(&exe_in_store)?;
    Ok(Resolved {
        name: name.to_string(),
        kind: SourceKind::Path,
        exe_in_store,
        commit: None,
        sha256: Some(sha),
        has_manifest,
        hooks,
    })
}

/// Sideload a `.tgz`/`.tar.gz`: unpack into the store, find the `rackabel-<name>`
/// executable inside, pin by sha256 of that executable.
fn resolve_tarball(p: &Path, name: &str, store: &Path) -> CmdResult<Resolved> {
    let f = std::fs::File::open(p).map_err(|e| {
        RkError::of(
            ErrorCode::PluginNotFound,
            format!("could not open the tarball `{}`", p.display()),
            "check the path and that the file exists, then retry",
        )
        .at(p.display().to_string())
        .raw(e.into())
    })?;
    let gz = flate2::read::GzDecoder::new(f);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(store).map_err(|e| {
        RkError::of(
            ErrorCode::PluginNotFound,
            format!("could not unpack the tarball `{}`", p.display()),
            "ensure it is a valid .tgz/.tar.gz produced by the plugin's author",
        )
        .at(p.display().to_string())
        .raw(e.into())
    })?;

    // The executable is `rackabel-<name>` somewhere in the unpacked tree (commonly at the
    // root, or under a single top-level dir).
    let exe_in_store = find_exe_in_tree(store, name).ok_or_else(|| {
        RkError::of(
            ErrorCode::PluginNotFound,
            format!("the tarball did not contain a `rackabel-{name}` executable"),
            "the tarball must contain an executable named `rackabel-<name>` (§5.1)",
        )
        .at(p.display().to_string())
    })?;
    set_executable(&exe_in_store)?;

    // The manifest may sit next to the executable rather than at the store root.
    let exe_dir = exe_in_store.parent().unwrap_or(store);
    let (has_manifest, hooks) = read_manifest(exe_dir)?;
    let sha = sha256::hash_file(&exe_in_store)?;
    Ok(Resolved {
        name: name.to_string(),
        kind: SourceKind::Tarball,
        exe_in_store,
        commit: None,
        sha256: Some(sha),
        has_manifest,
        hooks,
    })
}

/// `OWNER/REPO`: prefer a release asset `rackabel-<name>-<os>-<arch>` (sha256-pinned via
/// the GitHub API), else clone the repo and build it (commit-pinned).
fn resolve_repo(
    ctx: &Ctx,
    owner: &str,
    repo: &str,
    name: &str,
    store: &Path,
) -> CmdResult<Resolved> {
    // 1) Try a release asset via the GitHub API seam.
    match try_release_asset(owner, repo, name, store) {
        Ok(Some(resolved)) => return Ok(resolved),
        Ok(None) => { /* no matching asset → fall through to clone+build */ }
        Err(e) => return Err(e), // a hard network/parse error is surfaced (RK0404 etc.)
    }

    // 2) Clone + build.
    resolve_clone_and_build(ctx, owner, repo, name, store)
}

/// Query the GitHub API for the latest release and download the asset
/// `rackabel-<name>-<os>-<arch>` if present. Returns `Ok(None)` when there is no such
/// asset (caller falls back to clone+build). Network failures are `RK0404`.
fn try_release_asset(
    owner: &str,
    repo: &str,
    name: &str,
    store: &Path,
) -> CmdResult<Option<Resolved>> {
    let api = super::source::github_api_base();
    let url = format!(
        "{}/repos/{owner}/{repo}/releases/latest",
        api.trim_end_matches('/')
    );
    let body = http_get_string(&url)?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| {
        RkError::of(
            ErrorCode::NoNetwork,
            "the GitHub release response could not be parsed",
            "retry shortly; if it persists, sideload a local path/tarball instead",
        )
        .at(url.clone())
        .raw(e.into())
    })?;

    let want = format!("rackabel-{name}-{}-{}", os_token(), arch_token());
    let assets = json.get("assets").and_then(|a| a.as_array());
    let Some(assets) = assets else {
        return Ok(None);
    };
    let Some(asset) = assets.iter().find(|a| {
        a.get("name")
            .and_then(|n| n.as_str())
            .map(|n| n == want)
            .unwrap_or(false)
    }) else {
        return Ok(None);
    };
    let dl = asset
        .get("browser_download_url")
        .and_then(|u| u.as_str())
        .map(rewrite_dl_host)
        .ok_or_else(|| {
            RkError::of(
                ErrorCode::NoNetwork,
                "the matching release asset has no download URL",
                "sideload a local path/tarball, or report it to the plugin's author",
            )
        })?;

    let bytes = http_get_bytes(&dl)?;
    let exe_in_store = store.join(exe_basename(name));
    std::fs::write(&exe_in_store, &bytes).map_err(|e| io_err(&exe_in_store, e))?;
    set_executable(&exe_in_store)?;
    let sha = sha256::hash_bytes(&bytes);
    Ok(Some(Resolved {
        name: name.to_string(),
        kind: SourceKind::Gh,
        exe_in_store,
        commit: None,
        sha256: Some(sha),
        has_manifest: false,
        hooks: Vec::new(),
    }))
}

/// Clone the repo (shallow) into the store and build it. A clone with no obvious build
/// (no produced `rackabel-<name>` and no recognized build manifest) is a clear frame.
fn resolve_clone_and_build(
    ctx: &Ctx,
    owner: &str,
    repo: &str,
    name: &str,
    store: &Path,
) -> CmdResult<Resolved> {
    let url = super::source::PluginSource::Repo {
        owner: owner.to_string(),
        repo: repo.to_string(),
    }
    .clone_url()
    .expect("Repo source yields a clone url");

    let checkout = store.join("src");
    git::clone_shallow(&url, None, &checkout, ErrorCode::PluginNotFound)?;
    let commit = git::rev_parse_head(&checkout, ErrorCode::PluginNotFound)?;

    // A prebuilt `rackabel-<name>` committed at the repo root needs no build step.
    if let Some(exe) = find_exe_in_tree(&checkout, name) {
        set_executable(&exe)?;
        let exe_dir = exe.parent().unwrap_or(&checkout);
        let (has_manifest, hooks) = read_manifest(exe_dir)?;
        return Ok(Resolved {
            name: name.to_string(),
            kind: SourceKind::Gh,
            exe_in_store: exe,
            commit: Some(commit),
            sha256: None,
            has_manifest,
            hooks,
        });
    }

    // Otherwise the repo must declare a build we recognize. 0.4 does not invent a build
    // toolchain: a clone with no prebuilt binary and no recognized build is a CLEAR frame
    // (not a silent success), pointing the user at sideloading the built artifact.
    let _ = ctx; // (build invocation will use ctx in a later milestone)
    Err(RkError::of(
        ErrorCode::PluginNotFound,
        format!(
            "cloned {owner}/{repo} at {} but found no `rackabel-{name}` to install and no \
             build rackabel knows how to run",
            short_commit(&commit)
        ),
        "ask the author to publish a release asset `rackabel-<name>-<os>-<arch>`, or build \
         the plugin yourself and `rackabel plugin install <path>` the resulting executable",
    )
    .at(checkout.display().to_string()))
}

/// Enforce the `plugins.lock` pin against freshly-resolved bytes/commit (§5.4/§5.7).
/// A mismatch is `RK4007` (exit 4) UNLESS `--force`, which announces a deliberate update.
fn enforce_pin(
    ctx: &Ctx,
    prev: &PluginLockEntry,
    resolved: &Resolved,
    force: bool,
) -> CmdResult<()> {
    let prev_pin = prev.pin();
    let new_pin = resolved.commit.as_deref().or(resolved.sha256.as_deref());
    let matches = matches!((prev_pin, new_pin), (Some(a), Some(b)) if a.eq_ignore_ascii_case(b));

    if matches {
        return Ok(()); // identical code — an idempotent reinstall.
    }

    if force {
        // A deliberate update past the pin: announce it loudly (never silent — §5.7).
        if ctx.echo_on() {
            ui::frame::emit(
                ui::frame::Symbol::Warn,
                &format!(
                    "--force: updating `{}` past its pin ({} -> {}); for a hook plugin this \
                     disables it until you re-`enable` (new code never runs under old consent)",
                    prev.name,
                    prev_pin
                        .map(short_commit)
                        .unwrap_or_else(|| "unpinned".into()),
                    new_pin
                        .map(short_commit)
                        .unwrap_or_else(|| "unpinned".into()),
                ),
                ctx,
            );
        }
        return Ok(());
    }

    Err(RkError::new(
        ErrorCode::PinMismatch,
        ExitClass::Validation,
        format!(
            "`{}` resolves to different code than its pin in plugins.lock",
            prev.name
        ),
        "re-run to fetch the pinned code, or pass --force to update past the pin \
         (it will announce the change)",
    )
    .at(format!(
        "pinned   {}\n  resolved {}",
        prev_pin.unwrap_or("unpinned"),
        new_pin.unwrap_or("unpinned"),
    )))
}

/// Verify an installed plugin's on-disk executable still matches its lockfile pin (§5.4).
/// A tamper (a modified file) is `RK4007` (exit 4). Only sha256-pinned entries are
/// byte-verifiable here; a commit-pinned clone is verified at fetch time.
pub fn verify_entry(entry: &PluginLockEntry) -> CmdResult<()> {
    if let Some(expected) = &entry.sha256 {
        // Verify the symlink TARGET (the store file), following the managed-bin symlink.
        let target = std::fs::canonicalize(&entry.executable).unwrap_or(entry.executable.clone());
        return sha256::verify_file(&target, expected);
    }
    Ok(())
}

/// Verify a managed (lock-recorded) plugin's on-disk bytes against its pin before running
/// it (§5.7 tamper check). A name not in the lock (an unmanaged `$PATH` plugin) has no pin
/// and passes silently — the user owns it. A modified installed file is `RK4007` (exit 4).
/// A lockfile READ error does not block dispatch (we'd rather run than crash on a state
/// hiccup); a real pin MISMATCH is surfaced.
pub fn verify_managed(ctx: &Ctx, name: &str) -> CmdResult<()> {
    let lock = match LockFile::load(ctx) {
        Ok(l) => l,
        Err(_) => return Ok(()),
    };
    match lock.find(name) {
        Some(entry) => verify_entry(entry),
        None => Ok(()),
    }
}

fn report_installed(
    ctx: &Ctx,
    name: &str,
    resolved: &Resolved,
    link: &Path,
    was_installed: bool,
    code_changed: bool,
) {
    if ctx.json {
        let obj = serde_json::json!({
            "installed": name,
            "source": format!("{:?}", resolved.kind).to_lowercase(),
            "commit": resolved.commit,
            "sha256": resolved.sha256,
            "executable": link.display().to_string(),
            "has_plugin_manifest": resolved.has_manifest,
            "hooks": resolved.hooks,
            "reinstalled": was_installed,
            "code_changed": code_changed,
        });
        println!("{}", serde_json::to_string_pretty(&obj).unwrap());
        return;
    }
    if !ctx.echo_on() {
        return;
    }
    let pin = resolved
        .commit
        .as_deref()
        .map(|c| format!("commit {}", short_commit(c)))
        .or_else(|| {
            resolved
                .sha256
                .as_deref()
                .map(|s| format!("sha256 {}", short_commit(s)))
        })
        .unwrap_or_else(|| "unpinned".to_string());
    let verb = if was_installed {
        "reinstalled"
    } else {
        "installed"
    };
    ui::frame::emit(
        ui::frame::Symbol::Good,
        &format!("{verb} `{name}` ({pin}) -> {}", link.display()),
        ctx,
    );
    if resolved.has_manifest {
        ui::frame::note(
            &format!(
                "carries a rackabel-plugin.toml with {} hook(s) — disabled (run `rackabel \
                 plugin enable {name}` to consent; hooks run in a later release)",
                resolved.hooks.len()
            ),
            ctx,
        );
    }
}

// --- small helpers --------------------------------------------------------------

/// Set the transient `--yes` for the current install. Called by the command entry point.
pub fn set_yes(yes: bool) {
    YES.with(|y| y.set(yes));
}

/// Whether `name` is installed as a MANAGED plugin AND currently disabled (§5.4 dispatch
/// gating). A name that is not in the lock (an unmanaged `$PATH` plugin) is never "disabled"
/// — the enable/disable flag only governs the managed copy. A lockfile read error is
/// treated as "not disabled" (we'd rather run than block on a state hiccup).
pub fn is_managed_disabled(ctx: &Ctx, name: &str) -> bool {
    LockFile::load(ctx)
        .ok()
        .and_then(|l| l.find(name).map(|e| !e.enabled))
        .unwrap_or(false)
}

/// Read the manifest at `dir` (inert 0.5 metadata): presence + sorted hook names.
fn read_manifest(dir: &Path) -> CmdResult<(bool, Vec<String>)> {
    match PluginManifest::load_from_dir(dir)? {
        Some(m) => Ok((true, m.hook_names())),
        None => Ok((false, Vec::new())),
    }
}

/// Find the single `rackabel-<name>` executable directly inside `dir`.
fn find_exe_in_dir(dir: &Path) -> CmdResult<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir)
        .map_err(|e| io_err(dir, e))?
        .flatten()
    {
        let p = entry.path();
        if p.is_file()
            && p.file_name()
                .and_then(|s| s.to_str())
                // A `rackabel-<name>` executable — but NOT the `rackabel-plugin.toml`
                // manifest (which shares the prefix but is metadata, not the executable).
                .map(|n| {
                    n.starts_with("rackabel-") && n != crate::plugin::manifest::PLUGIN_MANIFEST_NAME
                })
                .unwrap_or(false)
        {
            found.push(p);
        }
    }
    match found.len() {
        1 => Ok(found.pop().unwrap()),
        0 => Err(RkError::of(
            ErrorCode::PluginNotFound,
            format!("no `rackabel-<name>` executable in `{}`", dir.display()),
            "the directory must contain exactly one `rackabel-<name>` executable (§5.1)",
        )
        .at(dir.display().to_string())),
        _ => Err(RkError::of(
            ErrorCode::UsageError,
            format!(
                "multiple `rackabel-<name>` executables in `{}`",
                dir.display()
            ),
            "point at the specific executable instead of the directory",
        )
        .at(dir.display().to_string())),
    }
}

/// Find a `rackabel-<name>` anywhere in a tree (root first, then one level of subdirs —
/// the common single-top-level-dir tarball/clone layout).
fn find_exe_in_tree(root: &Path, name: &str) -> Option<PathBuf> {
    let want = exe_basename(name);
    let direct = root.join(&want);
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(root).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let cand = p.join(&want);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Recursively copy a directory tree into `dst` (created if needed).
fn copy_tree(src: &Path, dst: &Path) -> CmdResult<()> {
    std::fs::create_dir_all(dst).map_err(|e| io_err(dst, e))?;
    for entry in std::fs::read_dir(src)
        .map_err(|e| io_err(src, e))?
        .flatten()
    {
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| io_err(&to, e))?;
        }
    }
    Ok(())
}

/// Create (or replace) the managed-bin symlink to the store executable.
fn link_exe(target: &Path, link: &Path) -> CmdResult<()> {
    if link.exists() || link.symlink_metadata().is_ok() {
        std::fs::remove_file(link).map_err(|e| io_err(link, e))?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).map_err(|e| io_err(link, e))?;
    }
    #[cfg(not(unix))]
    {
        // No symlink on non-unix without privilege; copy as a fallback.
        std::fs::copy(target, link).map_err(|e| io_err(link, e))?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_executable(p: &Path) -> CmdResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(p)
        .map_err(|e| io_err(p, e))?
        .permissions();
    perms.set_mode(perms.mode() | 0o755);
    std::fs::set_permissions(p, perms).map_err(|e| io_err(p, e))
}

#[cfg(not(unix))]
fn set_executable(_p: &Path) -> CmdResult<()> {
    Ok(())
}

/// `os` token for the asset name (`rackabel-<name>-<os>-<arch>`). Matches Rust's
/// `std::env::consts::OS` mapped to the common release-asset spelling.
fn os_token() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

/// `arch` token for the asset name. Maps Rust's arch to the common spelling.
fn arch_token() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    }
}

/// Rewrite a release-asset download URL through the `RACKABEL_GITHUB_DL` seam so tests
/// fetch from a local stub host. Only the host prefix is swapped.
fn rewrite_dl_host(url: &str) -> String {
    let host = std::env::var(DL_HOST_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty());
    match host {
        Some(h) => {
            // Replace a leading https://github.com (or http://…) with the seam host,
            // preserving the path.
            if let Some(path) = url
                .strip_prefix("https://github.com")
                .or_else(|| url.strip_prefix("http://github.com"))
            {
                format!("{}{}", h.trim_end_matches('/'), path)
            } else {
                url.to_string()
            }
        }
        None => url.to_string(),
    }
}

fn http_get_string(url: &str) -> CmdResult<String> {
    http_get(url)?.into_string().map_err(|e| {
        RkError::of(
            ErrorCode::NoNetwork,
            "could not read the response body",
            "retry shortly, or sideload a local path/tarball instead",
        )
        .at(url.to_string())
        .raw(e.into())
    })
}

fn http_get_bytes(url: &str) -> CmdResult<Vec<u8>> {
    let resp = http_get(url)?;
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf).map_err(|e| {
        RkError::of(
            ErrorCode::NoNetwork,
            "could not download the release asset",
            "retry shortly, or sideload a local path/tarball instead",
        )
        .at(url.to_string())
        .raw(e.into())
    })?;
    Ok(buf)
}

/// A single HTTP GET behind the network seams, mapping every failure to `RK0404`
/// (no-network/rate-limit). A 403/429 is the GitHub rate-limit signal.
fn http_get(url: &str) -> CmdResult<ureq::Response> {
    match ureq::get(url)
        .set("User-Agent", "rackabel")
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(resp) => Ok(resp),
        Err(ureq::Error::Status(code, _)) if code == 403 || code == 429 => Err(RkError::of(
            ErrorCode::NoNetwork,
            "the GitHub API rate-limited the request",
            "wait a few minutes and retry, or set GITHUB_TOKEN for a higher limit",
        )
        .at(url.to_string())),
        Err(ureq::Error::Status(404, _)) => Err(RkError::of(
            ErrorCode::PluginNotFound,
            "no such repository or release on GitHub",
            "check the OWNER/REPO spelling, or sideload a local path/tarball",
        )
        .at(url.to_string())),
        Err(ureq::Error::Status(code, _)) => Err(RkError::of(
            ErrorCode::NoNetwork,
            format!("the GitHub request failed (HTTP {code})"),
            "retry shortly, or sideload a local path/tarball instead",
        )
        .at(url.to_string())),
        Err(e @ ureq::Error::Transport(_)) => Err(RkError::of(
            ErrorCode::NoNetwork,
            "could not reach the network",
            "check your connection and retry, or sideload a local path/tarball instead",
        )
        .at(url.to_string())
        .raw(e.into())),
    }
}

fn os_dl_default() -> &'static str {
    DEFAULT_DL_HOST
}

fn now_rfc3339() -> String {
    // A dependency-free RFC3339-ish timestamp in UTC. Seconds precision is plenty for an
    // install record; we avoid pulling chrono (the lock model documents it as a string).
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Convert epoch seconds to a civil date (UTC) with a small inline algorithm.
    let (y, mo, d, h, mi, s) = civil_from_epoch(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Epoch-seconds → (year, month, day, hour, min, sec) in UTC (Howard Hinnant's algorithm).
fn civil_from_epoch(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d, h, mi, s)
}

fn short_commit(s: &str) -> String {
    s.chars().take(12).collect()
}

fn io_err(path: &Path, e: std::io::Error) -> RkError {
    RkError::of(
        ErrorCode::PluginNotFound,
        "a filesystem operation failed during install",
        "check permissions on ~/.rackabel/plugins and retry",
    )
    .at(path.display().to_string())
    .raw(e.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_name_strips_prefix_and_version() {
        let r = PluginSource::Repo {
            owner: "acme".into(),
            repo: "rackabel-notarize".into(),
        };
        assert_eq!(derive_name(&r).unwrap(), "notarize");
        let r2 = PluginSource::Repo {
            owner: "acme".into(),
            repo: "notarize".into(),
        };
        assert_eq!(derive_name(&r2).unwrap(), "notarize");
        let tb = PluginSource::Tarball(PathBuf::from("/d/rackabel-notarize-1.2.0.tgz"));
        assert_eq!(derive_name(&tb).unwrap(), "notarize");
        let tb2 = PluginSource::Tarball(PathBuf::from("/d/rackabel-clip-renamer-v2.tar.gz"));
        assert_eq!(derive_name(&tb2).unwrap(), "clip-renamer");
    }

    #[test]
    fn name_from_exe_requires_prefix() {
        assert_eq!(name_from_exe(Path::new("/x/rackabel-foo")).unwrap(), "foo");
        assert!(name_from_exe(Path::new("/x/foo")).is_err());
    }

    #[test]
    fn os_arch_tokens_map_to_release_spelling() {
        // The function is deterministic for the building host; just assert it's non-empty
        // and uses the mapped spelling for the common cases.
        assert!(!os_token().is_empty());
        assert!(!arch_token().is_empty());
        assert_ne!(os_token(), "macos"); // mapped to darwin on mac; passthrough elsewhere
    }

    #[test]
    fn rewrite_dl_host_swaps_only_with_seam() {
        unsafe {
            std::env::remove_var(DL_HOST_ENV);
        }
        let u = "https://github.com/o/r/releases/download/v1/rackabel-foo-darwin-arm64";
        assert_eq!(rewrite_dl_host(u), u);
        unsafe {
            std::env::set_var(DL_HOST_ENV, "http://127.0.0.1:9");
        }
        assert_eq!(
            rewrite_dl_host(u),
            "http://127.0.0.1:9/o/r/releases/download/v1/rackabel-foo-darwin-arm64"
        );
        unsafe {
            std::env::remove_var(DL_HOST_ENV);
        }
        assert_eq!(os_dl_default(), DEFAULT_DL_HOST);
    }

    #[test]
    fn civil_from_epoch_known_vector() {
        // 2026-06-07T00:00:00Z = 1780531200 (sanity: a fixed known instant).
        // 2021-01-01T00:00:00Z = 1609459200
        let s = now_rfc3339();
        assert!(s.ends_with('Z') && s.len() == 20, "got {s}");
        let (y, mo, d, _, _, _) = civil_from_epoch(1_609_459_200);
        assert_eq!((y, mo, d), (2021, 1, 1));
    }

    #[test]
    fn enforce_pin_matches_force_and_mismatch() {
        let prev = PluginLockEntry {
            name: "foo".into(),
            source: SourceKind::Path,
            origin: "/x".into(),
            commit: None,
            sha256: Some("aa".into()),
            installed_at: "t".into(),
            executable: PathBuf::from("/l"),
            has_plugin_manifest: false,
            hooks: vec![],
            enabled: true,
        };
        let same = Resolved {
            name: "foo".into(),
            kind: SourceKind::Path,
            exe_in_store: PathBuf::from("/s"),
            commit: None,
            sha256: Some("AA".into()), // case-insensitive match
            has_manifest: false,
            hooks: vec![],
        };
        let ctx = test_ctx();
        assert!(enforce_pin(&ctx, &prev, &same, false).is_ok());
        let diff = Resolved {
            sha256: Some("bb".into()),
            ..same
        };
        // Mismatch without --force is RK4007 (exit 4).
        let err = enforce_pin(&ctx, &prev, &diff, false).unwrap_err();
        assert_eq!(err.code, ErrorCode::PinMismatch);
        assert_eq!(err.class, ExitClass::Validation);
        // With --force it is allowed (announced).
        assert!(enforce_pin(&ctx, &prev, &diff, true).is_ok());
    }

    fn test_ctx() -> Ctx {
        crate::context::Ctx {
            no_input: true,
            json: false,
            quiet: true, // suppress the announce in the unit test
            verbose: false,
            raw: false,
            color: crate::ui::color::ColorMode::Never,
            color_err: crate::ui::color::ColorMode::Never,
            cwd: PathBuf::from("/"),
            rackabel_home: PathBuf::from("/tmp/.rackabel"),
            home: PathBuf::from("/tmp"),
            ableton_app: None,
            ableton_user_library: None,
            ableton_eh_mod: None,
            ableton_eh_node: None,
            ableton_extensions_dir: None,
            ableton_storage_base: None,
            rackabel_host_cmd: None,
        }
    }
}
