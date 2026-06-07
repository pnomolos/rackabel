//! Ableton Live install detection (DESIGN §3.6, §6 detection; SPEC B §6, SPEC C §3.5).
//!
//! Enumerates Live installs (newest-first), reads the version from `Info.plist`
//! (`CFBundleShortVersionString`), determines the arch from the Mach-O fat header of
//! the main executable, and probes the Extension Host layout — modern
//! `Contents/Helpers/ExtensionHost` first, then the `≤12.4`-alpha
//! `Contents/App-Resources/Extensions/ExtensionHost` fallback — reporting which
//! exists, never hardcoding. The `--live`/`$ABLETON_APP` override (resolved into
//! `ctx.ableton_app`) takes precedence over the `/Applications` scan, which is the
//! testability seam.

use std::path::{Path, PathBuf};

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, RkError};
use crate::ui;

/// The host module filename (SPEC A §1.5 constant).
pub const NODE_MODULE_FILE: &str = "ExtensionHostNodeModule.node";

/// The two host layouts, probed in this order (SPEC B §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostLayout {
    /// `Contents/Helpers/ExtensionHost` (≥ 12.4.5 beta).
    Helpers,
    /// `Contents/App-Resources/Extensions/ExtensionHost` (≤ 12.4 alpha).
    AppResources,
}

impl HostLayout {
    fn rel(self) -> &'static str {
        match self {
            HostLayout::Helpers => "Contents/Helpers/ExtensionHost",
            HostLayout::AppResources => "Contents/App-Resources/Extensions/ExtensionHost",
        }
    }
}

/// The arch the Live app runs as on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveArch {
    AppleSilicon,
    IntelRosetta,
    /// Universal binary; the friendly string depends on the host arch.
    Universal,
    /// Could not determine (degrade gracefully).
    Unknown,
}

impl LiveArch {
    /// A musician-facing description (DESIGN doctor transcript).
    pub fn friendly(self) -> &'static str {
        match self {
            LiveArch::AppleSilicon => "Apple Silicon",
            LiveArch::IntelRosetta => "Intel, via Rosetta",
            LiveArch::Universal => {
                if cfg!(target_arch = "aarch64") {
                    "Apple Silicon"
                } else {
                    "Intel, via Rosetta"
                }
            }
            LiveArch::Unknown => "unknown architecture",
        }
    }
}

/// A detected Live install.
#[derive(Debug, Clone)]
pub struct LiveInstall {
    pub app: PathBuf,
    /// Raw `CFBundleShortVersionString`, e.g. `"12.4.5b3"` (may carry a beta suffix).
    pub version: String,
    pub arch: LiveArch,
    /// The resolved host module path, whichever layout exists.
    pub host_module: Option<PathBuf>,
    pub host_layout: Option<HostLayout>,
    /// Live's bundled node next to the host module, if present.
    pub bundled_node: Option<PathBuf>,
}

impl LiveInstall {
    /// Best-effort semver parse of the version string (strips a trailing beta tag).
    pub fn semver(&self) -> Option<semver::Version> {
        parse_live_version(&self.version)
    }

    /// Whether this install supports Extensions (>= 12.4.5).
    pub fn supports_extensions(&self) -> bool {
        match self.semver() {
            Some(v) => v >= semver::Version::new(12, 4, 5),
            None => self.host_module.is_some(),
        }
    }

    /// The host module path (cached at detect time).
    pub fn host_module(&self) -> Option<PathBuf> {
        self.host_module.clone()
    }
}

/// Detect all Live installs. Honors the `--live`/`$ABLETON_APP` override (a single
/// app) before scanning `/Applications`. Newest-first by directory name (the
/// scaffolder convention; mtime is a 0.3 refinement). Never touches real paths when
/// an override is present (the testability contract).
pub fn detect(ctx: &Ctx) -> Vec<LiveInstall> {
    if let Some(app) = &ctx.ableton_app {
        return vec![inspect(app)];
    }
    let mut apps = scan_applications();
    apps.sort_by(|a, b| b.cmp(a)); // newest-name first (b.localeCompare(a))
    apps.iter().map(|p| inspect(p)).collect()
}

/// The primary Live install: the single detected one, or a pick-list. `RK0303` if
/// none found (or all below 12.4.5).
pub fn primary(ctx: &Ctx) -> CmdResult<LiveInstall> {
    let installs = detect(ctx);
    let usable: Vec<&LiveInstall> = installs
        .iter()
        .filter(|i| i.host_module.is_some())
        .collect();
    match usable.as_slice() {
        [] => Err(no_live_error()),
        [only] => Ok((*only).clone()),
        many => {
            // Multiple: under --no-input pick newest and echo; else pick-list.
            if ctx.no_input {
                let chosen = many[0].clone();
                ui::echo_resolved(
                    "Ableton Live",
                    &chosen.app.display().to_string(),
                    "newest; set --live or ABLETON_APP to override",
                    ctx,
                );
                Ok(chosen)
            } else {
                let labels: Vec<String> =
                    many.iter().map(|i| i.app.display().to_string()).collect();
                let idx = ui::prompt::select("Ableton Live application", &labels, ctx)?;
                Ok(many[idx].clone())
            }
        }
    }
}

/// The `RK0303` no-Live error with the DESIGN §6.2 help line.
pub fn no_live_error() -> RkError {
    RkError::of(
        ErrorCode::NoLiveInstall,
        "No Ableton Live install found",
        "install Live Suite 12.4.5+ and enable the Extensions beta\n\
         (Live → Settings → … → Beta), then rerun `rackabel doctor`.",
    )
}

/// Inspect a single `.app`: version, arch, host layout, bundled node.
pub fn inspect(app: &Path) -> LiveInstall {
    let version = read_short_version(app).unwrap_or_else(|| "unknown".to_string());
    let arch = detect_arch(app);
    let (host_module, host_layout) = probe_host(app);
    let bundled_node = host_module.as_ref().and_then(|hm| {
        let dir = hm.parent()?;
        let node = dir.join(node_basename());
        node.exists().then_some(node)
    });
    LiveInstall {
        app: app.to_path_buf(),
        version,
        arch,
        host_module,
        host_layout,
        bundled_node,
    }
}

/// The node binary basename probed inside the host dir (SPEC A §1.5).
pub fn node_basename() -> &'static str {
    if cfg!(target_os = "windows") {
        "node.exe"
    } else {
        "node"
    }
}

/// Probe both host layouts, returning the first whose `.node` exists.
fn probe_host(app: &Path) -> (Option<PathBuf>, Option<HostLayout>) {
    for layout in [HostLayout::Helpers, HostLayout::AppResources] {
        let dir = app.join(layout.rel());
        let module = dir.join(NODE_MODULE_FILE);
        if module.is_file() {
            return (Some(module), Some(layout));
        }
    }
    (None, None)
}

/// Scan `/Applications` for `Ableton Live*.app` (macOS). Empty elsewhere.
fn scan_applications() -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir("/Applications") else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "app")
                && p.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.starts_with("Ableton Live"))
        })
        .collect()
}

/// Read `CFBundleShortVersionString` from `<app>/Contents/Info.plist`.
fn read_short_version(app: &Path) -> Option<String> {
    let plist_path = app.join("Contents/Info.plist");
    let value = plist::Value::from_file(&plist_path).ok()?;
    let dict = value.as_dictionary()?;
    dict.get("CFBundleShortVersionString")?
        .as_string()
        .map(|s| s.to_string())
}

/// Read the `CFBundleExecutable` name (for the Mach-O arch read).
fn read_executable_name(app: &Path) -> Option<String> {
    let plist_path = app.join("Contents/Info.plist");
    let value = plist::Value::from_file(&plist_path).ok()?;
    let dict = value.as_dictionary()?;
    dict.get("CFBundleExecutable")?
        .as_string()
        .map(|s| s.to_string())
}

/// Determine the app arch by reading the Mach-O (fat) header of the main executable
/// (SPEC C §2 — no extra crate, just the magic + cputype entries).
fn detect_arch(app: &Path) -> LiveArch {
    let Some(exec_name) = read_executable_name(app) else {
        return LiveArch::Unknown;
    };
    let exec = app.join("Contents/MacOS").join(exec_name);
    match read_macho_archs(&exec) {
        Some(archs) => {
            let has_arm = archs.contains(&MachCpu::Arm64);
            let has_x86 = archs.contains(&MachCpu::X86_64);
            match (has_arm, has_x86) {
                (true, true) => LiveArch::Universal,
                (true, false) => LiveArch::AppleSilicon,
                (false, true) => LiveArch::IntelRosetta,
                (false, false) => LiveArch::Unknown,
            }
        }
        None => LiveArch::Unknown,
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum MachCpu {
    Arm64,
    X86_64,
    Other,
}

/// Read the architectures advertised by a Mach-O file. Handles both fat (universal)
/// and thin binaries. Returns `None` if the file can't be read or isn't Mach-O.
fn read_macho_archs(path: &Path) -> Option<Vec<MachCpu>> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 8 {
        return None;
    }
    let magic = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    // Fat (universal) magic, big-endian on disk: 0xCAFEBABE / 0xCAFEBABF (64-bit).
    const FAT_MAGIC: u32 = 0xCAFE_BABE;
    const FAT_MAGIC_64: u32 = 0xCAFE_BABF;
    // Thin Mach-O magics.
    const MH_MAGIC: u32 = 0xFEED_FACE;
    const MH_CIGAM: u32 = 0xCEFA_EDFE;
    const MH_MAGIC_64: u32 = 0xFEED_FACF;
    const MH_CIGAM_64: u32 = 0xCFFA_EDFE;

    if magic == FAT_MAGIC || magic == FAT_MAGIC_64 {
        let nfat = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let entry_size = if magic == FAT_MAGIC_64 { 32 } else { 20 };
        let mut archs = Vec::new();
        for i in 0..nfat {
            let off = 8 + i * entry_size;
            if off + 4 > bytes.len() {
                break;
            }
            let cputype =
                i32::from_be_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
            archs.push(cputype_to_cpu(cputype));
        }
        return Some(archs);
    }

    // Thin binary: read the single cputype after the magic (offset 4), respecting
    // endianness for the cputype field.
    let cputype = match magic {
        MH_MAGIC | MH_MAGIC_64 => i32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        MH_CIGAM | MH_CIGAM_64 => i32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        _ => return None,
    };
    Some(vec![cputype_to_cpu(cputype)])
}

fn cputype_to_cpu(cputype: i32) -> MachCpu {
    // CPU_TYPE_X86_64 = 0x01000007, CPU_TYPE_ARM64 = 0x0100000C.
    const CPU_TYPE_X86_64: i32 = 0x0100_0007;
    const CPU_TYPE_ARM64: i32 = 0x0100_000C;
    match cputype {
        CPU_TYPE_ARM64 => MachCpu::Arm64,
        CPU_TYPE_X86_64 => MachCpu::X86_64,
        _ => MachCpu::Other,
    }
}

/// Parse a Live version string (e.g. `"12.4.5b3 (...)"`) to semver, stripping any
/// non-numeric suffix on the patch component.
fn parse_live_version(s: &str) -> Option<semver::Version> {
    // Take the leading token before whitespace, then split on '.'.
    let head = s.split_whitespace().next()?;
    let mut parts = head.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().unwrap_or("0").parse().unwrap_or(0);
    // Patch may have a beta tag like "5b3"; take the leading digits.
    let patch_raw = parts.next().unwrap_or("0");
    let patch_digits: String = patch_raw
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let patch: u64 = patch_digits.parse().unwrap_or(0);
    Some(semver::Version::new(major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_beta_version() {
        let v = parse_live_version("12.4.5b3").unwrap();
        assert_eq!(v, semver::Version::new(12, 4, 5));
        let v2 = parse_live_version("12.4.5b3 (12345)").unwrap();
        assert_eq!(v2, semver::Version::new(12, 4, 5));
        let v3 = parse_live_version("11.3.0").unwrap();
        assert_eq!(v3, semver::Version::new(11, 3, 0));
    }

    #[test]
    fn supports_extensions_threshold() {
        let mk = |ver: &str| LiveInstall {
            app: PathBuf::from("/x"),
            version: ver.to_string(),
            arch: LiveArch::Universal,
            host_module: Some(PathBuf::from("/x/m.node")),
            host_layout: Some(HostLayout::Helpers),
            bundled_node: None,
        };
        assert!(mk("12.4.5b3").supports_extensions());
        assert!(mk("12.5.0").supports_extensions());
        assert!(!mk("12.4.0").supports_extensions());
        assert!(!mk("11.0.0").supports_extensions());
    }

    #[test]
    fn fat_header_universal() {
        // Build a fake fat header: magic 0xCAFEBABE, nfat=2, arm64 + x86_64.
        let mut bytes = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 2];
        // entry 1: cputype arm64 (0x0100000C) + 16 bytes padding
        bytes.extend_from_slice(&0x0100_000Ci32.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 16]);
        // entry 2: cputype x86_64 (0x01000007) + 16 bytes padding
        bytes.extend_from_slice(&0x0100_0007i32.to_be_bytes());
        bytes.extend_from_slice(&[0u8; 16]);
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("exec");
        std::fs::write(&p, &bytes).unwrap();
        let archs = read_macho_archs(&p).unwrap();
        assert!(archs.contains(&MachCpu::Arm64));
        assert!(archs.contains(&MachCpu::X86_64));
    }

    #[test]
    fn thin_arm64() {
        let mut bytes = vec![0xFE, 0xED, 0xFA, 0xCF]; // MH_MAGIC_64 BE
        bytes.extend_from_slice(&0x0100_000Ci32.to_be_bytes());
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("exec");
        std::fs::write(&p, &bytes).unwrap();
        let archs = read_macho_archs(&p).unwrap();
        assert_eq!(archs, vec![MachCpu::Arm64]);
    }

    #[test]
    fn probe_host_prefers_helpers() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("Ableton Live 12 Beta.app");
        // create both layouts
        let helpers = app.join("Contents/Helpers/ExtensionHost");
        let alpha = app.join("Contents/App-Resources/Extensions/ExtensionHost");
        std::fs::create_dir_all(&helpers).unwrap();
        std::fs::create_dir_all(&alpha).unwrap();
        std::fs::write(helpers.join(NODE_MODULE_FILE), b"").unwrap();
        std::fs::write(alpha.join(NODE_MODULE_FILE), b"").unwrap();
        let (module, layout) = probe_host(&app);
        assert_eq!(layout, Some(HostLayout::Helpers));
        assert!(module.unwrap().starts_with(&helpers));
    }
}
