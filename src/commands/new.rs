//! `rackabel new` — scaffold a new M4L device project.

use std::path::Path;

use anyhow::{Context, Result, bail};
use clap::ValueEnum;

use crate::max::patch::{self, PatchKind};
use crate::project::MANIFEST_NAME;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum DeviceKind {
    AudioEffect,
    MidiEffect,
    Instrument,
}

impl DeviceKind {
    fn patch_kind(self) -> PatchKind {
        match self {
            Self::AudioEffect => PatchKind::AudioEffect,
            Self::MidiEffect => PatchKind::MidiEffect,
            Self::Instrument => PatchKind::Instrument,
        }
    }

    fn manifest_name(self) -> &'static str {
        match self {
            Self::AudioEffect => "audio-effect",
            Self::MidiEffect => "midi-effect",
            Self::Instrument => "instrument",
        }
    }
}

pub fn run(name: &str, kind: DeviceKind) -> Result<()> {
    let root = Path::new(name);
    if root.exists() {
        bail!("`{name}` already exists");
    }

    let src = root.join("src");
    std::fs::create_dir_all(&src).with_context(|| format!("creating {}", src.display()))?;

    let entry = format!("src/{name}.maxpat");
    let manifest = format!(
        "[device]\nname = \"{name}\"\nkind = \"{}\"\nentry = \"{entry}\"\n",
        kind.manifest_name()
    );
    std::fs::write(root.join(MANIFEST_NAME), manifest)?;

    let patch_json = serde_json::to_string_pretty(&patch::starter_patch(kind.patch_kind()))?;
    std::fs::write(root.join(&entry), patch_json)?;

    std::fs::write(
        root.join(".gitignore"),
        "/build/\n.DS_Store\n*.maxpat.bak\n",
    )?;

    println!("Created `{name}` ({})", kind.manifest_name());
    println!("\n  cd {name}");
    println!("  rackabel build      # assemble the .amxd");
    println!("  rackabel install    # copy it into Ableton's User Library");
    println!("  rackabel watch      # rebuild on save");
    Ok(())
}
