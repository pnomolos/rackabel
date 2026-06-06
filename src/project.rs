//! The `rackabel.toml` project manifest.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const MANIFEST_NAME: &str = "rackabel.toml";

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub device: Device,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Device {
    pub name: String,
    pub kind: String,
    /// Path to the main .maxpat, relative to the project root.
    pub entry: PathBuf,
}

pub struct Project {
    pub root: PathBuf,
    pub manifest: Manifest,
}

impl Project {
    /// Find the nearest `rackabel.toml` at or above `start` and load it.
    pub fn discover(start: &Path) -> Result<Self> {
        for dir in start.ancestors() {
            let candidate = dir.join(MANIFEST_NAME);
            if candidate.is_file() {
                let raw = std::fs::read_to_string(&candidate)
                    .with_context(|| format!("reading {}", candidate.display()))?;
                let manifest: Manifest = toml::from_str(&raw)
                    .with_context(|| format!("parsing {}", candidate.display()))?;
                return Ok(Self {
                    root: dir.to_path_buf(),
                    manifest,
                });
            }
        }
        bail!(
            "no {MANIFEST_NAME} found in this directory or any parent — \
             run `rackabel new <name>` to start a project"
        );
    }

    /// Discover the project from the current working directory.
    pub fn discover_cwd() -> Result<Self> {
        Self::discover(&std::env::current_dir()?)
    }
}
