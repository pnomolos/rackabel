//! `rackabel install` — copy the built device into Ableton's User Library.

use anyhow::{Context, Result, bail};

use crate::max::paths;
use crate::project::Project;

pub fn run() -> Result<()> {
    let project = Project::discover_cwd()?;
    let device_name = &project.manifest.device.name;

    let built = project.root.join("build").join(format!("{device_name}.amxd"));
    if !built.is_file() {
        bail!(
            "no built device at {} — run `rackabel build` first",
            built.display()
        );
    }

    let Some(presets) = paths::m4l_presets_dir() else {
        bail!("couldn't determine Ableton's User Library location on this platform");
    };
    if !presets.is_dir() {
        bail!(
            "Ableton User Library not found at {} — is Live installed?",
            presets.display()
        );
    }

    let dest = presets.join(format!("{device_name}.amxd"));
    std::fs::copy(&built, &dest)
        .with_context(|| format!("copying to {}", dest.display()))?;
    println!("Installed {}", dest.display());
    Ok(())
}
