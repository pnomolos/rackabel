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
        ErrorCode::UsageError => "Command invoked incorrectly",
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

/// Long-form, cargo-`--explain`-style prose for each code: what happened, why it
/// happens, and the concrete next step (pointing at the relevant `doctor`/`--fix`
/// path). Each string is self-contained help text; the lookup is keyed by the same
/// [`ErrorCode`] every error frame carries, so the inline frame and `explain` never
/// drift.
fn long_form(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::UsageError => {
            "The command was invoked incorrectly — for example a required argument was\n\
             missing, a project name already exists, or a flag was given a value that\n\
             isn't supported yet.\n\
             \n\
             This is a usage error (exit 2), not a problem with your project or your\n\
             machine. The framed `help:` line above names the exact fix for the specific\n\
             case (the argument to pass, a different name to choose, or the flag to drop).\n\
             \n\
             To fix:\n\
               - follow the `help:` line printed with the error, or\n\
               - run `rackabel <command> --help` to see the command's accepted arguments."
        }
        ErrorCode::NoManifest => {
            "rackabel looks for a rackabel.toml in the current directory and its\n\
             parents. None was found. Run `rackabel new` to scaffold a project, or\n\
             cd into an existing project directory.\n\
             \n\
             Every rackabel command (build, deploy, pack, validate) runs against a\n\
             project rooted at a rackabel.toml. It is the single source of truth for\n\
             the extension's name, version, entry point, and toolchain.\n\
             \n\
             To fix:\n\
               - `cd` into the project folder (the one holding rackabel.toml), or\n\
               - run `rackabel new <name>` to scaffold a new project here."
        }
        ErrorCode::AmbiguousKind => {
            "A project must declare exactly one of [extension] or [device].\n\
             \n\
             A Live Extension ([extension]) and a Max for Live device ([device]) are\n\
             built and deployed in completely different ways, so a single project can\n\
             only be one of them. Declaring both is ambiguous; declaring neither\n\
             leaves rackabel nothing to build.\n\
             \n\
             To fix:\n\
               - keep exactly one of [extension] / [device] in rackabel.toml, or\n\
               - if you need both, split them into two project folders. A [workspace]\n\
                 root can hold many single-kind member projects."
        }
        ErrorCode::ManifestParse => {
            "rackabel.toml is not valid TOML, or it contains a field rackabel does not\n\
             recognize.\n\
             \n\
             rackabel rejects unknown fields on purpose: a typo'd key (say `verison`\n\
             instead of `version`) would otherwise be silently ignored and you would\n\
             chase a phantom bug. The parse error above points at the offending line.\n\
             \n\
             To fix:\n\
               - correct the TOML syntax, or remove/rename the unknown field, then\n\
                 rerun. Run with --raw to see the underlying parser message."
        }
        ErrorCode::ToolkitNotFound => {
            "The Ableton Extensions toolkit (the SDK + CLI) could not be found.\n\
             \n\
             During the beta, the toolkit is NOT on public npm — it is a separate,\n\
             access-gated download from Ableton. `rackabel new` vendors it into your\n\
             project so builds work offline; it cannot do that until it can find the\n\
             download on your machine.\n\
             \n\
             To fix:\n\
               1. Join / open the Extensions beta and download the toolkit (a .tgz).\n\
               2. Put it (or its already-expanded folder) somewhere easy, e.g.\n\
                  ~/Downloads — rackabel searches recursively and accepts either form.\n\
               3. Re-run pointing at it, e.g. `rackabel new <name> --sdk-dir ~/Downloads`.\n\
             \n\
             `rackabel doctor` reports whether the toolkit is present for a project."
        }
        ErrorCode::ToolkitVersionMismatch => {
            "The Extensions toolkit found on disk does not match the version pinned in\n\
             [toolchain].sdk in your rackabel.toml.\n\
             \n\
             Pinning the SDK keeps a project reproducible: a teammate (or CI) building\n\
             the same commit gets the same SDK. A mismatch means the vendored toolkit\n\
             and the pin disagree.\n\
             \n\
             To fix:\n\
               - update [toolchain].sdk to the version you actually have, or\n\
               - place the pinned toolkit version where rackabel can find it\n\
                 (see `rackabel explain RK0201`)."
        }
        ErrorCode::UserLibraryAmbiguous => {
            "More than one Ableton User Library was found, and --no-input forbids\n\
             prompting, so rackabel will not guess for you.\n\
             \n\
             Deploy copies your built extension into <User Library>/Extensions. With\n\
             several libraries present (e.g. a stable Live and an Alpha/Beta), picking\n\
             the wrong one would deploy somewhere Live never reads. In interactive\n\
             mode rackabel shows a numbered pick-list; under --no-input it refuses to\n\
             pick silently.\n\
             \n\
             To fix:\n\
               - pass `--user-library <path>`, or\n\
               - set the ABLETON_USER_LIBRARY environment variable, or\n\
               - set [host].user_library in rackabel.toml to persist the choice."
        }
        ErrorCode::UserLibraryNotFound => {
            "rackabel could not resolve your Ableton User Library.\n\
             \n\
             The User Library is created by Ableton Live the first time it runs; until\n\
             then there is no <User Library>/Extensions folder to deploy into.\n\
             \n\
             To fix:\n\
               - open Ableton Live once so it creates ~/Music/Ableton…/User Library,\n\
                 then rerun, or\n\
               - point rackabel at it explicitly with `--user-library <path>` (or the\n\
                 ABLETON_USER_LIBRARY environment variable).\n\
             \n\
             `rackabel doctor` shows the resolved User Library and how it was chosen."
        }
        ErrorCode::NoLiveInstall => {
            "No Ableton Live install was found (or all installs are below 12.4.5).\n\
             Install Live Suite 12.4.5+ and enable the Extensions beta, then rerun\n\
             `rackabel doctor`.\n\
             \n\
             Live Extensions require Live Suite 12.4.5 or newer with the Extensions\n\
             beta enabled. rackabel scans /Applications for `Ableton Live*.app`; you\n\
             can also point it at a specific app with `--live <path>` or the\n\
             ABLETON_APP environment variable.\n\
             \n\
             To fix:\n\
               1. Install Ableton Live Suite 12.4.5 or newer.\n\
               2. Enable the Extensions beta (Live → Settings → … → Beta).\n\
               3. Run `rackabel doctor` to confirm it is detected."
        }
        ErrorCode::NativeDepNotCompiled => {
            "This extension declares a native dependency, but its compiled component\n\
             (a .node binary) is missing.\n\
             \n\
             Native deps (e.g. MIDI or Ableton Link bindings) ship as a small C/C++\n\
             addon that must be compiled for your platform. Package managers often\n\
             skip the build step until it is explicitly approved, leaving the .node\n\
             file absent — so the host would fail to load the extension at runtime.\n\
             \n\
             To fix:\n\
               - run `rackabel deploy --fix`, which builds the native dependency for\n\
                 you under the hood (no package-manager commands to memorize). If your\n\
                 toolchain blocks it, the help line names the one command to run.\n\
             \n\
             `rackabel doctor --fix` performs the same native build."
        }
        ErrorCode::NoNodeRuntime => {
            "No usable Node runtime was found to build the extension with.\n\
             \n\
             rackabel drives esbuild (and tsc / `node --check`) through Node. It\n\
             prefers the Node that ships INSIDE Ableton Live, because that guarantees\n\
             the same runtime your extension will actually run under; failing that it\n\
             falls back to a Node on your PATH.\n\
             \n\
             To fix:\n\
               - install Ableton Live 12.4.5+ (it bundles the right Node), or\n\
               - install Node on your PATH for a pre-Live evaluation build.\n\
             If Live is present but its Node is below the runtime floor, upgrade\n\
             LIVE — do not install a separate Node, which would not match the host.\n\
             `rackabel doctor` reports the resolved Node and its source."
        }
        ErrorCode::DeveloperModeOff => {
            "Developer Mode is off in Ableton Live, and the dev loop requires it.\n\
             \n\
             With Developer Mode ON, Live does not start its own Extension Host — it\n\
             lets rackabel run the host instead, which is what makes live-reload and\n\
             log streaming possible. With it OFF, rackabel cannot drive the host.\n\
             \n\
             Developer Mode cannot be read reliably from disk, so rackabel infers it\n\
             from whether Live is running its own host. `rackabel dev` blocks and\n\
             waits, continuing automatically the moment you flip it on.\n\
             \n\
             To fix:\n\
               - turn on Live → Settings → Extensions → Developer Mode, then rerun,\n\
                 or just run `rackabel dev` and toggle it when prompted."
        }
        ErrorCode::BuildFailed => {
            "esbuild failed to bundle the extension.\n\
             \n\
             This is almost always a problem in your source: a syntax error, a\n\
             missing import, or a dependency that is not installed. The bundler output\n\
             above names the file and line.\n\
             \n\
             To fix:\n\
               - fix the error shown above, then rerun the build.\n\
               - run with --raw to see esbuild's full output.\n\
               - if the message is \"couldn't find esbuild\", install the project's\n\
                 dependencies (its package.json pins esbuild), e.g. `npm install`."
        }
        ErrorCode::TypecheckFailed => {
            "The TypeScript typecheck (`tsc --noEmit`) reported errors.\n\
             \n\
             rackabel runs your project's pinned TypeScript before bundling, so type\n\
             errors are caught before they reach Live. Typecheck is on by default for\n\
             `--release` and can be toggled with `--typecheck`/`--no-typecheck`.\n\
             \n\
             To fix:\n\
               - fix the reported type errors, or\n\
               - pass --no-typecheck for a quick iteration (not recommended for a\n\
                 release build).\n\
             If the message is \"couldn't find the project's TypeScript\", install the\n\
             project's dependencies (its package.json pins typescript)."
        }
        ErrorCode::BundleSanity => {
            "The built bundle failed a sanity check.\n\
             \n\
             After bundling, rackabel runs `node --check` on the output to confirm it\n\
             is valid JavaScript the host can load. (A very small bundle is reported\n\
             as a warning, not a failure — a minimal extension can legitimately be\n\
             small.) A `node --check` failure means the build did not produce loadable\n\
             code, which usually points at a bug in the build pipeline itself.\n\
             \n\
             To fix:\n\
               - rebuild with --clean, and run with --raw to see the checker output.\n\
               - if it persists, please report it: this should not happen for a\n\
                 valid source tree."
        }
        ErrorCode::DeployCopyFailed => {
            "rackabel could not copy the built extension into the User Library.\n\
             \n\
             Deploy writes manifest.json + dist/extension.js into\n\
             <User Library>/Extensions/<slug>. A copy failure is almost always a\n\
             filesystem-permissions problem on that folder; the error above names the\n\
             exact path.\n\
             \n\
             To fix:\n\
               - check write permissions on the named folder, then rerun.\n\
               - confirm the resolved User Library is the one you expect with\n\
                 `rackabel doctor` (it echoes the path and how it was chosen)."
        }
        ErrorCode::PackFailed => {
            "Packaging the .ablx archive failed.\n\
             \n\
             For pure-JS extensions, rackabel shells out to the official\n\
             `extensions-cli package`; its non-zero exit status is wrapped into this\n\
             error so you get a framed message instead of a bare CLI dump.\n\
             \n\
             To fix:\n\
               - run with --raw to see the underlying packager output.\n\
               - confirm the build succeeded first (`rackabel build`), since pack\n\
                 packages the built bundle.\n\
               - `--no-official-cli` forces rackabel's own packer if the official one\n\
                 is the problem."
        }
        ErrorCode::ManifestIncomplete => {
            "A required manifest field is missing or empty after inference.\n\
             \n\
             A distributable extension needs all five SDK fields: name, author,\n\
             entry, version, and minimumApiVersion. rackabel infers most of these\n\
             (name from the folder, version 0.1.0, entry src/extension.ts), but it\n\
             will not invent an author — a real one must be set for something you ship.\n\
             \n\
             To fix:\n\
               - set the missing field(s) under [extension] in rackabel.toml. For the\n\
                 author, `[extension].author = \"Your Name\"` (or set git user.name).\n\
             `rackabel validate` lists exactly which fields are missing."
        }
        ErrorCode::ApiVersionTooHigh => {
            "The extension's minimumApiVersion is higher than the host's apiVersion.\n\
             \n\
             minimumApiVersion is the oldest Extensions API the extension can run on.\n\
             If it exceeds what the installed Live host provides, the host refuses to\n\
             load the extension — and an incompatible value can even prevent OTHER\n\
             extensions from loading.\n\
             \n\
             To fix:\n\
               - lower [extension].minimum_api_version to a version the host supports,\n\
                 or\n\
               - upgrade Ableton Live so its host provides the newer API.\n\
             `rackabel doctor` shows the host's detected apiVersion."
        }
        ErrorCode::VersionNotBumped => {
            "The current version is not newer than the last version you packed.\n\
             \n\
             rackabel records the last packed version in .rackabel/state.toml. If you\n\
             pack again with the same (or an older) version, existing users would not\n\
             receive the update, and two different builds would share one version\n\
             number — a support nightmare.\n\
             \n\
             To fix:\n\
               - bump [extension].version (e.g. 1.2.0 → 1.2.1 for a fix, → 1.3.0 for\n\
                 a feature), then rerun. `rackabel validate` reports the last packed\n\
                 version it is comparing against."
        }
        ErrorCode::IncludeInvalid => {
            "An --include path is invalid.\n\
             \n\
             pack only bundles files from inside the extension directory, by a\n\
             relative path, and only if they exist. An absolute path, a path that\n\
             escapes the project (../…), or a non-existent file is rejected before any\n\
             archive is written.\n\
             \n\
             To fix:\n\
               - use a relative path that stays inside the project and points at a\n\
                 file/dir that exists, e.g. `--include assets/icon.png`."
        }
        ErrorCode::IdentifierDrift => {
            "A stable command identifier appears to have changed between releases.\n\
             \n\
             Command ids are a compatibility contract: Live keys a user's saved state\n\
             (key bindings, customizations) off the id. Removing or renaming one\n\
             silently breaks existing users' setups.\n\
             \n\
             Note: in this milestone command ids are registered in code at runtime and\n\
             are NOT present in the on-disk manifest, so rackabel cannot yet diff them\n\
             automatically — validate reports this rule as \"not checkable yet\". When\n\
             you do rename an id, keep the old id working (or ship a migration) so\n\
             existing setups survive.\n\
             \n\
             Under `rackabel validate --strict` this rule is fatal once it has\n\
             something to compare."
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
