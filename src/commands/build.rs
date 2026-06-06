//! `rackabel build` — assemble the project's .amxd device.

use anyhow::{Context, Result, bail};

use crate::project::Project;

pub fn run() -> Result<()> {
    let project = Project::discover_cwd()?;
    let entry = project.root.join(&project.manifest.device.entry);
    if !entry.is_file() {
        bail!(
            "entry patch not found: {} (check `entry` in rackabel.toml)",
            entry.display()
        );
    }

    // Validate the entry patch is well-formed JSON before going further.
    let raw = std::fs::read_to_string(&entry)
        .with_context(|| format!("reading {}", entry.display()))?;
    serde_json::from_str::<serde_json::Value>(&raw)
        .with_context(|| format!("{} is not valid patch JSON", entry.display()))?;

    // TODO: wrap the patch in the .amxd container (ampf header + device
    // metadata) and write it to build/<name>.amxd.
    bail!(
        "`build` isn't implemented yet — it will assemble {} into build/{}.amxd",
        project.manifest.device.entry.display(),
        project.manifest.device.name
    );
}
