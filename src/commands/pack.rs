//! `rackabel pack` — production build → validate → distributable `.ablx` (DESIGN §2
//! pack, §4.7; SPEC A §1.4; SPEC B §4). OWNED BY THE PACK AGENT.
//!
//! Flow (the pre-publish gate, DESIGN §2):
//!   1. **Production build** (`--release` semantics through the shared build API) so a
//!      distributable is always minified, banner-baked, manifest-current.
//!   2. **Validate** (auto-run; any validation failure ⇒ exit 4) — never ship an
//!      artifact that fails the ship rules. Pack runs a focused subset here
//!      (manifest completeness + `minimumApiVersion ≤ host`); see the note on the
//!      validate-owner below.
//!   3. **Pre-validate `--include` rules** (relative, inside the extension dir,
//!      exists) and emit rackabel three-part errors BEFORE invoking any packer
//!      (SPEC A §1.4 step 5; DESIGN §2 include guards).
//!   4. **Route:**
//!      - pure-JS (no native deps) + default ⇒ shell out to the official
//!        `extensions-cli package` (DESIGN §4.7: thin wrapper, no drift), surfacing the
//!        exact `<name>-<version>.ablx` filename and passing `-o` to collect outputs.
//!      - native deps, OR `--no-official-cli` ⇒ rackabel's own packer (SPEC B §4):
//!        one archive per declared target, `<slug>-v<version>-<os>-<arch>.ablx`.
//!   5. Print copy-pasteable **install instructions** on success.
//!
//! ## Validation gate
//! pack delegates to `commands::validate::run` (the full ship-readiness checklist)
//! before producing any artifact — "never ship a failing artifact". Any validation
//! failure aborts the pack with exit 4.

use std::path::PathBuf;

use serde_json::json;

use crate::cli::PackArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::{Kind, Project, ResolvedExtension};
use crate::services::esbuild::{self, BuildOptions, DIST_ENTRY};
use crate::services::{node, official_cli, packer};
use crate::ui;

pub fn run(args: &PackArgs, ctx: &Ctx) -> CmdResult<()> {
    let project = Project::discover_cwd(ctx)?;
    match project.kind()? {
        Kind::Extension => pack_extension(&project, args, ctx),
        // The device `.amxd` pack path lands with the Max for Live milestone; the
        // existing M4L behavior is preserved (it has no pack yet).
        Kind::Device => Err(RkError::new(
            ErrorCode::PackFailed,
            ExitClass::BuildRuntime,
            "device `pack` isn't implemented yet — it will produce a .amxd",
            "track the Max for Live milestone for device packaging",
        )),
        Kind::Workspace => Err(RkError::new(
            ErrorCode::AmbiguousKind,
            ExitClass::Usage,
            "this is a workspace root, not a single project",
            "cd into a member directory to pack it",
        )),
    }
}

fn pack_extension(project: &Project, args: &PackArgs, ctx: &Ctx) -> CmdResult<()> {
    let ext = project.resolved_extension(ctx)?;
    let slug = project.slug();
    let version = ext.version.to_string();

    // The target list: --target overrides the manifest's [extension.pack].targets,
    // which themselves default to the host (resolved in the manifest layer).
    let targets: Vec<String> = if args.target.is_empty() {
        ext.pack_targets.clone()
    } else {
        args.target.clone()
    };

    // Decide the packer up front so --dry-run can describe it accurately.
    let use_official = !args.no_official_cli && ext.native_deps.is_empty();

    // --- Pre-validate include guards BEFORE anything else (cheap, friendly first). ---
    for inc in &args.include {
        packer::validate_include(&project.root, inc)?;
    }

    // --- Plan the output paths (so --dry-run reports the real filenames). ---
    let plan = OutputPlan::resolve(project, &ext, &slug, &version, &targets, use_official, args)?;

    if args.dry_run {
        report_dry_run(&plan, &ext, use_official, &args.include, ctx);
        return Ok(());
    }

    // --- 1. Production build (release semantics through the shared build API). ---
    let build_opts = BuildOptions {
        release: true,
        clean: false,
        typecheck: None, // None => default-on for release (esbuild API contract)
        print_config: false,
        dry_run: false,
        json: ctx.json,
    };
    esbuild::build_extension(project, &build_opts, ctx)?;

    // --- 2. Validate (auto-run; any failure ⇒ exit 4). ---
    // Delegate to the full `validate` checklist so pack's "validation passed" means
    // exactly what `rackabel validate` means — never ship an artifact `validate` would
    // reject (DESIGN §2 pack). See D-26: this replaces the earlier inline subset now
    // that `commands::validate::run` is a real, callable command.
    crate::commands::validate::run(&crate::cli::ValidateArgs { strict: false }, ctx)?;

    // --- 3/4. Pack via the chosen path. ---
    let outputs = if use_official {
        pack_official(project, &ext, &plan, &args.include, ctx)?
    } else {
        pack_own(
            project,
            &ext,
            &slug,
            &version,
            &targets,
            &plan,
            &args.include,
        )?
    };

    // --- 5. Report + install instructions. ---
    report_success(&outputs, use_official, ctx);
    Ok(())
}

// ---------------------------------------------------------------------------
// Output planning.
// ---------------------------------------------------------------------------

/// The resolved output path(s). Pure-JS produces a single `.ablx`; native produces one
/// per target. `--output`/`-o` overrides the *single* pure-JS path verbatim; for the
/// multi-target native case `-o` is treated as an output *directory*.
struct OutputPlan {
    /// (target-label, output-path). For pure-JS there is exactly one entry with an
    /// empty target label.
    entries: Vec<(String, PathBuf)>,
}

impl OutputPlan {
    fn resolve(
        project: &Project,
        ext: &ResolvedExtension,
        slug: &str,
        version: &str,
        targets: &[String],
        use_official: bool,
        args: &PackArgs,
    ) -> CmdResult<Self> {
        if use_official {
            // Single .ablx named the official way (<name>-<version>.ablx), in the
            // extension dir unless -o overrides the whole path.
            let name = packer::ablx_filename(&ext.name, version);
            let path = match &args.output {
                Some(o) => o.clone(),
                None => project.root.join(name),
            };
            Ok(Self {
                entries: vec![(String::new(), path)],
            })
        } else {
            // One .ablx per target. -o (if given) is the output *directory* for the set.
            let out_dir = args
                .output
                .clone()
                .unwrap_or_else(|| project.root.join("releases"));
            let mut entries = Vec::with_capacity(targets.len());
            for target in targets {
                let name = packer::native_ablx_filename(slug, version, target);
                entries.push((target.clone(), out_dir.join(name)));
            }
            Ok(Self { entries })
        }
    }
}

// ---------------------------------------------------------------------------
// The two packer paths.
// ---------------------------------------------------------------------------

/// Pure-JS: shell out to the official `extensions-cli package` (DESIGN §4.7).
fn pack_official(
    project: &Project,
    _ext: &ResolvedExtension,
    plan: &OutputPlan,
    includes: &[String],
    ctx: &Ctx,
) -> CmdResult<Vec<PathBuf>> {
    let cli = official_cli::locate(&project.root)?;
    let runtime = resolve_node(ctx)?;
    let (_, output) = &plan.entries[0];
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            RkError::new(
                ErrorCode::PackFailed,
                ExitClass::BuildRuntime,
                "could not create the output directory for the .ablx",
                "check write permissions for the output path",
            )
            .at(parent.display().to_string())
            .raw(e.into())
        })?;
    }
    official_cli::package(&cli, &runtime, &project.root, output, includes)?;
    Ok(vec![output.clone()])
}

/// Native (or `--no-official-cli`): rackabel's own packer, one archive per target.
fn pack_own(
    project: &Project,
    ext: &ResolvedExtension,
    _slug: &str,
    _version: &str,
    _targets: &[String],
    plan: &OutputPlan,
    includes: &[String],
) -> CmdResult<Vec<PathBuf>> {
    let host_target = crate::manifest::default_pack_target();
    let mut outputs = Vec::with_capacity(plan.entries.len());

    for (target, output) in &plan.entries {
        if ext.native_deps.is_empty() {
            // --no-official-cli on a pure-JS extension: reproduce the official member
            // layout ourselves (manifest + entry + includes), no node_modules.
            packer::pack_pure_js(&project.root, output, DIST_ENTRY, includes)?;
        } else {
            packer::pack_native_target(
                &project.root,
                output,
                DIST_ENTRY,
                &ext.extra_dist_files,
                &ext.native_deps,
                includes,
                target,
                &host_target,
            )?;
        }
        outputs.push(output.clone());
    }
    Ok(outputs)
}

/// Resolve the node used to drive the official CLI (same precedence as build).
fn resolve_node(ctx: &Ctx) -> CmdResult<node::NodeRuntime> {
    node::any_usable(ctx).ok_or_else(|| {
        RkError::new(
            ErrorCode::NoNodeRuntime,
            ExitClass::Environment,
            "couldn't find a Node runtime to run the packager",
            "install Ableton Live 12.4.5+ (it bundles the right Node), or install Node \
             on your PATH, then rerun — or pass --no-official-cli to use rackabel's \
             own packer (no Node needed)",
        )
    })
}

// ---------------------------------------------------------------------------
// Reporting.
// ---------------------------------------------------------------------------

fn report_dry_run(
    plan: &OutputPlan,
    ext: &ResolvedExtension,
    use_official: bool,
    includes: &[String],
    ctx: &Ctx,
) {
    if ctx.json {
        let v = json!({
            "dry_run": true,
            "packer": if use_official { "official-cli" } else { "rackabel" },
            "native_deps": ext.native_deps,
            "includes": includes,
            "outputs": plan.entries.iter().map(|(t, p)| json!({
                "target": t,
                "path": p,
            })).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&v).expect("json"));
        return;
    }
    println!("planned pack steps (nothing was changed):");
    println!("  - production build (--release)");
    println!("  - validate (manifest complete, minimumApiVersion <= host)");
    if !includes.is_empty() {
        println!("  - include: {}", includes.join(", "));
    }
    if use_official {
        println!("  - packer: official extensions-cli (pure-JS)");
    } else if ext.native_deps.is_empty() {
        println!("  - packer: rackabel (--no-official-cli)");
    } else {
        println!(
            "  - packer: rackabel (native deps: {})",
            ext.native_deps.join(", ")
        );
    }
    for (target, path) in &plan.entries {
        if target.is_empty() {
            println!("  - write {}", path.display());
        } else {
            println!("  - write {} [{target}]", path.display());
        }
    }
}

fn report_success(outputs: &[PathBuf], use_official: bool, ctx: &Ctx) {
    if ctx.json {
        let v = json!({
            "ok": true,
            "packer": if use_official { "official-cli" } else { "rackabel" },
            "outputs": outputs,
        });
        println!("{}", serde_json::to_string_pretty(&v).expect("json"));
        return;
    }
    for out in outputs {
        let kb = std::fs::metadata(out).map(|m| m.len() / 1024).unwrap_or(0);
        ui::frame::emit(
            ui::frame::Symbol::Good,
            &format!("packed {} ({kb} KB)", out.display()),
            ctx,
        );
    }
    // Copy-pasteable install instructions (DESIGN §2 pack).
    println!();
    println!("To install, drop the .ablx into Live → Settings → Extensions:");
    for out in outputs {
        println!("  {}", out.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use tempfile::tempdir;

    fn ext(
        name: &str,
        author: &str,
        api: (u64, u64, u64),
        native: Vec<String>,
    ) -> ResolvedExtension {
        ResolvedExtension {
            name: name.into(),
            author: author.into(),
            version: semver::Version::new(0, 1, 0),
            entry: PathBuf::from("src/extension.ts"),
            minimum_api_version: semver::Version::new(api.0, api.1, api.2),
            extra_dist_files: vec![],
            native_deps: native,
            pack_targets: vec!["darwin-arm64".into()],
            inferred: vec![],
        }
    }

    fn project_at(dir: &Path) -> Project {
        fs::write(dir.join("rackabel.toml"), "[extension]\n").unwrap();
        Project::discover(dir).unwrap()
    }

    #[test]
    fn plan_official_single_official_filename() {
        let tmp = tempdir().unwrap();
        let proj = project_at(tmp.path());
        let e = ext("Clip Renamer", "Jane", (1, 0, 0), vec![]);
        let args = PackArgs {
            target: vec![],
            include: vec![],
            output: None,
            no_official_cli: false,
            dry_run: false,
        };
        let plan = OutputPlan::resolve(
            &proj,
            &e,
            "x",
            "0.1.0",
            &["darwin-arm64".into()],
            true,
            &args,
        )
        .unwrap();
        assert_eq!(plan.entries.len(), 1);
        assert!(plan.entries[0].1.ends_with("Clip-Renamer-0.1.0.ablx"));
    }

    #[test]
    fn plan_native_one_per_target_in_releases() {
        let tmp = tempdir().unwrap();
        let proj = project_at(tmp.path());
        let e = ext("x", "Jane", (1, 0, 0), vec!["easymidi".into()]);
        let args = PackArgs {
            target: vec![],
            include: vec![],
            output: None,
            no_official_cli: false,
            dry_run: false,
        };
        let targets = vec!["darwin-arm64".into(), "darwin-x64".into()];
        let plan =
            OutputPlan::resolve(&proj, &e, "myext", "0.1.0", &targets, false, &args).unwrap();
        assert_eq!(plan.entries.len(), 2);
        assert!(
            plan.entries[0]
                .1
                .ends_with("myext-v0.1.0-darwin-arm64.ablx")
        );
        assert!(
            plan.entries[0]
                .1
                .components()
                .any(|c| c.as_os_str() == "releases")
        );
    }

    #[test]
    fn plan_output_override_for_pure_js_is_verbatim() {
        let tmp = tempdir().unwrap();
        let proj = project_at(tmp.path());
        let e = ext("x", "Jane", (1, 0, 0), vec![]);
        let custom = tmp.path().join("out/custom.ablx");
        let args = PackArgs {
            target: vec![],
            include: vec![],
            output: Some(custom.clone()),
            no_official_cli: false,
            dry_run: false,
        };
        let plan = OutputPlan::resolve(
            &proj,
            &e,
            "x",
            "0.1.0",
            &["darwin-arm64".into()],
            true,
            &args,
        )
        .unwrap();
        assert_eq!(plan.entries[0].1, custom);
    }
}
