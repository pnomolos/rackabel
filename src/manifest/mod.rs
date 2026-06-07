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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Extension,
    Device,
    Workspace,
}

/// A loaded project: its root + the raw manifest. Resolution is on demand.
#[derive(Debug)]
pub struct Project {
    pub root: PathBuf,
    pub raw: ManifestRaw,
}

impl Project {
    /// Walk up from `start` for the nearest `rackabel.toml` and load it.
    /// `RK0001` if none found, `RK0003` on a parse error.
    pub fn discover(start: &Path) -> CmdResult<Self> {
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
                return Ok(Self {
                    root: dir.to_path_buf(),
                    raw,
                });
            }
        }
        Err(RkError::of(
            ErrorCode::NoManifest,
            "no manifest found",
            "run `rackabel new` to scaffold one, or cd into a project directory",
        )
        .at(format!(
            "looked for {MANIFEST_NAME} in {} and its parents",
            start.display()
        )))
    }

    /// Discover the project from the cwd recorded in `ctx`.
    pub fn discover_cwd(ctx: &Ctx) -> CmdResult<Self> {
        Self::discover(&ctx.cwd)
    }

    /// The project kind. `RK0002` if both or neither `[extension]`/`[device]`
    /// (unless a `[workspace]` makes it a workspace root).
    pub fn kind(&self) -> CmdResult<Kind> {
        let has_ext = self.raw.extension.is_some();
        let has_dev = self.raw.device.is_some();
        let has_ws = self.raw.workspace.is_some();
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
            (false, false, false) => Err(RkError::of(
                ErrorCode::AmbiguousKind,
                "this project declares neither [extension] nor [device]",
                "add an [extension] or [device] table (see `rackabel new`)",
            )
            .at(self.manifest_path_string())),
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
        // Safe: kind() confirmed extension is present.
        let ext = self.raw.extension.as_ref().expect("extension present");
        let mut inferred = Vec::new();

        let name = match &ext.name {
            Some(n) => n.clone(),
            None => {
                let n = infer::infer_name_from_dir(&self.root);
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
                let a = infer::infer_author_from_git().unwrap_or_default();
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
                let v = infer::default_version();
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
                let e = infer::infer_entry(&self.root);
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

    fn manifest_path_string(&self) -> String {
        self.root.join(MANIFEST_NAME).display().to_string()
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
        let device = project.raw.device.expect("device present");
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
