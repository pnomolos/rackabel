//! `rackabel explain <code>` — long-form help for an error code (cargo --explain).
//!
//! OWNED BY THE VALIDATE+EXPLAIN AGENT, who maintains the long-form prose. The
//! foundation lands a working table keyed by the frozen [`ErrorCode`] enum so the
//! command works on day one and stays in sync with the codes used in error frames
//! (every `RkError` call site references the same `ErrorCode`). The owner may
//! expand the prose; the lookup mechanism is stable.

use crate::cli::ExplainArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};

pub fn run(args: &ExplainArgs, _ctx: &Ctx) -> CmdResult<()> {
    let Some(code) = ErrorCode::from_str(&args.code) else {
        let valid: Vec<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        return Err(RkError::new(
            // Usage class: an unknown code is a usage mistake, not an environment one.
            ErrorCode::NoManifest,
            ExitClass::Usage,
            format!("no such error code `{}`", args.code),
            format!("valid codes: {}", valid.join(", ")),
        ));
    };

    println!("{} — {}", code.as_str(), short_title(code));
    println!();
    println!("{}", long_form(code));
    Ok(())
}

/// A one-line title for the code.
fn short_title(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::NoManifest => "No rackabel.toml found",
        ErrorCode::AmbiguousKind => "Project has both or neither [extension]/[device]",
        ErrorCode::ManifestParse => "rackabel.toml could not be parsed",
        ErrorCode::ToolkitNotFound => "Extensions toolkit (SDK/CLI) not found",
        ErrorCode::ToolkitVersionMismatch => "Toolkit version mismatch vs [toolchain].sdk",
        ErrorCode::UserLibraryAmbiguous => "Multiple User Libraries; cannot pick under --no-input",
        ErrorCode::UserLibraryNotFound => "User Library not resolvable",
        ErrorCode::NoLiveInstall => "No Ableton Live install found",
        ErrorCode::NativeDepNotCompiled => "A native dependency is not compiled",
        ErrorCode::NoNodeRuntime => "No usable node runtime for build",
        ErrorCode::DeveloperModeOff => "Developer Mode is off",
        ErrorCode::BuildFailed => "esbuild bundle failed",
        ErrorCode::TypecheckFailed => "tsc --noEmit typecheck failed",
        ErrorCode::BundleSanity => "Bundle sanity check failed",
        ErrorCode::DeployCopyFailed => "Deploy copy failed",
        ErrorCode::PackFailed => "Pack failed",
        ErrorCode::ManifestIncomplete => "Manifest is incomplete",
        ErrorCode::ApiVersionTooHigh => "minimumApiVersion exceeds the host's apiVersion",
        ErrorCode::VersionNotBumped => "Version not bumped vs the last packed version",
        ErrorCode::IncludeInvalid => "An --include path is invalid",
        ErrorCode::IdentifierDrift => "A stable command identifier changed",
    }
}

/// Long-form prose. The validate+explain-owner is expected to enrich these; the
/// foundation seeds each with an accurate, actionable paragraph.
fn long_form(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::NoManifest => {
            "rackabel looks for a rackabel.toml in the current directory and its\n\
             parents. None was found. Run `rackabel new` to scaffold a project, or\n\
             cd into an existing project directory."
        }
        ErrorCode::AmbiguousKind => {
            "A project must declare exactly one of [extension] or [device]. Declaring\n\
             both is ambiguous; declaring neither leaves nothing to build. A workspace\n\
             can hold many single-kind member projects."
        }
        ErrorCode::ManifestParse => {
            "rackabel.toml is not valid TOML, or contains an unknown field. The parse\n\
             error points at the offending line. Fix the syntax (or remove the unknown\n\
             field) and rerun."
        }
        ErrorCode::ToolkitNotFound => {
            "The Ableton Extensions toolkit (the SDK + CLI tarballs) was not found.\n\
             It is a separate, beta-gated download from Ableton. Place the .tgz (or its\n\
             expanded folder) somewhere like ~/Downloads and rerun with --sdk-dir."
        }
        ErrorCode::ToolkitVersionMismatch => {
            "The discovered toolkit version does not match [toolchain].sdk in your\n\
             rackabel.toml. Update [toolchain].sdk, or place the expected toolkit\n\
             version."
        }
        ErrorCode::UserLibraryAmbiguous => {
            "More than one Live User Library was found and --no-input forbids prompting.\n\
             Choose one explicitly with --user-library, or set ABLETON_USER_LIBRARY."
        }
        ErrorCode::UserLibraryNotFound => {
            "rackabel could not resolve your Live User Library. Open Ableton Live once so\n\
             it creates ~/Music/Ableton…/User Library (with an Extensions folder), or\n\
             point at it with --user-library."
        }
        ErrorCode::NoLiveInstall => {
            "No Ableton Live install was found (or all installs are below 12.4.5).\n\
             Install Live Suite 12.4.5+ and enable the Extensions beta, then rerun\n\
             `rackabel doctor`."
        }
        ErrorCode::NativeDepNotCompiled => {
            "An extension declares a native dependency whose compiled component\n\
             (.node binary) is missing. Run `rackabel deploy --fix` to build it under\n\
             the hood."
        }
        ErrorCode::NoNodeRuntime => {
            "No usable node runtime was found for the build. rackabel prefers Live's\n\
             bundled node; if Live is below the runtime floor, upgrade Live (do not\n\
             install Node separately)."
        }
        ErrorCode::DeveloperModeOff => {
            "Developer Mode is off in Live, which the dev loop requires. Open\n\
             Live → Settings → Extensions → turn on Developer Mode, then rerun — or run\n\
             `rackabel dev`, which waits for it."
        }
        ErrorCode::BuildFailed => {
            "esbuild failed to bundle the extension. The bundler output names the file\n\
             and line. Run with --raw to see the full bundler output."
        }
        ErrorCode::TypecheckFailed => {
            "`tsc --noEmit` reported type errors. Fix the reported errors, or build\n\
             without --typecheck for a quick iteration (typecheck is on by default for\n\
             --release)."
        }
        ErrorCode::BundleSanity => {
            "The built bundle failed a sanity check (too small, or `node --check`\n\
             rejected it). This usually means the build did not produce a real bundle.\n\
             Rebuild with --clean and inspect the output."
        }
        ErrorCode::DeployCopyFailed => {
            "rackabel could not copy the build into the User Library folder. The error\n\
             names the folder; check its write permissions."
        }
        ErrorCode::PackFailed => {
            "Packaging failed. When shelling to the official extensions-cli, its exit\n\
             status is wrapped here; run with --raw to see its output."
        }
        ErrorCode::ManifestIncomplete => {
            "One of the required manifest fields (name, author, entry, version,\n\
             minimumApiVersion) is missing or empty after inference. Set the missing\n\
             field in rackabel.toml."
        }
        ErrorCode::ApiVersionTooHigh => {
            "The extension's minimumApiVersion is higher than the detected host's\n\
             apiVersion, so the host cannot load it. Lower minimumApiVersion or upgrade\n\
             Live."
        }
        ErrorCode::VersionNotBumped => {
            "The current version matches the last packed version. Bump [extension].version\n\
             so existing users receive an update."
        }
        ErrorCode::IncludeInvalid => {
            "An --include path must be relative and stay inside the extension directory,\n\
             and must exist. Fix the path and rerun."
        }
        ErrorCode::IdentifierDrift => {
            "A command identifier present in the last packed manifest has been removed or\n\
             renamed. This breaks existing users' saved state — keep the old id or\n\
             provide a migration."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_has_prose() {
        for &code in ErrorCode::ALL {
            assert!(!short_title(code).is_empty());
            assert!(!long_form(code).is_empty());
        }
    }
}
