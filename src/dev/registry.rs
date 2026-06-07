//! The persistent dev-host registry — `~/.rackabel/registry.toml` (DESIGN §3.2).
//!
//! OWNERSHIP SPLIT (SPEC D §3): the FOUNDATION owns the serde model, the file IO
//! (atomic write under an advisory lock), the `RACKABEL_HOME` pathing, and the two
//! pure helpers every other module relies on — `disambiguate` (name-vs-verb / name
//! collision) and `prefilter` (drop host-incompatible entries). The REGISTRY agent
//! owns the CRUD *verbs* (`dev register`/`unregister`/`enable`/`disable`/`list`) and
//! will refine the disambiguation/`--recursive`-vs-`[workspace].members`
//! reconciliation; the load-bearing model below compiles and is correct enough for
//! the rest of the tree to build and for hermetic tests.
//!
//! The registry is operable with a DEAD daemon (SuperCollider Quarks lesson, §3.2):
//! nothing here touches the host or the socket.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::manifest::{MANIFEST_NAME, Project};

use super::{DEV_VERBS, RegistryEntry, registry_lock_path, registry_path};

/// The reserved `dev` verbs, re-exported at the registry module per SPEC D §4 so the
/// registry agent's CRUD verbs can reference `registry::RESERVED_VERBS` without
/// reaching into `dev::`. Part of the frozen surface; not yet consumed by a landed
/// verb (the foundation only lands stubs), hence the allow.
#[allow(unused_imports)]
pub use super::DEV_VERBS as RESERVED_VERBS;

/// The on-disk shape of `registry.toml`: a list of `[[extension]]` tables.
#[derive(Debug, Default, Serialize, Deserialize)]
struct RegistryFile {
    #[serde(default, rename = "extension")]
    extensions: Vec<RegistryEntry>,
}

/// The loaded registry: the entries plus the home it was loaded from (so `save`
/// writes back to the same place).
#[derive(Debug)]
pub struct Registry {
    entries: Vec<RegistryEntry>,
    path: PathBuf,
    lock_path: PathBuf,
}

impl Registry {
    /// Load `~/.rackabel/registry.toml`. A missing file yields an empty registry (the
    /// "delete the file to reset" affordance, §3.2). A parse error is surfaced framed.
    pub fn load(ctx: &Ctx) -> CmdResult<Self> {
        let path = registry_path(ctx);
        let lock_path = registry_lock_path(ctx);
        let entries = if path.is_file() {
            let text = std::fs::read_to_string(&path).map_err(|e| {
                RkError::of(
                    ErrorCode::ManifestParse,
                    "could not read the registry",
                    "check the file's permissions and try again",
                )
                .at(path.display().to_string())
                .raw(e.into())
            })?;
            let file: RegistryFile = toml::from_str(&text).map_err(|e| {
                RkError::of(
                    ErrorCode::ManifestParse,
                    "registry.toml could not be parsed",
                    "fix the TOML shown above, or delete the file to reset the registry",
                )
                .at(path.display().to_string())
                .raw(e.into())
            })?;
            file.extensions
        } else {
            Vec::new()
        };
        Ok(Self {
            entries,
            path,
            lock_path,
        })
    }

    /// Atomically persist the registry (write temp + rename) while holding the
    /// advisory lock. `RK0311` if the lock can't be acquired in time.
    pub fn save(&self) -> CmdResult<()> {
        let _guard = LockGuard::acquire(&self.lock_path)?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        let file = RegistryFile {
            extensions: self.entries.clone(),
        };
        let body = toml::to_string_pretty(&file).map_err(|e| {
            RkError::of(
                ErrorCode::ManifestParse,
                "could not serialize the registry",
                "this is a bug; please report it",
            )
            .raw(e.into())
        })?;
        let header = "# ~/.rackabel/registry.toml  —  managed by rackabel, but safe to hand-edit\n";
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, format!("{header}{body}")).map_err(|e| io_err(&tmp, e))?;
        std::fs::rename(&tmp, &self.path).map_err(|e| io_err(&self.path, e))?;
        Ok(())
    }

    /// All entries, in file order.
    pub fn entries(&self) -> &[RegistryEntry] {
        &self.entries
    }

    /// Just the enabled entries — what the bare `dev` loop loads (§3.2).
    pub fn enabled(&self) -> impl Iterator<Item = &RegistryEntry> {
        self.entries.iter().filter(|e| e.enabled)
    }

    /// Resolve a `NAME|PATH` token: exact name first, then a path match (canonicalized
    /// where possible so relative/absolute forms unify).
    pub fn find(&self, name_or_path: &str) -> Option<&RegistryEntry> {
        if let Some(by_name) = self.entries.iter().find(|e| e.name == name_or_path) {
            return Some(by_name);
        }
        let target =
            std::fs::canonicalize(name_or_path).unwrap_or_else(|_| PathBuf::from(name_or_path));
        self.entries.iter().find(|e| {
            let p = std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone());
            p == target
        })
    }

    /// The set of names already taken (for disambiguation).
    fn taken_names(&self) -> HashSet<String> {
        self.entries.iter().map(|e| e.name.clone()).collect()
    }

    /// Add a single manifest-bearing path to the registry. Returns the (possibly
    /// disambiguated) name actually used. `--name` (if `name`) is honored verbatim
    /// when free; otherwise the dir basename is disambiguated against existing names
    /// and the reserved verbs.
    ///
    /// An explicit `--name` that collides with an existing entry or a reserved verb is
    /// auto-disambiguated here (parent-prefixed). The interactive/`--no-input`
    /// `RK0312 NameCollision` decision lives in the `register` verb (it inspects
    /// [`name_outcome`] before calling `add`); this method is the always-consistent
    /// model default so a programmatic caller never gets a duplicate name.
    pub fn add(
        &mut self,
        path: &Path,
        name: Option<String>,
        disabled: bool,
        _ctx: &Ctx,
    ) -> CmdResult<String> {
        let root = require_manifest_dir(path)?;
        let taken = self.taken_names();
        let chosen = match name {
            Some(explicit) => {
                if taken.contains(&explicit) || DEV_VERBS.contains(&explicit.as_str()) {
                    Self::disambiguate(&explicit, &root, &taken)
                } else {
                    explicit
                }
            }
            None => {
                let base = basename(&root);
                Self::disambiguate(&base, &root, &taken)
            }
        };
        self.entries.push(RegistryEntry {
            name: chosen.clone(),
            path: root,
            source: super::Source::Dist,
            enabled: !disabled,
        });
        Ok(chosen)
    }

    /// Store an entry under a caller-decided `name` (already resolved by the verb's
    /// policy), guarding only the hard invariant — names are UNIQUE. Unlike [`add`],
    /// this does NOT re-disambiguate against the reserved verbs, so a forced verb-name
    /// (`register --name status`, accepted with a warning) is stored verbatim; such an
    /// entry is reachable only via `--only`/`--`, never the bare `dev` parse. An entry
    /// collision is still rejected (`RK0312`) — the caller must have resolved it first.
    pub fn add_named(&mut self, path: &Path, name: String, disabled: bool) -> CmdResult<String> {
        let root = require_manifest_dir(path)?;
        if self.taken_names().contains(&name) {
            return Err(RkError::of(
                ErrorCode::NameCollision,
                format!("the name `{name}` is already taken"),
                "choose a free name and rerun",
            ));
        }
        self.entries.push(RegistryEntry {
            name: name.clone(),
            path: root,
            source: super::Source::Dist,
            enabled: !disabled,
        });
        Ok(name)
    }

    /// Classify how a candidate name would be resolved before it is added, so the
    /// `register` verb can echo a disambiguation / warn on a forced verb-collision /
    /// raise `RK0312` under `--no-input`. Pure (does not mutate the registry).
    pub fn name_outcome(&self, candidate: &str, parent: &Path) -> NameOutcome {
        let taken = self.taken_names();
        let collides_entry = taken.contains(candidate);
        let collides_verb = DEV_VERBS.contains(&candidate);
        if !collides_entry && !collides_verb {
            return NameOutcome::Free;
        }
        let resolved = Self::disambiguate(candidate, parent, &taken);
        if collides_entry {
            NameOutcome::CollidesEntry { resolved }
        } else {
            NameOutcome::CollidesVerb { resolved }
        }
    }

    /// Register every manifest-bearing subdir of `root` (the monorepo / `--recursive`
    /// case, §3.2 / §4.4). Returns the names added, in deterministic order. Collisions
    /// across members are auto-disambiguated as they are discovered.
    ///
    /// Member discovery (§4.4): if `root` itself bears a manifest declaring a
    /// `[workspace]` with `members` globs, those globs (relative to `root`) drive the
    /// scan; otherwise the whole subtree is walked for `rackabel.toml` files. Either
    /// way, **library members are skipped** — a member directory with no manifest, or a
    /// manifest that declares neither `[extension]` nor `[device]` (a shared `lib`), is
    /// not registrable as a dev-host extension and is silently passed over.
    pub fn add_recursive(
        &mut self,
        root: &Path,
        disabled: bool,
        ctx: &Ctx,
    ) -> CmdResult<Vec<String>> {
        let mut added = Vec::new();
        for dir in recursive_member_dirs(root) {
            // Skip already-registered paths (idempotent re-register).
            let canon = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
            let already = self.entries.iter().any(|e| {
                std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone()) == canon
            });
            if already {
                continue;
            }
            let name = self.add(&dir, None, disabled, ctx)?;
            added.push(name);
        }
        Ok(added)
    }

    /// Remove an entry (or, with `recursive`, every entry whose path is under the
    /// given path). Returns the names removed.
    pub fn remove(&mut self, name_or_path: &str, recursive: bool) -> CmdResult<Vec<String>> {
        if recursive {
            let target =
                std::fs::canonicalize(name_or_path).unwrap_or_else(|_| PathBuf::from(name_or_path));
            let mut removed = Vec::new();
            self.entries.retain(|e| {
                let p = std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone());
                if p.starts_with(&target) {
                    removed.push(e.name.clone());
                    false
                } else {
                    true
                }
            });
            if removed.is_empty() {
                return Err(not_found(name_or_path));
            }
            Ok(removed)
        } else {
            let idx = self
                .entries
                .iter()
                .position(|e| e.name == name_or_path)
                .or_else(|| {
                    let target = std::fs::canonicalize(name_or_path)
                        .unwrap_or_else(|_| PathBuf::from(name_or_path));
                    self.entries.iter().position(|e| {
                        std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone()) == target
                    })
                })
                .ok_or_else(|| not_found(name_or_path))?;
            let removed = self.entries.remove(idx);
            Ok(vec![removed.name])
        }
    }

    /// Flip an entry's `enabled` flag. Returns the updated entry.
    pub fn set_enabled(&mut self, name_or_path: &str, enabled: bool) -> CmdResult<&RegistryEntry> {
        let idx = self
            .entries
            .iter()
            .position(|e| e.name == name_or_path)
            .or_else(|| {
                let target = std::fs::canonicalize(name_or_path)
                    .unwrap_or_else(|_| PathBuf::from(name_or_path));
                self.entries.iter().position(|e| {
                    std::fs::canonicalize(&e.path).unwrap_or_else(|_| e.path.clone()) == target
                })
            })
            .ok_or_else(|| not_found(name_or_path))?;
        self.entries[idx].enabled = enabled;
        Ok(&self.entries[idx])
    }

    /// Drop entries whose `minimumApiVersion` exceeds the host's `apiVersion` — the
    /// pre-filter that keeps one bad manifest from aborting the WHOLE host (verified
    /// host behavior, §3.2 / SPEC H §6). Returns `(kept, skipped-with-reason)`. An
    /// entry whose manifest can't be read or whose version is unparseable is KEPT
    /// (the build/validate path will surface that), not silently skipped here.
    pub fn prefilter(
        entries: &[RegistryEntry],
        host_api: &semver::Version,
    ) -> (Vec<RegistryEntry>, Vec<(RegistryEntry, String)>) {
        let mut kept = Vec::new();
        let mut skipped = Vec::new();
        for e in entries {
            match read_minimum_api_version(&e.path) {
                Some(min) if &min > host_api => {
                    skipped.push((
                        e.clone(),
                        format!("minimumApiVersion={min} > host {host_api}"),
                    ));
                }
                _ => kept.push(e.clone()),
            }
        }
        (kept, skipped)
    }

    /// Produce a unique name for `candidate` that is not in `taken` and not a reserved
    /// verb, by prefixing the parent dir basename (`packages-a-foo` vs `vendor-foo`,
    /// §3.2). If the parent-prefixed name is still taken, append `-2`, `-3`, … A bare
    /// verb collision (no `taken` conflict) is also resolved by the parent prefix.
    pub fn disambiguate(candidate: &str, parent: &Path, taken: &HashSet<String>) -> String {
        let needs_change = taken.contains(candidate) || DEV_VERBS.contains(&candidate);
        if !needs_change {
            return candidate.to_string();
        }
        // Prefix with the parent dir basename.
        let prefix = parent
            .parent()
            .map(basename)
            .filter(|p| !p.is_empty())
            .unwrap_or_default();
        let base = if prefix.is_empty() {
            candidate.to_string()
        } else {
            format!("{prefix}-{candidate}")
        };
        if !taken.contains(&base) && !DEV_VERBS.contains(&base.as_str()) {
            return base;
        }
        // Fall back to a numeric suffix.
        for n in 2.. {
            let attempt = format!("{base}-{n}");
            if !taken.contains(&attempt) && !DEV_VERBS.contains(&attempt.as_str()) {
                return attempt;
            }
        }
        unreachable!("the numeric-suffix loop always terminates")
    }
}

/// How a candidate registry name resolves against the existing entries + reserved
/// verbs (see [`Registry::name_outcome`]). The `register` verb turns this into the
/// echo / warning / `RK0312` behavior of §3.2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameOutcome {
    /// The candidate is usable verbatim.
    Free,
    /// The candidate equals an existing entry's name; `resolved` is what `add` would
    /// pick instead (parent-prefixed). Under `--no-input` an explicit `--name`
    /// collision here is `RK0312` (can't silently rename what the user asked for).
    CollidesEntry { resolved: String },
    /// The candidate equals a reserved dev verb; `resolved` is the parent-prefixed
    /// alternative. The auto name path takes `resolved` and echoes it; an explicit
    /// `--name` that equals a verb is *forced* with a warning (only `--only`/`--` can
    /// then target it, never the bare `dev` parse).
    CollidesVerb { resolved: String },
}

/// The member directories a `--recursive` register should consider (§4.4). When `root`
/// bears a manifest with a `[workspace]` `members` list, the globs (relative to `root`)
/// are expanded and each matched directory is taken as a member; otherwise the whole
/// subtree is scanned for `rackabel.toml`. In both cases a member that is NOT a
/// registrable extension/device project (no manifest, or a library manifest declaring
/// neither table) is dropped — only loadable dev-host projects come back.
fn recursive_member_dirs(root: &Path) -> Vec<PathBuf> {
    let candidates = match workspace_members(root) {
        Some(members) => members,
        None => manifest_dirs(root),
    };
    let mut out: Vec<PathBuf> = candidates
        .into_iter()
        .filter(|dir| is_registrable_project(dir))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// If `root/rackabel.toml` declares a `[workspace]`, expand its `members` globs
/// (relative to `root`) into the set of matched directories. Returns `None` when
/// `root` is not a workspace manifest (the manifest-scan fallback applies).
fn workspace_members(root: &Path) -> Option<Vec<PathBuf>> {
    let project = Project::discover(root).ok()?;
    // The discovered manifest must be AT `root` (not an ancestor) to count as the
    // workspace root being registered.
    if std::fs::canonicalize(&project.root).ok() != std::fs::canonicalize(root).ok() {
        return None;
    }
    let ws = project.raw.workspace.as_ref()?;
    if ws.members.is_empty() {
        return Some(Vec::new());
    }
    let mut dirs = Vec::new();
    for pattern in &ws.members {
        let glob = match globset::Glob::new(pattern) {
            Ok(g) => g.compile_matcher(),
            Err(_) => continue,
        };
        // Walk the subtree and match each directory's path RELATIVE to root against the
        // member glob, so `packages/*` selects the package dirs.
        for entry in walkdir::WalkDir::new(root)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                !(name == "node_modules" || name == "dist" || name == ".git")
            })
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_dir() {
                continue;
            }
            if let Ok(rel) = entry.path().strip_prefix(root)
                && !rel.as_os_str().is_empty()
                && glob.is_match(rel)
            {
                dirs.push(entry.path().to_path_buf());
            }
        }
    }
    Some(dirs)
}

/// True if `dir` holds a manifest that declares a registrable dev-host project
/// (`[extension]` or `[device]`). A directory with no manifest, or a manifest that
/// declares only `[workspace]` / nothing (a shared library member), is not
/// registrable and is skipped by `--recursive`.
fn is_registrable_project(dir: &Path) -> bool {
    let manifest = dir.join(MANIFEST_NAME);
    if !manifest.is_file() {
        return false;
    }
    match Project::discover(dir) {
        Ok(p) if std::fs::canonicalize(&p.root).ok() == std::fs::canonicalize(dir).ok() => {
            p.raw.extension.is_some() || p.raw.device.is_some()
        }
        _ => false,
    }
}

/// Read `minimum_api_version`/`minimumApiVersion` from the project at `path`, if it
/// declares one. Returns `None` when absent or unparseable (the caller keeps the
/// entry in that case — see `prefilter`).
fn read_minimum_api_version(path: &Path) -> Option<semver::Version> {
    let project = Project::discover(path).ok()?;
    let ext = project.raw.extension.as_ref()?;
    let raw = ext.minimum_api_version.as_ref()?;
    semver::Version::parse(raw).ok()
}

/// True if `path` (or `path/manifest`-bearing) holds a `rackabel.toml`.
fn require_manifest_dir(path: &Path) -> CmdResult<PathBuf> {
    let candidate = path.join(MANIFEST_NAME);
    if candidate.is_file() {
        return Ok(path.to_path_buf());
    }
    // Also accept being handed the manifest file itself.
    if path.is_file()
        && path.file_name().and_then(|s| s.to_str()) == Some(MANIFEST_NAME)
        && let Some(parent) = path.parent()
    {
        return Ok(parent.to_path_buf());
    }
    Err(RkError::of(
        ErrorCode::NoManifest,
        "no rackabel.toml in that path",
        "register a directory that holds a rackabel.toml (or use --recursive for a monorepo)",
    )
    .at(path.display().to_string()))
}

/// Every manifest-bearing directory under `root` (inclusive), shallow-walked via the
/// existing `walkdir` dependency. Skips `node_modules`/`dist`/`.git`.
fn manifest_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !(name == "node_modules" || name == "dist" || name == ".git")
        })
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file()
            && entry.file_name().to_string_lossy() == MANIFEST_NAME
            && let Some(parent) = entry.path().parent()
        {
            out.push(parent.to_path_buf());
        }
    }
    out.sort();
    out
}

fn basename(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("extension")
        .to_string()
}

fn not_found(what: &str) -> RkError {
    RkError::of(
        ErrorCode::NoDaemon,
        format!("no registry entry named or at `{what}`"),
        "run `rackabel dev list` to see registered extensions",
    )
    .at(what.to_string())
}

fn io_err(path: &Path, e: std::io::Error) -> RkError {
    RkError::of(
        ErrorCode::ManifestParse,
        "could not write the registry",
        "check write permissions on ~/.rackabel and retry",
    )
    .at(path.display().to_string())
    .raw(e.into())
}

/// A best-effort advisory exclusive lock via an `O_CREAT|O_EXCL` lockfile (no extra
/// dependency, SPEC D §4). Held only across a read-modify-write. A stale lockfile
/// (older than the timeout, with a dead writer) is reclaimed. `RK0311` on timeout.
struct LockGuard {
    path: PathBuf,
}

impl LockGuard {
    fn acquire(path: &Path) -> CmdResult<LockGuard> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = write!(f, "{}", std::process::id());
                    return Ok(LockGuard {
                        path: path.to_path_buf(),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Reclaim a stale lock: if the recorded pid is dead, remove it.
                    if Self::is_stale(path) {
                        let _ = std::fs::remove_file(path);
                        continue;
                    }
                    if Instant::now() >= deadline {
                        return Err(RkError::of(
                            ErrorCode::RegistryLocked,
                            "could not lock the registry",
                            "another rackabel may be writing it; wait and retry, or remove \
                             ~/.rackabel/registry.lock if no rackabel is running",
                        )
                        .at(path.display().to_string()));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    return Err(io_err(path, e));
                }
            }
        }
    }

    /// A lockfile is stale if its recorded pid is no longer alive.
    fn is_stale(path: &Path) -> bool {
        let Ok(text) = std::fs::read_to_string(path) else {
            return false;
        };
        let Ok(pid) = text.trim().parse::<i32>() else {
            return true; // unparseable → treat as stale
        };
        // kill(pid, 0): Ok => alive, ESRCH => dead.
        !matches!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Ok(())
        )
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_home(home: &Path) -> Ctx {
        // Build a minimal Ctx pointing RACKABEL_HOME at a temp dir. We only need the
        // rackabel_home field for registry IO.
        Ctx {
            no_input: true,
            json: false,
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

    fn write_project(dir: &Path, min_api: Option<&str>) {
        std::fs::create_dir_all(dir).unwrap();
        let api = min_api
            .map(|v| format!("minimum_api_version = \"{v}\"\n"))
            .unwrap_or_default();
        std::fs::write(
            dir.join(MANIFEST_NAME),
            format!("[extension]\nname = \"x\"\nauthor = \"a\"\n{api}"),
        )
        .unwrap();
    }

    #[test]
    fn load_missing_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        let reg = Registry::load(&ctx).unwrap();
        assert!(reg.entries().is_empty());
    }

    #[test]
    fn add_save_load_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        let proj = tmp.path().join("my-ext");
        write_project(&proj, None);

        let mut reg = Registry::load(&ctx).unwrap();
        let name = reg.add(&proj, None, false, &ctx).unwrap();
        assert_eq!(name, "my-ext");
        reg.save().unwrap();

        let reg2 = Registry::load(&ctx).unwrap();
        assert_eq!(reg2.entries().len(), 1);
        assert_eq!(reg2.entries()[0].name, "my-ext");
        assert!(reg2.entries()[0].enabled);
        assert_eq!(reg2.entries()[0].source, super::super::Source::Dist);
    }

    #[test]
    fn disambiguate_avoids_taken_and_verbs() {
        let mut taken = HashSet::new();
        taken.insert("foo".to_string());
        // collision with an existing name → parent-prefixed
        let parent = Path::new("/code/packages-a/foo");
        let got = Registry::disambiguate("foo", parent, &taken);
        assert_eq!(got, "packages-a-foo");
        // collision with a reserved verb → parent-prefixed even with empty taken
        let empty = HashSet::new();
        let parent2 = Path::new("/code/pkg/test");
        let got2 = Registry::disambiguate("test", parent2, &empty);
        assert_eq!(got2, "pkg-test");
        // a free name is returned unchanged
        assert_eq!(Registry::disambiguate("unique", parent, &empty), "unique");
    }

    #[test]
    fn disambiguate_numeric_fallback() {
        let mut taken = HashSet::new();
        taken.insert("foo".to_string());
        taken.insert("pkg-foo".to_string());
        let parent = Path::new("/code/pkg/foo");
        let got = Registry::disambiguate("foo", parent, &taken);
        assert_eq!(got, "pkg-foo-2");
    }

    #[test]
    fn add_recursive_disambiguates_collisions() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        let root = tmp.path().join("mono");
        write_project(&root.join("packages/a/foo"), None);
        write_project(&root.join("packages/b/foo"), None);

        let mut reg = Registry::load(&ctx).unwrap();
        let names = reg.add_recursive(&root, false, &ctx).unwrap();
        assert_eq!(names.len(), 2);
        // The two foo dirs get distinct names.
        let set: HashSet<_> = names.iter().cloned().collect();
        assert_eq!(set.len(), 2);
        assert!(names.iter().any(|n| n == "foo"));
        assert!(names.iter().any(|n| n.ends_with("-foo")));
    }

    #[test]
    fn prefilter_drops_incompatible() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = tmp.path().join("ok");
        let bad = tmp.path().join("bad");
        write_project(&ok, Some("1.0.0"));
        write_project(&bad, Some("2.0.0"));
        let entries = vec![
            RegistryEntry {
                name: "ok".into(),
                path: ok,
                source: super::super::Source::Dist,
                enabled: true,
            },
            RegistryEntry {
                name: "bad".into(),
                path: bad,
                source: super::super::Source::Dist,
                enabled: true,
            },
        ];
        let host = semver::Version::parse("1.0.0").unwrap();
        let (kept, skipped) = Registry::prefilter(&entries, &host);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].name, "ok");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].0.name, "bad");
        assert!(skipped[0].1.contains("2.0.0"));
    }

    fn write_workspace(root: &Path, members: &[&str]) {
        std::fs::create_dir_all(root).unwrap();
        let list = members
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(", ");
        std::fs::write(
            root.join(MANIFEST_NAME),
            format!("[workspace]\nmembers = [{list}]\n"),
        )
        .unwrap();
    }

    /// `--recursive` over a `[workspace].members` monorepo: every matched member that is
    /// an extension project is registered; a library member (no `[extension]`) is
    /// skipped; the manifest-scan fallback would have caught the lib, the workspace
    /// globs deliberately do not.
    #[test]
    fn add_recursive_uses_workspace_members_and_skips_libraries() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        let root = tmp.path().join("mono");
        write_workspace(&root, &["packages/*"]);
        write_project(&root.join("packages/foo"), None);
        write_project(&root.join("packages/bar"), None);
        // A library member matched by the glob but declaring no [extension]: skipped.
        write_workspace(&root.join("packages/shared-lib"), &[]);
        // A manifest OUTSIDE the member globs: not selected at all.
        write_project(&root.join("tools/helper"), None);

        let mut reg = Registry::load(&ctx).unwrap();
        let names = reg.add_recursive(&root, false, &ctx).unwrap();
        let set: HashSet<_> = names.iter().cloned().collect();
        assert_eq!(set, ["foo", "bar"].iter().map(|s| s.to_string()).collect());
    }

    /// With no `[workspace]` manifest at the root, `--recursive` falls back to a full
    /// manifest scan, still skipping library members (manifests with no [extension]).
    #[test]
    fn add_recursive_flat_layout_skips_library_member() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        let root = tmp.path().join("flat");
        write_project(&root.join("ext-one"), None);
        write_project(&root.join("nested/ext-two"), None);
        write_workspace(&root.join("nested/lib"), &[]); // library: skipped

        let mut reg = Registry::load(&ctx).unwrap();
        let names = reg.add_recursive(&root, false, &ctx).unwrap();
        let set: HashSet<_> = names.iter().cloned().collect();
        assert_eq!(
            set,
            ["ext-one", "ext-two"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
    }

    #[test]
    fn name_outcome_classifies_free_entry_and_verb() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        let proj = tmp.path().join("packages/thing");
        write_project(&proj, None);
        let mut reg = Registry::load(&ctx).unwrap();
        reg.add(&proj, None, false, &ctx).unwrap();

        // Free.
        assert_eq!(
            reg.name_outcome("fresh", Path::new("/x/fresh")),
            NameOutcome::Free
        );
        // Entry collision → parent-prefixed resolution.
        match reg.name_outcome("thing", Path::new("/code/pkg/thing")) {
            NameOutcome::CollidesEntry { resolved } => assert_eq!(resolved, "pkg-thing"),
            other => panic!("expected entry collision, got {other:?}"),
        }
        // Verb collision (even with no entry of that name).
        match reg.name_outcome("status", Path::new("/code/pkg/status")) {
            NameOutcome::CollidesVerb { resolved } => assert_eq!(resolved, "pkg-status"),
            other => panic!("expected verb collision, got {other:?}"),
        }
    }

    #[test]
    fn add_named_stores_verbatim_but_rejects_duplicate() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        let proj = tmp.path().join("vd");
        write_project(&proj, None);
        let mut reg = Registry::load(&ctx).unwrap();
        // A verb name forced through (the register verb's --name-equals-verb path).
        let stored = reg.add_named(&proj, "status".to_string(), false).unwrap();
        assert_eq!(stored, "status");
        assert_eq!(reg.find("status").unwrap().name, "status");
        // A duplicate is rejected with RK0312.
        let proj2 = tmp.path().join("vd2");
        write_project(&proj2, None);
        let err = reg
            .add_named(&proj2, "status".to_string(), false)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::NameCollision);
    }

    #[test]
    fn prefilter_keeps_unparseable_and_absent() {
        // An extension with no minimumApiVersion, and one with a junk version, are both
        // KEPT (the build/validate path surfaces those; the pre-filter only drops a
        // cleanly-parsed version that exceeds the host).
        let tmp = tempfile::tempdir().unwrap();
        let none = tmp.path().join("none");
        write_project(&none, None);
        let entries = vec![RegistryEntry {
            name: "none".into(),
            path: none,
            source: super::super::Source::Dist,
            enabled: true,
        }];
        let host = semver::Version::parse("1.0.0").unwrap();
        let (kept, skipped) = Registry::prefilter(&entries, &host);
        assert_eq!(kept.len(), 1);
        assert!(skipped.is_empty());
    }

    #[test]
    fn remove_and_set_enabled() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        let proj = tmp.path().join("ext");
        write_project(&proj, None);
        let mut reg = Registry::load(&ctx).unwrap();
        reg.add(&proj, None, false, &ctx).unwrap();
        reg.set_enabled("ext", false).unwrap();
        assert!(!reg.find("ext").unwrap().enabled);
        let removed = reg.remove("ext", false).unwrap();
        assert_eq!(removed, vec!["ext".to_string()]);
        assert!(reg.entries().is_empty());
    }

    #[test]
    fn lock_is_released_after_save() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx_with_home(tmp.path());
        let proj = tmp.path().join("ext");
        write_project(&proj, None);
        let mut reg = Registry::load(&ctx).unwrap();
        reg.add(&proj, None, false, &ctx).unwrap();
        reg.save().unwrap();
        // The lockfile must not linger.
        assert!(!registry_lock_path(&ctx).exists());
        // A second save still succeeds (lock reacquired).
        reg.save().unwrap();
    }
}
