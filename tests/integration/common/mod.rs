//! Shared test scaffolding (SPEC C §5): a fake Live `.app` builder, a fake User
//! Library, a fake toolkit dropper, and the env wiring that keeps tests off the
//! real machine. Every test sets `RACKABEL_HOME` and the relevant `ABLETON_*`
//! overrides to temp dirs — tests NEVER write to the real User Library, the real
//! `~/.rackabel`, or the ground-truth repos.

#![allow(dead_code)] // Each integration binary uses a subset of these helpers.

use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use tempfile::TempDir;

/// The arch a fake Live `.app` advertises in its Mach-O header.
#[derive(Clone, Copy)]
pub enum FakeArch {
    AppleSilicon,
    Intel,
    Universal,
}

/// Which Extension Host layout the fake `.app` uses.
#[derive(Clone, Copy)]
pub enum FakeLayout {
    /// Modern `Contents/Helpers/ExtensionHost`.
    Helpers,
    /// Legacy `Contents/App-Resources/Extensions/ExtensionHost`.
    AppResources,
}

/// A fabricated minimal Live `.app` tree under a temp dir.
pub struct FakeLive {
    pub root: TempDir,
    pub app: PathBuf,
}

impl FakeLive {
    /// Create `<tmp>/Ableton Live 12 Beta.app` with an Info.plist (version +
    /// executable name), a Mach-O executable with the requested arch header, the
    /// host module placeholder, and an executable bundled-node stub that prints a
    /// node version.
    pub fn new(version: &str, arch: FakeArch, layout: FakeLayout) -> Self {
        let root = TempDir::new().expect("tempdir");
        let app = root.path().join("Ableton Live 12 Beta.app");

        // Info.plist
        let contents = app.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        let plist = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\"><dict>\n\
             <key>CFBundleShortVersionString</key><string>{version}</string>\n\
             <key>CFBundleExecutable</key><string>Live</string>\n\
             </dict></plist>\n"
        );
        std::fs::write(contents.join("Info.plist"), plist).unwrap();

        // Mach-O executable with a fat/thin header advertising the arch.
        let macos = contents.join("MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("Live"), macho_bytes(arch)).unwrap();

        // Host module + bundled node stub.
        let host_rel = match layout {
            FakeLayout::Helpers => "Contents/Helpers/ExtensionHost",
            FakeLayout::AppResources => "Contents/App-Resources/Extensions/ExtensionHost",
        };
        let host_dir = app.join(host_rel);
        std::fs::create_dir_all(&host_dir).unwrap();
        std::fs::write(host_dir.join("ExtensionHostNodeModule.node"), b"").unwrap();

        let node = host_dir.join("node");
        std::fs::write(&node, "#!/bin/sh\necho v24.14.1\n").unwrap();
        make_executable(&node);

        Self { root, app }
    }

    pub fn app_path(&self) -> &Path {
        &self.app
    }
}

/// A fabricated User Library with an `Extensions/` folder.
pub struct FakeUserLibrary {
    pub root: TempDir,
    pub library: PathBuf,
}

impl FakeUserLibrary {
    pub fn new() -> Self {
        let root = TempDir::new().expect("tempdir");
        let library = root.path().join("User Library");
        std::fs::create_dir_all(library.join("Extensions")).unwrap();
        Self { root, library }
    }

    pub fn path(&self) -> &Path {
        &self.library
    }
}

impl Default for FakeUserLibrary {
    fn default() -> Self {
        Self::new()
    }
}

/// Drop minimal valid-shaped SDK + CLI tarball files into `dir` so toolkit
/// discovery finds them (it checks filename + extension; the contents aren't
/// inspected in 0.2 foundation tests).
pub fn fake_toolkit(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("ableton-extensions-sdk-1.0.0-beta.0.tgz"), b"fake").unwrap();
    std::fs::write(dir.join("ableton-extensions-cli-1.0.0-beta.0.tgz"), b"fake").unwrap();
}

/// A `rackabel` command pre-wired for hermetic tests: `RACKABEL_HOME`,
/// `NO_COLOR`, `--no-input`, and a `HOME` pinned under `home`. Callers add
/// `ABLETON_*` overrides and the subcommand args.
pub fn rackabel_cmd(home: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::cargo_bin("rackabel").expect("binary built");
    cmd.current_dir(cwd)
        .env("RACKABEL_HOME", home.join(".rackabel"))
        .env("HOME", home)
        .env("NO_COLOR", "1")
        // Clear inherited Ableton overrides so a developer's real machine env can't
        // leak into a test.
        .env_remove("ABLETON_APP")
        .env_remove("ABLETON_USER_LIBRARY")
        .env_remove("ABLETON_EH_MOD")
        .env_remove("ABLETON_EH_NODE")
        .env_remove("ABLETON_EXTENSIONS_DIR")
        .env_remove("ABLETON_STORAGE_BASE");
    cmd
}

/// Copy the committed `tests/fixtures/<name>` project into a fresh temp dir and
/// return both the holding `TempDir` and the project root inside it. Tests mutate the
/// copy (build writes `dist/`, `manifest.json`, `.rackabel/`) without touching the
/// committed fixture.
pub fn fixture_project(name: &str) -> (TempDir, PathBuf) {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let tmp = TempDir::new().expect("tempdir");
    let dst = tmp.path().join(name);
    copy_dir(&src, &dst);
    (tmp, dst)
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

/// Resolve a real `node` on `PATH` that can `require.resolve("esbuild")` from the
/// given project root. Returns the node path if a usable node+esbuild pair exists, so
/// real end-to-end build tests can gate on it (and skip with a clear message
/// otherwise — esbuild may be unavailable in CI).
pub fn usable_node_with_esbuild(project_root: &Path) -> Option<PathBuf> {
    let node = which_node()?;
    let probe = format!(
        "try {{ require.resolve('esbuild', {{ paths: [{:?}] }}); process.exit(0); }} \
         catch (e) {{ process.exit(7); }}",
        project_root.to_string_lossy()
    );
    let ok = Command::new(&node)
        .args(["-e", &probe])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok { Some(node) } else { None }
}

/// A `node` on PATH (regardless of esbuild). Used to gate `node --check`-only paths.
pub fn which_node() -> Option<PathBuf> {
    let out = Command::new("sh")
        .args(["-c", "command -v node"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if p.is_empty() {
        None
    } else {
        Some(PathBuf::from(p))
    }
}

/// Symlink (or copy) a resolvable `esbuild` package into `<project_root>/node_modules`
/// so a hermetic build can find it without a network `npm install`. Resolves the
/// esbuild package directory via the given `node`. Returns `true` on success.
pub fn vendor_esbuild_into(node: &Path, source_root: &Path, project_root: &Path) -> bool {
    // Find esbuild's package dir reachable from `source_root`.
    let script = format!(
        "const path=require('node:path'); \
         try {{ const p=require.resolve('esbuild/package.json',{{paths:[{:?}]}}); \
         process.stdout.write(path.dirname(p)); }} catch(e) {{ process.exit(7); }}",
        source_root.to_string_lossy()
    );
    let out = match Command::new(node).args(["-e", &script]).output() {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };
    let pkg_dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
    if !pkg_dir.is_dir() {
        return false;
    }
    let nm = project_root.join("node_modules");
    std::fs::create_dir_all(&nm).ok();
    let link = nm.join("esbuild");
    #[cfg(unix)]
    {
        let _ = std::os::unix::fs::symlink(&pkg_dir, &link);
    }
    #[cfg(not(unix))]
    {
        copy_dir(&pkg_dir, &link);
    }
    link.exists()
}

/// Build the bytes of a tiny Mach-O (fat or thin) header for the given arch.
fn macho_bytes(arch: FakeArch) -> Vec<u8> {
    const CPU_X86_64: i32 = 0x0100_0007;
    const CPU_ARM64: i32 = 0x0100_000C;
    match arch {
        FakeArch::Universal => {
            let mut b = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 2];
            b.extend_from_slice(&CPU_ARM64.to_be_bytes());
            b.extend_from_slice(&[0u8; 16]);
            b.extend_from_slice(&CPU_X86_64.to_be_bytes());
            b.extend_from_slice(&[0u8; 16]);
            b
        }
        FakeArch::AppleSilicon => thin(CPU_ARM64),
        FakeArch::Intel => thin(CPU_X86_64),
    }
}

fn thin(cputype: i32) -> Vec<u8> {
    // MH_MAGIC_64 big-endian, then cputype.
    let mut b = vec![0xFE, 0xED, 0xFA, 0xCF];
    b.extend_from_slice(&cputype.to_be_bytes());
    b
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}
