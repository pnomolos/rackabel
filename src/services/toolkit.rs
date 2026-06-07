//! Extensions toolkit discovery (DESIGN §4, §6.2; SPEC C §3.7).
//!
//! The SDK is a separate, beta-gated download. `rackabel new` finds the SDK + CLI
//! tarballs by a *recursive* search of the download dir (or `--sdk-dir`) and
//! tolerates every shape a non-developer ends up with — the raw `.tgz`, an already
//! expanded toolkit *folder*, or a vendor folder dropped anywhere. If both a `.tgz`
//! and an expanded form exist it picks the expanded/newer one and echoes which.
//! `RK0201` (not found) drives the §6.2 guidance text.

use std::path::{Path, PathBuf};

use crate::error::{CmdResult, ErrorCode, RkError};
use crate::manifest::Project;

/// The two pieces of the toolkit.
#[derive(Debug, Clone)]
pub struct Toolkit {
    pub sdk: ToolkitItem,
    pub cli: ToolkitItem,
    /// The search root the toolkit was found under (for echo).
    pub root: PathBuf,
}

/// One discovered toolkit piece.
#[derive(Debug, Clone)]
pub struct ToolkitItem {
    pub path: PathBuf,
    pub form: ToolkitForm,
    /// Version parsed from the filename, if present (e.g. `"1.0.0-beta.0"`).
    pub version: Option<String>,
}

/// Whether a piece is a tarball or an already-expanded folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolkitForm {
    Tarball,
    Expanded,
}

/// The npm package identities we look for (the vendored tarball names are
/// authoritative — SPEC C §0).
const SDK_NEEDLE: &str = "ableton-extensions-sdk";
const CLI_NEEDLE: &str = "ableton-extensions-cli";

/// Recursively search `search_dir` for both the SDK and the CLI (tarball or
/// expanded folder), at any depth. Prefers the expanded/newer form. `RK0201` if
/// either piece is missing.
pub fn discover(search_dir: &Path) -> CmdResult<Toolkit> {
    let sdk_hits = find_pieces(search_dir, SDK_NEEDLE);
    let cli_hits = find_pieces(search_dir, CLI_NEEDLE);

    let sdk = pick_best(sdk_hits);
    let cli = pick_best(cli_hits);

    match (sdk, cli) {
        (Some(sdk), Some(cli)) => Ok(Toolkit {
            sdk,
            cli,
            root: search_dir.to_path_buf(),
        }),
        _ => Err(toolkit_not_found(search_dir)),
    }
}

/// The `RK0201` toolkit-not-found error with the DESIGN §6.2 guidance.
pub fn toolkit_not_found(search_dir: &Path) -> RkError {
    RkError::of(
        ErrorCode::ToolkitNotFound,
        "Couldn't find the Ableton Extensions toolkit download",
        "It's a separate file from Ableton, only available if you've joined the\n\
         Extensions beta. Once you have the toolkit file:\n\
         1. Join / open the beta at https://www.ableton.com/extensions-beta\n\
            (if that page has moved, search Ableton's site for \"Extensions beta\").\n\
         2. Download the toolkit file (it ends in .tgz).\n\
         3. Put it (or its folder) anywhere easy, e.g. your Downloads folder.\n\
         Then re-run pointing at where you saved it, e.g.:\n\
            rackabel new <name> --sdk-dir ~/Downloads",
    )
    .at(format!("searched {}", search_dir.display()))
}

/// Default search roots when `--sdk-dir` is absent: `~/Downloads`, cwd, project root.
pub fn default_search_roots(project: Option<&Project>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home::home_dir() {
        roots.push(home.join("Downloads"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Some(p) = project {
        roots.push(p.root.clone());
    }
    roots.dedup();
    roots
}

/// Vendor the discovered toolkit into `<project_root>/vendor` and (caller wires the
/// `file:` deps). For 0.2 the foundation provides the copy; the `new`-owner wires the
/// generated `package.json`. Tarballs are copied verbatim; expanded folders are
/// re-tarred is *not* done here (the generator references `.tgz`), so an expanded
/// form is copied as a directory and the new-owner decides how to reference it.
pub fn vendor_into(tk: &Toolkit, project_root: &Path) -> CmdResult<()> {
    let vendor = project_root.join("vendor");
    std::fs::create_dir_all(&vendor).map_err(|e| {
        RkError::of(
            ErrorCode::ToolkitNotFound,
            "could not create the vendor directory",
            "check write permissions for the project directory",
        )
        .at(vendor.display().to_string())
        .raw(e.into())
    })?;
    for item in [&tk.sdk, &tk.cli] {
        let dest = vendor.join(item.path.file_name().unwrap_or_default());
        copy_item(item, &dest)?;
    }
    Ok(())
}

fn copy_item(item: &ToolkitItem, dest: &Path) -> CmdResult<()> {
    match item.form {
        ToolkitForm::Tarball => std::fs::copy(&item.path, dest).map(|_| ()).map_err(|e| {
            RkError::of(
                ErrorCode::ToolkitNotFound,
                "could not vendor the toolkit tarball",
                "check that the source file is readable and the project is writable",
            )
            .at(item.path.display().to_string())
            .raw(e.into())
        }),
        ToolkitForm::Expanded => copy_dir_recursive(&item.path, dest),
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> CmdResult<()> {
    std::fs::create_dir_all(dst).map_err(io_err(dst))?;
    for entry in std::fs::read_dir(src).map_err(io_err(src))? {
        let entry = entry.map_err(io_err(src))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .map(|_| ())
                .map_err(io_err(&from))?;
        }
    }
    Ok(())
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> RkError {
    let path = path.to_path_buf();
    move |e| {
        RkError::of(
            ErrorCode::ToolkitNotFound,
            "could not copy a toolkit file",
            "check filesystem permissions and free space",
        )
        .at(path.display().to_string())
        .raw(e.into())
    }
}

/// Find all candidate paths for a needle (`.tgz` files and expanded folders).
fn find_pieces(search_dir: &Path, needle: &str) -> Vec<ToolkitItem> {
    let mut hits = Vec::new();
    for entry in walkdir::WalkDir::new(search_dir)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        let name = entry.file_name().to_string_lossy();
        if !name.contains(needle) {
            continue;
        }
        let path = entry.path().to_path_buf();
        if entry.file_type().is_file() && name.ends_with(".tgz") {
            hits.push(ToolkitItem {
                version: parse_version_from_name(&name),
                path,
                form: ToolkitForm::Tarball,
            });
        } else if entry.file_type().is_dir() {
            // An expanded toolkit folder: npm tarballs expand to a `package/` dir,
            // but the folder may also be named after the package. Accept a dir that
            // contains a package.json (the expanded npm layout) OR a `package/` child.
            let has_pkg = path.join("package.json").is_file()
                || path.join("package").join("package.json").is_file();
            if has_pkg {
                hits.push(ToolkitItem {
                    version: parse_version_from_name(&name),
                    path,
                    form: ToolkitForm::Expanded,
                });
            }
        }
    }
    hits
}

/// Pick the best candidate: prefer Expanded over Tarball, then newer version, then
/// shallower path (more likely the intended drop).
fn pick_best(mut hits: Vec<ToolkitItem>) -> Option<ToolkitItem> {
    if hits.is_empty() {
        return None;
    }
    hits.sort_by(|a, b| {
        // Expanded first.
        let form_rank = |f: ToolkitForm| match f {
            ToolkitForm::Expanded => 0,
            ToolkitForm::Tarball => 1,
        };
        form_rank(a.form)
            .cmp(&form_rank(b.form))
            // Then newer version (descending).
            .then_with(|| b.version.cmp(&a.version))
    });
    hits.into_iter().next()
}

/// Parse a trailing `-X.Y.Z[-tag]` version from a filename/folder name.
fn parse_version_from_name(name: &str) -> Option<String> {
    // Strip a trailing `.tgz`.
    let stem = name.strip_suffix(".tgz").unwrap_or(name);
    // Find the last `-` followed by a digit.
    let bytes = stem.as_bytes();
    for (i, w) in bytes.windows(2).enumerate() {
        if w[0] == b'-' && w[1].is_ascii_digit() {
            return Some(stem[i + 1..].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn touch(p: &Path) {
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, b"x").unwrap();
    }

    #[test]
    fn parses_versions() {
        assert_eq!(
            parse_version_from_name("ableton-extensions-sdk-1.0.0-beta.0.tgz").as_deref(),
            Some("1.0.0-beta.0")
        );
        assert_eq!(
            parse_version_from_name("ableton-extensions-cli").as_deref(),
            None
        );
    }

    #[test]
    fn discovers_tarballs_at_depth() {
        let tmp = tempdir().unwrap();
        let nested = tmp.path().join("a/b");
        touch(&nested.join("ableton-extensions-sdk-1.0.0-beta.0.tgz"));
        touch(&nested.join("ableton-extensions-cli-1.0.0-beta.0.tgz"));
        let tk = discover(tmp.path()).unwrap();
        assert_eq!(tk.sdk.form, ToolkitForm::Tarball);
        assert_eq!(tk.cli.form, ToolkitForm::Tarball);
        assert_eq!(tk.sdk.version.as_deref(), Some("1.0.0-beta.0"));
    }

    #[test]
    fn missing_piece_is_rk0201() {
        let tmp = tempdir().unwrap();
        touch(&tmp.path().join("ableton-extensions-sdk-1.0.0-beta.0.tgz"));
        // No CLI.
        let err = discover(tmp.path()).unwrap_err();
        assert_eq!(err.code, ErrorCode::ToolkitNotFound);
    }

    #[test]
    fn prefers_expanded_over_tarball() {
        let tmp = tempdir().unwrap();
        // Tarball.
        touch(&tmp.path().join("ableton-extensions-sdk-1.0.0-beta.0.tgz"));
        // Expanded folder with a package.json.
        let folder = tmp.path().join("ableton-extensions-sdk-expanded");
        touch(&folder.join("package.json"));
        // CLI needs to be present too.
        touch(&tmp.path().join("ableton-extensions-cli-1.0.0-beta.0.tgz"));
        let tk = discover(tmp.path()).unwrap();
        assert_eq!(tk.sdk.form, ToolkitForm::Expanded);
    }

    #[test]
    fn vendor_copies_tarballs() {
        let tmp = tempdir().unwrap();
        touch(&tmp.path().join("ableton-extensions-sdk-1.0.0-beta.0.tgz"));
        touch(&tmp.path().join("ableton-extensions-cli-1.0.0-beta.0.tgz"));
        let tk = discover(tmp.path()).unwrap();
        let proj = tmp.path().join("proj");
        fs::create_dir_all(&proj).unwrap();
        vendor_into(&tk, &proj).unwrap();
        assert!(
            proj.join("vendor/ableton-extensions-sdk-1.0.0-beta.0.tgz")
                .is_file()
        );
        assert!(
            proj.join("vendor/ableton-extensions-cli-1.0.0-beta.0.tgz")
                .is_file()
        );
    }
}
