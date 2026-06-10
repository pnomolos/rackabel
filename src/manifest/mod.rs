//! The `rackabel.toml` project manifest (DESIGN §4) — the single source of truth.
//!
//! `ManifestRaw` mirrors exactly what is on disk (every field optional, except the
//! existing `[device]` table whose schema is frozen). [`Project`] wraps a loaded
//! manifest with its root; resolution + inference happen on demand, echoing each
//! inferred field. Exactly one of `[extension]`/`[device]` is required (or a
//! `[workspace]` root). The `[workspace]` table is parse-only for 0.2.
//!
//! ## Frozen surface
//! The public types and signatures here are frozen as of the foundation commit;
//! parallel command-owners compile against them. The old `project.rs` device types
//! are re-exported from here as a compatibility shim until the M4L paths migrate.

pub mod infer;
pub mod pkgjson;
pub mod sdk_manifest;
pub mod state;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::ui;

pub const MANIFEST_NAME: &str = "rackabel.toml";

/// The on-disk manifest, exactly as written. All tables optional; `deny_unknown_fields`
/// turns a typo'd table into a parse error (RK0003) rather than a silent no-op.
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ManifestRaw {
    pub extension: Option<ExtensionRaw>,
    /// Existing M4L device table — semantics UNCHANGED.
    pub device: Option<DeviceRaw>,
    pub host: Option<Host>,
    pub toolchain: Option<Toolchain>,
    pub meta: Option<Meta>,
    pub workspace: Option<Workspace>,
    /// `[dev]` — managed dev-host knobs (milestone 0.3). Optional so every other command
    /// accepts a manifest that carries it (without it, `deny_unknown_fields` would reject
    /// `[dev]` as RK0003 while the watch loop read it separately — D-65).
    pub dev: Option<Dev>,
    /// `[hooks]` + `[hooks.timeouts]` — PROJECT-LOCAL lifecycle hooks (milestone 0.5,
    /// DESIGN §5.5). The user's OWN repo scripts, run with no plugin manifest and no enable
    /// step (implicit trust); paths are relative to the project root. Optional, like
    /// `[dev]`, so a manifest without it parses under `deny_unknown_fields`.
    pub hooks: Option<crate::hooks::manifest::HooksTable>,
}

/// `[extension]` — all fields optional with documented inference (DESIGN §4.2).
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ExtensionRaw {
    pub name: Option<String>,
    pub author: Option<String>,
    pub version: Option<String>,
    pub entry: Option<PathBuf>,
    pub minimum_api_version: Option<String>,
    pub build: Option<ExtBuild>,
    pub pack: Option<ExtPack>,
}

/// `[extension.build]` (was `arclwill*` package.json keys).
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ExtBuild {
    /// dist-relative basenames copied alongside the bundle (was arclightExtraDistFiles).
    #[serde(default)]
    pub extra_dist_files: Vec<String>,
    /// npm package names externalized from the bundle and copied to node_modules.
    #[serde(default)]
    pub native_deps: Vec<String>,
}

/// `[extension.pack]`.
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ExtPack {
    /// Hyphenated os-arch strings, e.g. `"darwin-arm64"` (was arclightPackTargets objects).
    #[serde(default)]
    pub targets: Vec<String>,
}

/// `[device]` — the existing M4L schema. All fields required, verbatim.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRaw {
    pub name: String,
    pub kind: String,
    /// Path to the main .maxpat, relative to the project root.
    pub entry: PathBuf,
}

/// `[host]` — shared host overrides.
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Host {
    pub live: Option<PathBuf>,
    pub user_library: Option<PathBuf>,
}

/// `[toolchain]` — two node floors + SDK contract version (DESIGN §4.2).
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Toolchain {
    pub node_runtime: Option<String>,
    pub node_build: Option<String>,
    pub sdk: Option<String>,
}

/// `[meta]` — carried ahead of the SDK for future manifest.json fields.
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
}

/// `[workspace]` — member globs. Parse-only in 0.2.
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Workspace {
    #[serde(default)]
    pub members: Vec<String>,
}

/// `[dev]` — managed dev-host knobs (milestone 0.3, DESIGN §3.3).
#[derive(Debug, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Dev {
    /// Watch-loop debounce before a rebuild→deploy→reload, in milliseconds. The watch
    /// loop defaults to 200 ms when this is absent.
    pub debounce_ms: Option<u64>,
}

/// What kind of project this is. Exactly one of these is valid.
///
/// `Serialize`/`Deserialize` (lowercase) so the dev registry can persist a kind
/// chosen at `register --type` time (registry agent owns the entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Extension,
    Device,
    Workspace,
}

/// A loaded project: its root + the raw manifest. Resolution is on demand.
///
/// A project may be *synthesized* — anchored on a `package.json` with no
/// `rackabel.toml` on disk (DESIGN §4.1). In that case `manifest_path` is `None`,
/// `raw` is the [`ManifestRaw::default()`] (all tables absent), and `pkg` carries
/// the anchoring `package.json` as a fallback inference + kind source. When a real
/// `rackabel.toml` is present it ALWAYS wins; `package.json` only fills gaps.
#[derive(Debug)]
pub struct Project {
    pub root: PathBuf,
    pub raw: ManifestRaw,
    /// `Some(path)` for a real on-disk `rackabel.toml`; `None` for a synthesized
    /// (manifestless) project anchored on a `package.json`.
    pub manifest_path: Option<PathBuf>,
    /// The anchoring / adjacent `package.json`, if any — a fallback inference and
    /// kind source. Never authoritative over a present `rackabel.toml`.
    pub pkg: Option<pkgjson::PkgJson>,
    /// A kind injected by the caller (the dev registry's `--type`), checked FIRST
    /// in [`Self::kind`] so a registered project resolves without a manifest.
    pub kind_override: Option<Kind>,
}

impl Project {
    /// Walk up from `start` for the project root (DESIGN §4.1).
    ///
    /// Anchoring order: (a) the nearest `rackabel.toml` wins (today's behavior),
    /// parsed as a real manifest; (b) else the nearest `package.json` anchors a
    /// *synthesized* manifestless project; (c) else `RK0001`. A present
    /// `rackabel.toml` always wins — `package.json` is only a fallback anchor.
    /// `RK0003` on a manifest parse error.
    pub fn discover(start: &Path) -> CmdResult<Self> {
        // (a) A real rackabel.toml anywhere up the tree wins.
        for dir in start.ancestors() {
            let candidate = dir.join(MANIFEST_NAME);
            if candidate.is_file() {
                let raw_str = std::fs::read_to_string(&candidate).map_err(|e| {
                    RkError::of(
                        ErrorCode::ManifestParse,
                        format!("could not read {MANIFEST_NAME}"),
                        "check the file's permissions and try again",
                    )
                    .at(candidate.display().to_string())
                    .raw(e.into())
                })?;
                let raw: ManifestRaw = toml::from_str(&raw_str).map_err(|e| {
                    RkError::of(
                        ErrorCode::ManifestParse,
                        format!("{MANIFEST_NAME} could not be parsed"),
                        "fix the TOML syntax (or unknown field) shown above and rerun",
                    )
                    .at(candidate.display().to_string())
                    .raw(e.into())
                })?;
                let root = dir.to_path_buf();
                let pkg = pkgjson::read(&root);
                return Ok(Self {
                    root,
                    raw,
                    manifest_path: Some(candidate),
                    pkg,
                    kind_override: None,
                });
            }
        }
        // (b) No manifest — fall back to the nearest package.json as the anchor, but
        // BOUNDED (#5): a package.json inside `node_modules` is a dependency manifest,
        // never a project root; and we never anchor at or above the home directory, so a
        // stray `~/package.json` can't silently turn an arbitrary cwd into a "project"
        // (that case must stay a clean RK0001).
        let home = std::env::var_os("HOME").map(PathBuf::from);
        for dir in start.ancestors() {
            if home.as_deref() == Some(dir) {
                break;
            }
            if dir.components().any(|c| c.as_os_str() == "node_modules") {
                continue;
            }
            if dir.join("package.json").is_file() {
                let root = dir.to_path_buf();
                let pkg = pkgjson::read(&root);
                return Ok(Self {
                    root,
                    raw: ManifestRaw::default(),
                    manifest_path: None,
                    pkg,
                    kind_override: None,
                });
            }
        }
        // (c) Neither anchor — RK0001.
        Err(RkError::of(
            ErrorCode::NoManifest,
            "no manifest found",
            "run `rackabel new` to scaffold one, add a package.json, or cd into a project directory",
        )
        .at(format!(
            "looked for {MANIFEST_NAME} or package.json in {} and its parents",
            start.display()
        )))
    }

    /// Discover the project, then inject a caller-supplied `kind` as the override
    /// (checked first in [`Self::kind`]). Used by the dev registry's `--type` path
    /// so a registered, manifestless project resolves to the registered kind.
    pub fn discover_with_kind(start: &Path, kind: Option<Kind>) -> CmdResult<Self> {
        let mut project = Self::discover(start)?;
        project.kind_override = kind;
        Ok(project)
    }

    /// Discover the project from the cwd recorded in `ctx`.
    pub fn discover_cwd(ctx: &Ctx) -> CmdResult<Self> {
        Self::discover(&ctx.cwd)
    }

    /// The project kind. `RK0002` if both or neither `[extension]`/`[device]`
    /// (unless a `[workspace]` makes it a workspace root).
    ///
    /// A caller-injected `kind_override` (the dev registry's `--type`) wins first.
    /// A present table always decides next (including the RK0002 errors). Only a
    /// *synthesized* (manifestless) project with no recognized table falls through
    /// to the default: `package.json`'s `"rackabel": { "kind": "device" }` opts in
    /// to Device, otherwise Extension (DESIGN §4.1).
    pub fn kind(&self) -> CmdResult<Kind> {
        let has_ext = self.raw.extension.is_some();
        let has_dev = self.raw.device.is_some();
        let has_ws = self.raw.workspace.is_some();
        if let Some(k) = self.kind_override {
            // A `--type`/registry override may only DECIDE the kind when the present
            // manifest declares no conflicting table. If the manifest actually
            // declares the OTHER kind's table, overriding would route to the wrong
            // resolver (and previously panicked) — reject it explicitly (FIX #3).
            let conflicts = match k {
                Kind::Extension => has_dev,
                Kind::Device => has_ext,
                Kind::Workspace => has_ext || has_dev,
            };
            if conflicts {
                return Err(RkError::of(
                    ErrorCode::AmbiguousKind,
                    "--type conflicts with the table declared in rackabel.toml",
                    "drop --type (the manifest already declares the kind), or fix the manifest's table to match",
                )
                .at(self.manifest_path_string()));
            }
            return Ok(k);
        }
        match (has_ext, has_dev, has_ws) {
            (true, false, _) => Ok(Kind::Extension),
            (false, true, _) => Ok(Kind::Device),
            (false, false, true) => Ok(Kind::Workspace),
            (true, true, _) => Err(RkError::of(
                ErrorCode::AmbiguousKind,
                "this project declares both [extension] and [device]",
                "keep exactly one — split them into separate projects (a workspace can hold both)",
            )
            .at(self.manifest_path_string())),
            (false, false, false) => {
                if self.manifest_path.is_some() {
                    // A real but empty manifest is a genuine user mistake.
                    Err(RkError::of(
                        ErrorCode::AmbiguousKind,
                        "this project declares neither [extension] nor [device]",
                        "add an [extension] or [device] table (see `rackabel new`)",
                    )
                    .at(self.manifest_path_string()))
                } else {
                    // Synthesized (manifestless) project: default to Extension
                    // unless package.json opts in to a device.
                    let is_device = self
                        .pkg
                        .as_ref()
                        .and_then(|p| p.rackabel.as_ref())
                        .and_then(|r| r.kind.as_deref())
                        .map(|k| k.eq_ignore_ascii_case("device"))
                        .unwrap_or(false);
                    Ok(if is_device {
                        Kind::Device
                    } else {
                        Kind::Extension
                    })
                }
            }
        }
    }

    /// Resolve the `[extension]` with inference, echoing each inferred field (unless
    /// `--json`). Errors `RK0002` if the project is not an extension.
    pub fn resolved_extension(&self, ctx: &Ctx) -> CmdResult<ResolvedExtension> {
        if self.kind()? != Kind::Extension {
            return Err(RkError::of(
                ErrorCode::AmbiguousKind,
                "this command needs a Live Extension project",
                "run it inside a project with an [extension] table",
            )
            .at(self.manifest_path_string()));
        }
        // A synthesized (manifestless) project resolves as an extension with no
        // `[extension]` table on disk; fall back to a defaulted `ExtensionRaw` so
        // every field infers (dir basename, git author, 0.1.0, src/extension.ts).
        let default_ext = ExtensionRaw::default();
        let ext = self.raw.extension.as_ref().unwrap_or(&default_ext);
        let mut inferred = Vec::new();

        // Inference order per field (DESIGN §4.2): manifest -> package.json -> default.
        // package.json is consulted ONLY for synthesized (manifestless) projects;
        // when a real `rackabel.toml` is present it never participates — a missing
        // field infers manifest -> git/dir/default exactly as before (FIX #4). Any
        // package.json value used here is still an *inferred* value, so it echoes
        // like any other inference.
        let pkg = if self.manifest_path.is_none() {
            self.pkg.as_ref()
        } else {
            None
        };

        let name = match &ext.name {
            Some(n) => n.clone(),
            None => {
                let from_pkg = pkg
                    .and_then(|p| p.rackabel.as_ref().and_then(|r| r.name.clone()).or_else(|| p.name.clone()))
                    .filter(|s| !s.trim().is_empty());
                let n = from_pkg.unwrap_or_else(|| infer::infer_name_from_dir(&self.root));
                ui::echo_inferred("name", &n, "set [extension].name to override", ctx);
                inferred.push(InferredField {
                    key: "name",
                    value: n.clone(),
                });
                n
            }
        };

        let author = match &ext.author {
            Some(a) => a.clone(),
            None => {
                let a = pkg
                    .and_then(|p| p.author_display())
                    .or_else(infer::infer_author_from_git)
                    .unwrap_or_default();
                if a.is_empty() {
                    // We do not hard-fail; the empty author surfaces in validate (RK4001).
                    ui::echo_inferred(
                        "author",
                        "(unknown)",
                        "set [extension].author or run `git config user.name`",
                        ctx,
                    );
                } else {
                    ui::echo_inferred("author", &a, "set [extension].author to override", ctx);
                }
                inferred.push(InferredField {
                    key: "author",
                    value: a.clone(),
                });
                a
            }
        };

        let version = match &ext.version {
            Some(v) => parse_version(v, "version")?,
            None => {
                let v = match pkg.and_then(|p| p.version.as_deref()) {
                    // A valid package.json version wins over the 0.1.0 default; an
                    // unparseable one falls through silently to the default.
                    Some(s) => semver::Version::parse(s).unwrap_or_else(|_| infer::default_version()),
                    None => infer::default_version(),
                };
                ui::echo_inferred(
                    "version",
                    &v.to_string(),
                    "set [extension].version to override",
                    ctx,
                );
                inferred.push(InferredField {
                    key: "version",
                    value: v.to_string(),
                });
                v
            }
        };

        let entry = match &ext.entry {
            Some(e) => e.clone(),
            None => {
                let from_pkg = pkg
                    .and_then(|p| p.rackabel.as_ref().and_then(|r| r.entry.clone()))
                    .filter(|s| !s.trim().is_empty())
                    .map(PathBuf::from);
                let e = from_pkg.unwrap_or_else(|| infer::infer_entry(&self.root));
                ui::echo_inferred(
                    "entry",
                    &e.display().to_string(),
                    "set [extension].entry to override",
                    ctx,
                );
                inferred.push(InferredField {
                    key: "entry",
                    value: e.display().to_string(),
                });
                e
            }
        };

        // minimum_api_version: inference from the vendored SDK is the build/new
        // owner's job (it has a resolved Toolkit/SDK). The foundation default is the
        // known beta value; if absent, we echo the fallback and record it as inferred.
        let minimum_api_version = match &ext.minimum_api_version {
            Some(v) => parse_version(v, "minimum_api_version")?,
            None => {
                let v = semver::Version::new(1, 0, 0);
                ui::echo_inferred(
                    "minimum_api_version",
                    &v.to_string(),
                    "set [extension].minimum_api_version or vendor the SDK",
                    ctx,
                );
                inferred.push(InferredField {
                    key: "minimum_api_version",
                    value: v.to_string(),
                });
                v
            }
        };

        let (extra_dist_files, native_deps) = match &ext.build {
            Some(b) => (b.extra_dist_files.clone(), b.native_deps.clone()),
            None => (Vec::new(), Vec::new()),
        };
        let pack_targets = match &ext.pack {
            Some(p) if !p.targets.is_empty() => p.targets.clone(),
            _ => vec![default_pack_target()],
        };

        Ok(ResolvedExtension {
            name,
            author,
            version,
            entry,
            minimum_api_version,
            extra_dist_files,
            native_deps,
            pack_targets,
            inferred,
        })
    }

    /// The install slug = the project root directory basename (launcher
    /// convention, DESIGN §2 deploy) — **not** the manifest name.
    pub fn slug(&self) -> String {
        self.root
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("extension")
            .to_string()
    }

    /// A location string for error `.at(...)` sites. Prefers the real manifest
    /// path; for a synthesized project (no `rackabel.toml`) it falls back to the
    /// project root so we never point at a file that does not exist.
    fn manifest_path_string(&self) -> String {
        match &self.manifest_path {
            Some(p) => p.display().to_string(),
            None => self.root.display().to_string(),
        }
    }

    /// The project-local `[hooks]` table (DESIGN §5.5), if the manifest declares one.
    /// These run with implicit trust (the user's own repo, no enable step). The hook
    /// discovery resolver ([`crate::hooks::discovery::resolve`]) takes this + the project
    /// root as its project source; commands are relative to [`Self::root`].
    pub fn hooks_table(&self) -> Option<&crate::hooks::manifest::HooksTable> {
        self.raw.hooks.as_ref()
    }
}

/// The host platform-arch string used as the default single pack target
/// (Node's `process.platform`/`process.arch` vocabulary).
pub fn default_pack_target() -> String {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        std::env::consts::ARCH
    };
    format!("{os}-{arch}")
}

fn parse_version(s: &str, field: &str) -> CmdResult<semver::Version> {
    semver::Version::parse(s).map_err(|e| {
        RkError::of(
            ErrorCode::ManifestParse,
            format!("[extension].{field} is not a valid semver version"),
            "use a version like 0.1.0",
        )
        .at(format!("{field} = \"{s}\""))
        .raw(e.into())
    })
}

/// All extension fields concrete after inference. `inferred` lists what was guessed
/// (for `--json` and re-echoing).
#[derive(Debug)]
pub struct ResolvedExtension {
    pub name: String,
    pub author: String,
    pub version: semver::Version,
    pub entry: PathBuf,
    pub minimum_api_version: semver::Version,
    pub extra_dist_files: Vec<String>,
    pub native_deps: Vec<String>,
    pub pack_targets: Vec<String>,
    pub inferred: Vec<InferredField>,
}

/// A single inferred field, for echo + `--json`.
#[derive(Debug, Clone)]
pub struct InferredField {
    pub key: &'static str,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Compatibility shim for the existing M4L `[device]` command paths.
//
// The old `crate::project` module exposed `Manifest`/`Device`/`Project` with a
// flat `[device]`-only schema. To keep those paths compiling unchanged while they
// migrate to `ManifestRaw`, we provide a thin `DeviceProject` here that the M4L
// commands use. The `[device]` TOML schema is preserved verbatim.
// ---------------------------------------------------------------------------

/// A device project loaded from the new `ManifestRaw` but exposing the old
/// `.device` shape the M4L commands expect. Behavior is identical to the previous
/// `project::Project`.
#[derive(Debug)]
pub struct DeviceProject {
    pub root: PathBuf,
    pub device: DeviceRaw,
}

impl DeviceProject {
    /// Discover and load a device project (errors if it is not a device project).
    pub fn discover_cwd(ctx: &Ctx) -> CmdResult<Self> {
        let project = Project::discover_cwd(ctx)?;
        if project.kind()? != Kind::Device {
            return Err(RkError::of(
                ErrorCode::AmbiguousKind,
                "this command needs a Max for Live device project",
                "run it inside a project with a [device] table",
            )
            .at(project.manifest_path_string()));
        }
        // A manifestless device opt-in (package.json `"rackabel":{"kind":"device"}`
        // or `--type device`) passes the kind guard but has no `[device]` table.
        // The `[device]` schema's fields (name/kind/entry) are required with no
        // inference, so we cannot synthesize one — frame a clear error instead of
        // panicking.
        let Some(device) = project.raw.device else {
            return Err(RkError::of(
                ErrorCode::AmbiguousKind,
                "a Max for Live device needs a [device] table — manifestless device projects aren't supported",
                "add a rackabel.toml with a [device] table (name, kind, entry), or if this is an extension drop --type device",
            )
            .at(project.manifest_path_string()));
        };
        Ok(Self {
            root: project.root,
            device,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write_manifest(dir: &Path, body: &str) {
        fs::write(dir.join(MANIFEST_NAME), body).unwrap();
    }

    fn write_pkg(dir: &Path, body: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("package.json"), body).unwrap();
    }

    #[test]
    fn discover_walks_up() {
        let tmp = tempdir().unwrap();
        write_manifest(tmp.path(), "[extension]\nname = \"x\"\n");
        let sub = tmp.path().join("a/b/c");
        fs::create_dir_all(&sub).unwrap();
        let p = Project::discover(&sub).unwrap();
        assert_eq!(p.root, tmp.path());
    }

    #[test]
    fn discover_no_manifest_is_rk0001() {
        let tmp = tempdir().unwrap();
        let err = Project::discover(tmp.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::NoManifest);
    }

    #[test]
    fn parse_error_is_rk0003() {
        let tmp = tempdir().unwrap();
        write_manifest(tmp.path(), "[extension]\nname = \n");
        let err = Project::discover(tmp.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestParse);
    }

    #[test]
    fn unknown_field_is_parse_error() {
        let tmp = tempdir().unwrap();
        write_manifest(tmp.path(), "[extension]\nbogus = 1\n");
        let err = Project::discover(tmp.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ManifestParse);
    }

    #[test]
    fn kind_extension_and_device_and_both() {
        let tmp = tempdir().unwrap();
        write_manifest(tmp.path(), "[extension]\n");
        assert_eq!(
            Project::discover(tmp.path()).unwrap().kind().unwrap(),
            Kind::Extension
        );

        write_manifest(
            tmp.path(),
            "[device]\nname=\"d\"\nkind=\"audio-effect\"\nentry=\"x.maxpat\"\n",
        );
        assert_eq!(
            Project::discover(tmp.path()).unwrap().kind().unwrap(),
            Kind::Device
        );

        write_manifest(
            tmp.path(),
            "[extension]\n[device]\nname=\"d\"\nkind=\"audio-effect\"\nentry=\"x.maxpat\"\n",
        );
        let err = Project::discover(tmp.path()).unwrap().kind().unwrap_err();
        assert_eq!(err.code, ErrorCode::AmbiguousKind);
    }

    #[test]
    fn project_local_hooks_table_parses() {
        use crate::hooks::HookKind;
        let tmp = tempdir().unwrap();
        write_manifest(
            tmp.path(),
            "[extension]\nname=\"x\"\n[hooks]\npost_build = \".rackabel/hooks/post-build\"\n",
        );
        let p = Project::discover(tmp.path()).unwrap();
        let table = p.hooks_table().expect("a [hooks] table");
        assert_eq!(
            table.command(HookKind::PostBuild),
            Some(".rackabel/hooks/post-build")
        );
    }

    #[test]
    fn manifest_without_hooks_parses_and_has_none() {
        let tmp = tempdir().unwrap();
        write_manifest(tmp.path(), "[extension]\nname=\"x\"\n");
        let p = Project::discover(tmp.path()).unwrap();
        assert!(p.hooks_table().is_none());
    }

    #[test]
    fn slug_is_dir_basename_not_name() {
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("clip-renamer");
        fs::create_dir_all(&proj).unwrap();
        write_manifest(&proj, "[extension]\nname = \"Clip Renamer Deluxe\"\n");
        let p = Project::discover(&proj).unwrap();
        assert_eq!(p.slug(), "clip-renamer");
    }

    #[test]
    fn resolved_extension_infers_and_records() {
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("my-ext");
        fs::create_dir_all(&proj).unwrap();
        write_manifest(&proj, "[extension]\n");
        let p = Project::discover(&proj).unwrap();
        // Build a minimal ctx with json on to suppress echo noise in the test.
        let ctx = test_ctx(&proj, true);
        let r = p.resolved_extension(&ctx).unwrap();
        assert_eq!(r.name, "my-ext");
        assert_eq!(r.version, semver::Version::new(0, 1, 0));
        assert_eq!(r.entry, PathBuf::from("src/extension.ts"));
        assert!(r.inferred.iter().any(|f| f.key == "name"));
    }

    #[test]
    fn resolved_extension_explicit_values_not_inferred() {
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("my-ext");
        fs::create_dir_all(&proj).unwrap();
        write_manifest(
            &proj,
            "[extension]\nname=\"Cool\"\nauthor=\"Jane\"\nversion=\"2.3.4\"\nentry=\"src/main.ts\"\nminimum_api_version=\"1.0.0\"\n",
        );
        let p = Project::discover(&proj).unwrap();
        let ctx = test_ctx(&proj, true);
        let r = p.resolved_extension(&ctx).unwrap();
        assert_eq!(r.name, "Cool");
        assert_eq!(r.author, "Jane");
        assert_eq!(r.version, semver::Version::new(2, 3, 4));
        assert!(r.inferred.is_empty());
    }

    #[test]
    fn pack_targets_default_to_host() {
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("e");
        fs::create_dir_all(&proj).unwrap();
        write_manifest(&proj, "[extension]\n");
        let p = Project::discover(&proj).unwrap();
        let ctx = test_ctx(&proj, true);
        let r = p.resolved_extension(&ctx).unwrap();
        assert_eq!(r.pack_targets, vec![default_pack_target()]);
    }

    // --- manifest-optional: package.json fallback anchor + default kind -------------

    #[test]
    fn discover_anchors_on_package_json_when_no_manifest() {
        // No rackabel.toml anywhere — the nearest package.json synthesizes the project.
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("pkg-only");
        write_pkg(&proj, r#"{"name":"pkg-only"}"#);
        let sub = proj.join("src/deep");
        fs::create_dir_all(&sub).unwrap();

        let p = Project::discover(&sub).unwrap();
        assert_eq!(p.root, proj, "anchors at the package.json dir");
        assert!(p.manifest_path.is_none(), "synthesized: no rackabel.toml");
        assert!(p.pkg.is_some(), "carries the anchoring package.json");
    }

    #[test]
    fn discover_rk0001_only_when_neither_anchor_present() {
        let tmp = tempdir().unwrap();
        // Neither a rackabel.toml nor a package.json up the tree → RK0001.
        let err = Project::discover(tmp.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::NoManifest);
    }

    #[test]
    fn discover_manifest_wins_over_package_json() {
        // REGRESSION: a present rackabel.toml ALWAYS wins, even when a package.json sits
        // beside it — the manifest is the override surface, package.json only a fallback.
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("both");
        fs::create_dir_all(&proj).unwrap();
        write_manifest(&proj, "[extension]\nname = \"FromToml\"\n");
        write_pkg(&proj, r#"{"name":"from-pkg","rackabel":{"kind":"device"}}"#);

        let p = Project::discover(&proj).unwrap();
        assert!(p.manifest_path.is_some(), "the real manifest is the anchor");
        // The manifest declares [extension]; the package.json's device opt-in is ignored.
        assert_eq!(p.kind().unwrap(), Kind::Extension);
    }

    #[test]
    fn synthesized_kind_defaults_to_extension() {
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("syn");
        write_pkg(&proj, r#"{"name":"syn"}"#);
        let p = Project::discover(&proj).unwrap();
        assert!(p.manifest_path.is_none());
        assert_eq!(p.kind().unwrap(), Kind::Extension);
    }

    #[test]
    fn synthesized_package_json_kind_device_opts_in() {
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("dev");
        write_pkg(&proj, r#"{"name":"dev","rackabel":{"kind":"device"}}"#);
        let p = Project::discover(&proj).unwrap();
        assert_eq!(p.kind().unwrap(), Kind::Device);
    }

    #[test]
    fn kind_override_decides_when_no_conflicting_table() {
        // A registry-supplied --type override decides the kind for a manifestless /
        // synthesized project (and over a package.json device opt-in) — but only when
        // the manifest declares no conflicting table (see the conflict test below).
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("ovr");
        write_pkg(&proj, r#"{"name":"ovr","rackabel":{"kind":"device"}}"#);
        let p = Project::discover_with_kind(&proj, Some(Kind::Extension)).unwrap();
        assert_eq!(
            p.kind().unwrap(),
            Kind::Extension,
            "override beats the package.json device opt-in"
        );
    }

    #[test]
    fn kind_override_conflicting_with_declared_table_errors() {
        // FIX #3: a --type/kind_override that conflicts with a table actually declared
        // in a present rackabel.toml is rejected (it would otherwise route to the wrong
        // resolver and panic).
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("conflict");
        fs::create_dir_all(&proj).unwrap();
        write_manifest(&proj, "[extension]\nname=\"x\"\n");
        let p = Project::discover_with_kind(&proj, Some(Kind::Device)).unwrap();
        let err = p.kind().unwrap_err();
        assert_eq!(err.code, ErrorCode::AmbiguousKind);

        // The mirror case: --type extension over a real [device] table.
        let proj2 = tmp.path().join("conflict2");
        fs::create_dir_all(&proj2).unwrap();
        write_manifest(
            &proj2,
            "[device]\nname=\"d\"\nkind=\"audio-effect\"\nentry=\"x.maxpat\"\n",
        );
        let p2 = Project::discover_with_kind(&proj2, Some(Kind::Extension)).unwrap();
        assert_eq!(p2.kind().unwrap_err().code, ErrorCode::AmbiguousKind);
    }

    #[test]
    fn discover_with_kind_none_is_noop() {
        // #8: the backbone of the backward-compat claim — injecting NO override resolves
        // identically to a plain discover for a manifest-present project. A refactor that
        // accidentally defaulted kind_override to something would break this.
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("ext");
        fs::create_dir_all(&proj).unwrap();
        write_manifest(&proj, "[extension]\nname=\"x\"\n");
        let plain = Project::discover(&proj).unwrap();
        let injected = Project::discover_with_kind(&proj, None).unwrap();
        assert_eq!(injected.kind_override, None);
        assert_eq!(plain.kind().unwrap(), injected.kind().unwrap());
        assert_eq!(plain.kind().unwrap(), Kind::Extension);
    }

    #[test]
    fn real_manifest_with_no_table_still_rk0002() {
        // A REAL but empty rackabel.toml is a genuine user mistake — NOT the synthesized
        // default. Only manifestless projects fall through to the Extension default.
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("empty");
        fs::create_dir_all(&proj).unwrap();
        write_manifest(&proj, "[meta]\ndescription = \"no kind table\"\n");
        let err = Project::discover(&proj).unwrap().kind().unwrap_err();
        assert_eq!(err.code, ErrorCode::AmbiguousKind);
    }

    #[test]
    fn resolved_extension_falls_back_to_package_json_fields_when_manifestless() {
        // SYNTHESIZED (no rackabel.toml): package.json fills name/version/author/entry.
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("filled");
        write_pkg(
            &proj,
            r#"{"name":"pkg-name","version":"3.4.5","author":"Pkg Author",
                "rackabel":{"entry":"src/from-pkg.ts"}}"#,
        );
        let p = Project::discover(&proj).unwrap();
        assert!(p.manifest_path.is_none(), "synthesized project");
        let ctx = test_ctx(&proj, true);
        let r = p.resolved_extension(&ctx).unwrap();
        assert_eq!(r.name, "pkg-name");
        assert_eq!(r.version, semver::Version::new(3, 4, 5));
        assert_eq!(r.author, "Pkg Author");
        assert_eq!(r.entry, PathBuf::from("src/from-pkg.ts"));
    }

    #[test]
    fn resolved_extension_manifestless_truly_no_table_does_not_panic() {
        // FIX #1 + tests gap: a truly manifestless project (only package.json, NO
        // [extension] table) resolves Ok and does NOT panic; fields come from
        // package.json / inference.
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("zero-config");
        write_pkg(&proj, r#"{"name":"zero-config","version":"1.2.3"}"#);
        let p = Project::discover(&proj).unwrap();
        assert!(p.manifest_path.is_none());
        assert!(p.raw.extension.is_none(), "no [extension] table at all");
        let ctx = test_ctx(&proj, true);
        let r = p.resolved_extension(&ctx).unwrap();
        assert_eq!(r.name, "zero-config");
        assert_eq!(r.version, semver::Version::new(1, 2, 3));
        assert_eq!(r.entry, PathBuf::from("src/extension.ts"));
    }

    #[test]
    fn resolved_extension_manifestless_no_rackabel_key_succeeds() {
        // A bare package.json with NO "rackabel" key: kind defaults to Extension and
        // resolves fine (the build-style resolve path).
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("bare-pkg");
        write_pkg(&proj, r#"{"name":"bare-pkg"}"#);
        let p = Project::discover(&proj).unwrap();
        assert_eq!(p.kind().unwrap(), Kind::Extension);
        let ctx = test_ctx(&proj, true);
        let r = p.resolved_extension(&ctx).unwrap();
        assert_eq!(r.name, "bare-pkg");
        assert_eq!(r.version, semver::Version::new(0, 1, 0));
    }

    #[test]
    fn manifestless_device_opt_in_errors_not_panics() {
        // FIX #2: kind()==Device via package.json "rackabel".kind="device" with no
        // [device] table returns a framed Err, NOT a panic.
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("dev-no-table");
        write_pkg(&proj, r#"{"name":"dev-no-table","rackabel":{"kind":"device"}}"#);
        let ctx = test_ctx(&proj, true);
        let err = DeviceProject::discover_cwd(&ctx).unwrap_err();
        assert_eq!(err.code, ErrorCode::AmbiguousKind);
    }

    #[test]
    fn manifest_present_does_not_leak_package_json_fields() {
        // FIX #4 regression: a project WITH a rackabel.toml that omits `version` infers
        // the default 0.1.0 even when an adjacent package.json declares a different
        // version — proving package.json does NOT participate for manifest-present
        // projects.
        let tmp = tempdir().unwrap();
        let proj = tmp.path().join("manifest-present");
        fs::create_dir_all(&proj).unwrap();
        write_manifest(&proj, "[extension]\nname=\"x\"\n");
        write_pkg(&proj, r#"{"name":"pkg-name","version":"9.9.9","author":"Pkg"}"#);
        let p = Project::discover(&proj).unwrap();
        assert!(p.manifest_path.is_some());
        let ctx = test_ctx(&proj, true);
        let r = p.resolved_extension(&ctx).unwrap();
        assert_eq!(r.name, "x", "manifest name wins, no package.json leak");
        assert_eq!(
            r.version,
            semver::Version::new(0, 1, 0),
            "missing version infers the 0.1.0 default, NOT package.json's 9.9.9"
        );
    }

    // A bare Ctx for tests, with json on so echoes stay quiet.
    fn test_ctx(cwd: &Path, json: bool) -> Ctx {
        Ctx {
            no_input: true,
            json,
            quiet: false,
            verbose: false,
            raw: false,
            color: crate::ui::color::ColorMode::Never,
            color_err: crate::ui::color::ColorMode::Never,
            cwd: cwd.to_path_buf(),
            rackabel_home: cwd.join(".rackabel-home"),
            home: cwd.to_path_buf(),
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
