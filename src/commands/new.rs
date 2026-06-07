//! `rackabel new` — scaffold a project (Extension or M4L device).
//!
//! OWNED BY THE NEW AGENT for the Extension path. The foundation provides the
//! device (M4L) scaffolding, preserved verbatim from the previous implementation
//! (behavior unchanged), plus a stub for the Extension path so the command compiles
//! until the new-owner lands it. Kind dispatch: explicit `--kind`, else default to
//! Extension (DESIGN §2: the no-flag path is the musician/Extension path). The
//! new-owner refines the wizard.

use std::path::Path;

use crate::cli::{DeviceKindArg, NewArgs, ProjectKind};
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::MANIFEST_NAME;
use crate::max::patch::{self, PatchKind};

pub fn run(args: &NewArgs, ctx: &Ctx) -> CmdResult<()> {
    // Default to Extension when unspecified (the musician happy path). The
    // new-owner may add a wizard prompt for the kind when interactive.
    let kind = args.kind.unwrap_or(ProjectKind::Extension);
    match kind {
        ProjectKind::Extension => new_extension(args, ctx),
        ProjectKind::Device => new_device(args, ctx),
    }
}

/// Extension scaffolding — STUB owned by the new-owner.
fn new_extension(args: &NewArgs, _ctx: &Ctx) -> CmdResult<()> {
    let _ = args;
    Err(crate::services::esbuild::not_implemented("new (extension)"))
}

/// M4L device scaffolding — preserved verbatim from the previous implementation.
fn new_device(args: &NewArgs, ctx: &Ctx) -> CmdResult<()> {
    let name = match &args.name {
        Some(n) => n.clone(),
        None => {
            return Err(RkError::new(
                ErrorCode::ManifestIncomplete,
                ExitClass::Usage,
                "a device project needs a name",
                "pass it: `rackabel new <name> --kind device`",
            ));
        }
    };

    let dev_kind = args.device_kind.unwrap_or(DeviceKindArg::AudioEffect);

    let root = ctx.cwd.join(&name);
    if root.exists() {
        return Err(RkError::new(
            ErrorCode::ManifestIncomplete,
            ExitClass::Usage,
            format!("`{name}` already exists"),
            "choose a different name or remove the existing directory",
        )
        .at(root.display().to_string()));
    }

    let src = root.join("src");
    std::fs::create_dir_all(&src).map_err(io_err(&src))?;

    let entry = format!("src/{name}.maxpat");
    let manifest = format!(
        "[device]\nname = \"{name}\"\nkind = \"{}\"\nentry = \"{entry}\"\n",
        manifest_name(dev_kind)
    );
    std::fs::write(root.join(MANIFEST_NAME), manifest).map_err(io_err(&root))?;

    let patch_json = serde_json::to_string_pretty(&patch::starter_patch(patch_kind(dev_kind)))
        .expect("starter patch serializes");
    std::fs::write(root.join(&entry), patch_json).map_err(io_err(&root))?;

    std::fs::write(
        root.join(".gitignore"),
        "/build/\n.DS_Store\n*.maxpat.bak\n",
    )
    .map_err(io_err(&root))?;

    println!("Created `{name}` ({})", manifest_name(dev_kind));
    println!("\n  cd {name}");
    println!("  rackabel build      # assemble the .amxd");
    println!("  rackabel install    # copy it into Ableton's User Library");
    Ok(())
}

fn patch_kind(k: DeviceKindArg) -> PatchKind {
    match k {
        DeviceKindArg::AudioEffect => PatchKind::AudioEffect,
        DeviceKindArg::MidiEffect => PatchKind::MidiEffect,
        DeviceKindArg::Instrument => PatchKind::Instrument,
    }
}

fn manifest_name(k: DeviceKindArg) -> &'static str {
    match k {
        DeviceKindArg::AudioEffect => "audio-effect",
        DeviceKindArg::MidiEffect => "midi-effect",
        DeviceKindArg::Instrument => "instrument",
    }
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> RkError {
    let path = path.to_path_buf();
    move |e| {
        RkError::new(
            ErrorCode::DeployCopyFailed,
            ExitClass::BuildRuntime,
            "could not write the project files",
            "check write permissions for the target directory",
        )
        .at(path.display().to_string())
        .raw(e.into())
    }
}
