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
        ErrorCode::NoSuchExtension => "No registered extension by that name or path",
        ErrorCode::PluginShadowedByBuiltin => "A built-in subcommand shadows the plugin name",
        ErrorCode::PluginNotFound => "No plugin by that name is installed or on PATH",
        ErrorCode::TemplateNotFound => "The template ref could not be resolved",
        ErrorCode::TemplateFetchDeclined => "A remote fetch was declined (no confirmation)",
        ErrorCode::NoNetwork => "Could not reach the network (or hit a rate limit)",
        ErrorCode::ToolkitNotFound => "Extensions toolkit (SDK/CLI) not found",
        ErrorCode::ToolkitVersionMismatch => "Toolkit version mismatch vs [toolchain].sdk",
        ErrorCode::UserLibraryAmbiguous => "Multiple User Libraries; cannot pick under --no-input",
        ErrorCode::UserLibraryNotFound => "User Library not resolvable",
        ErrorCode::NoLiveInstall => "No Ableton Live install found",
        ErrorCode::NativeDepNotCompiled => "A native dependency is not compiled",
        ErrorCode::NoNodeRuntime => "No usable node runtime for build",
        ErrorCode::DeveloperModeOff => "Developer Mode is off",
        ErrorCode::DaemonStartFailed => "The dev host daemon did not start",
        ErrorCode::ProtocolMismatch => "Dev host protocol version mismatch",
        ErrorCode::NoDaemon => "No dev host is running",
        ErrorCode::HostCrashLooping => "The Extension Host keeps crashing",
        ErrorCode::RegistryLocked => "Could not lock the registry",
        ErrorCode::NameCollision => "A registry name collides with a reserved verb or entry",
        ErrorCode::BuildFailed => "esbuild bundle failed",
        ErrorCode::TypecheckFailed => "tsc --noEmit typecheck failed",
        ErrorCode::BundleSanity => "Bundle sanity check failed",
        ErrorCode::DeployCopyFailed => "Deploy copy failed",
        ErrorCode::PackFailed => "Pack failed",
        ErrorCode::ReloadActivateFailed => "An extension threw in activate() on reload",
        ErrorCode::HostLaunchFailed => "The Extension Host failed to launch",
        ErrorCode::TestFailed => "One or more `dev test` targets had failing tests",
        ErrorCode::ManifestIncomplete => "Manifest is incomplete",
        ErrorCode::ApiVersionTooHigh => "minimumApiVersion exceeds the host's apiVersion",
        ErrorCode::VersionNotBumped => "Version not bumped vs the last packed version",
        ErrorCode::IncludeInvalid => "An --include path is invalid",
        ErrorCode::IdentifierDrift => "A stable command identifier changed",
        ErrorCode::SkippedIncompatible => {
            "An extension was skipped as host-incompatible (--strict)"
        }
        ErrorCode::PinMismatch => "Installed plugin code does not match its lockfile pin",
        ErrorCode::UpdateConflicts => "A template update produced merge conflicts",
        ErrorCode::HookFailed => "A lifecycle hook exited nonzero (logged and skipped)",
        ErrorCode::PreDeployVetoed => "A pre_deploy hook vetoed the deploy",
        ErrorCode::HookTimeout => "A lifecycle hook exceeded its timeout",
        ErrorCode::HookApiUnsupported => "A plugin declares a hook_api newer than this build",
        ErrorCode::MigrateUnsupported => "No codemod ships for that hook_api migration yet",
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
        ErrorCode::NoSuchExtension => {
            "A `dev` registry verb named or pointed at an extension that is not in the\n\
             registry — almost always a typo'd name or the wrong path.\n\
             \n\
             `dev enable`, `dev disable`, `dev unregister`, and `dev reload <name>`\n\
             resolve their argument against ~/.rackabel/registry.toml (by name first,\n\
             then by path). These verbs work WITHOUT a running dev host, so a miss here\n\
             is a usage mistake (exit 2), not a daemon problem.\n\
             \n\
             To fix:\n\
               - run `rackabel dev list` to see the registered names and paths, then\n\
                 rerun with one of them, or\n\
               - `rackabel dev register <path>` to add the extension first."
        }
        ErrorCode::PluginShadowedByBuiltin => {
            "The name resolves to a BUILT-IN rackabel subcommand, so a third-party\n\
             `rackabel-<name>` on PATH (or in ~/.rackabel/plugins/bin) is shadowed and\n\
             never runs as the bare `rackabel <name>`.\n\
             \n\
             Built-ins always win the namespace (git/cargo behavior, §5.6): a plugin can\n\
             never hijack a blessed verb like `dev` or `build`. This is informational —\n\
             nothing is broken — but the plugin you installed is not reachable by the bare\n\
             name.\n\
             \n\
             To fix:\n\
               - run the plugin via the escape hatch: `rackabel plugin run <name> …`,\n\
                 which invokes the executable even when a built-in claims the name, or\n\
               - rename the plugin so its name does not collide with a built-in.\n\
             `rackabel plugin which <name>` shows exactly what each name resolves to."
        }
        ErrorCode::PluginNotFound => {
            "No plugin by that name is installed in ~/.rackabel/plugins/bin and none is\n\
             on your PATH as `rackabel-<name>`.\n\
             \n\
             `rackabel <foo>` (and `plugin which/run/enable/disable <foo>`) resolve a\n\
             non-built-in token to an executable named `rackabel-<foo>`, searching the\n\
             managed bin first, then $PATH (§5.1). A miss means the plugin isn't present\n\
             where rackabel looks.\n\
             \n\
             To fix:\n\
               - `rackabel plugin list` to see what's installed, or\n\
               - `rackabel plugin install OWNER/REPO` (or a local path/tarball to\n\
                 sideload) to add it, or\n\
               - `rackabel plugin search <term>` to find one to install."
        }
        ErrorCode::TemplateNotFound => {
            "The `--template` reference could not be resolved.\n\
             \n\
             A template is a git repo (or local dir) holding a `rackabel-template.toml`.\n\
             rackabel accepts `gh:owner/repo[@ref]`, `@scope/name`, or a local path. This\n\
             error means the repo/ref doesn't exist, or the local path has no\n\
             `rackabel-template.toml` at its root (so it isn't a template).\n\
             \n\
             To fix:\n\
               - check the owner/repo and ref spelling for a remote template, or\n\
               - point `--template` at a directory that contains a rackabel-template.toml,\n\
                 or omit `--template` to use the built-in default (the Persona-A path)."
        }
        ErrorCode::TemplateFetchDeclined => {
            "A remote template or plugin install needs your confirmation before it runs\n\
             unreviewed third-party code, and that confirmation was not given.\n\
             \n\
             `new --template gh:…`/`@scope/…` fetches an arbitrary repo whose build the\n\
             auto-build then executes; `plugin install OWNER/REPO` runs code on first\n\
             install. rackabel prints what it will fetch/run and asks before proceeding\n\
             (§5.5/§5.7). Nothing was fetched or built. Under --no-input the prompt is a\n\
             hard error rather than a silent default.\n\
             \n\
             To fix:\n\
               - rerun and confirm at the prompt, or\n\
               - pass `--yes` to consent non-interactively in a script (this is standing\n\
                 consent to fetch and build the named source), or\n\
               - use the built-in default template / sideload a local path instead."
        }
        ErrorCode::NoNetwork => {
            "A network operation could not reach the network, or a remote API rate-limited\n\
             the request.\n\
             \n\
             `plugin search` queries the GitHub API; `plugin install OWNER/REPO` and a\n\
             remote `--template` fetch a repo or release asset. Any of these can fail\n\
             offline or under a rate limit. This is an environment problem (exit 3),\n\
             distinct from a not-found.\n\
             \n\
             To fix:\n\
               - check your connection and retry; a rate limit usually clears shortly, or\n\
               - sideload instead: `plugin install <path|tarball>` and `new --template\n\
                 <local-path>` never touch the network."
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
        ErrorCode::DaemonStartFailed => {
            "The managed dev host daemon did not come up within the start window.\n\
             \n\
             `rackabel dev`/`dev start` re-exec a detached daemon that owns the\n\
             Extension Host, then wait a few seconds for its control socket and a\n\
             health ping. If that never happens, the daemon failed to launch (a bad\n\
             Live/host path, a node that won't start, or the host couldn't connect to\n\
             Live's single host slot).\n\
             \n\
             Note: the managed dev host is macOS/Unix-only for now; on Windows this\n\
             code is returned immediately.\n\
             \n\
             To fix:\n\
               - run `rackabel doctor` to confirm Live, the host module, and Developer\n\
                 Mode are all ready, then retry.\n\
               - run with --raw to see the daemon's startup output.\n\
               - if a non-rackabel Extension Host is already connected to Live, stop it\n\
                 first (Live accepts only one host connection at a time)."
        }
        ErrorCode::ProtocolMismatch => {
            "The running dev host daemon speaks a different control-socket protocol\n\
             than this rackabel build.\n\
             \n\
             The daemon survives terminal close and upgrades; if you update rackabel\n\
             while an older daemon is still running, their socket protocol versions can\n\
             disagree. rackabel refuses to talk a protocol it doesn't understand rather\n\
             than risk a corrupt command.\n\
             \n\
             To fix:\n\
               - restart the dev host: `rackabel dev stop && rackabel dev`."
        }
        ErrorCode::NoDaemon => {
            "This command needs a running dev host, and none is up for the resolved\n\
             Live install.\n\
             \n\
             `dev status`, `dev reload`, `dev logs`, and `dev stop` are thin clients of\n\
             the daemon that owns the Extension Host. (Registry commands — `dev list`,\n\
             `dev register`, `dev enable`/`disable` — deliberately work WITHOUT a\n\
             daemon, so you can curate the set while the host is down.)\n\
             \n\
             To fix:\n\
               - start the dev host with `rackabel dev` (or `rackabel dev start`), then\n\
                 rerun. `dev watch` is the explicit no-auto-start form and reports this\n\
                 same code when nothing is up."
        }
        ErrorCode::HostCrashLooping => {
            "The Extension Host keeps crashing on startup and rackabel has stopped\n\
             auto-restarting it.\n\
             \n\
             After a bounded number of failed respawns in a short window the daemon\n\
             marks the host `crash-looping` and waits for you, rather than spinning a\n\
             doomed process forever (and so CI can detect the condition). The usual\n\
             cause is an extension that throws during `activate()`, or a bad native\n\
             dependency.\n\
             \n\
             To fix:\n\
               - read `rackabel dev logs` for the uncaught exception, fix the extension\n\
                 (or `dev disable <name>` it), then `rackabel dev reload` / `dev start`\n\
                 to clear the crash-looping state."
        }
        ErrorCode::RegistryLocked => {
            "rackabel could not acquire the registry lock.\n\
             \n\
             Writes to ~/.rackabel/registry.toml take a short advisory lock\n\
             (~/.rackabel/registry.lock) so two rackabel processes can't corrupt it.\n\
             A timeout means another rackabel is mid-write, or a previous run left a\n\
             stale lockfile.\n\
             \n\
             To fix:\n\
               - wait a moment and retry; if it persists and no other rackabel is\n\
                 running, delete ~/.rackabel/registry.lock and retry."
        }
        ErrorCode::NameCollision => {
            "A `register --name` value collides with an existing registry entry or a\n\
             reserved `dev` verb, and --no-input forbids the interactive rename.\n\
             \n\
             Registry names must be unique and must not shadow a `dev` verb\n\
             (start/stop/status/register/…), because that name is how you address the\n\
             extension (`dev logs <name>`, `dev --only <name>`). Normally rackabel\n\
             auto-disambiguates (e.g. `packages-a-foo`); under --no-input it will not\n\
             silently pick a name for you.\n\
             \n\
             To fix:\n\
               - pass a different, unique `--name` that isn't a dev verb, or\n\
               - drop --no-input so rackabel can auto-disambiguate and print what it\n\
                 chose."
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
        ErrorCode::ReloadActivateFailed => {
            "A targeted extension threw an exception inside its `activate()` during a\n\
             reload.\n\
             \n\
             The whole-host reload re-initialized, but the named extension's\n\
             `activate()` raised — so it is not loaded. rackabel maps the failing frame\n\
             back through the dist sourcemap to your source file:line where it can.\n\
             This is a build/runtime failure (exit 1): your code, not the machine.\n\
             \n\
             To fix:\n\
               - read the mapped stack in `rackabel dev logs <name>`, fix the throw,\n\
                 and save — the watch loop rebuilds, redeploys, and reloads. Other\n\
                 extensions stay loaded; only the failing one is affected."
        }
        ErrorCode::HostLaunchFailed => {
            "The Extension Host process failed to spawn.\n\
             \n\
             The daemon launches the host by running Live's bundled node against the\n\
             ExtensionHostNodeModule.node it found in your Live app. A spawn failure\n\
             means that node binary or host module could not be executed (a missing or\n\
             wrong path, or a permissions problem).\n\
             \n\
             To fix:\n\
               - run `rackabel doctor` to confirm the resolved Live, host module, and\n\
                 bundled node, then retry.\n\
               - override the paths explicitly with --eh-node / --eh-mod (or the\n\
                 ABLETON_EH_NODE / ABLETON_EH_MOD environment variables) if detection\n\
                 picked the wrong ones."
        }
        ErrorCode::TestFailed => {
            "`rackabel dev test` ran the headless test suite and at least one target\n\
             had a failing test (or its `activate()` smoke threw).\n\
             \n\
             `dev test` is the no-Live CI entry point (§3.8): per target it builds the\n\
             extension, then runs the project's own vitest/TestHarness tests, any\n\
             project-defined `*:headless` script, or — if neither exists — a best-effort\n\
             generic `activate()` smoke (reported as `skipped_no_harness`, NOT full CI\n\
             coverage). A failing target is a build/runtime failure (exit 1): your code\n\
             or your test, not the machine. It never needs Live, Developer Mode, or a\n\
             running dev host, and it never prompts.\n\
             \n\
             To fix:\n\
               - read the failing target(s) above; rerun with --raw to see the runner's\n\
                 full reporter output, or `--json` for a machine-readable summary\n\
                 ({ targets: [...], passed, failed }).\n\
               - run a single target with `rackabel dev test <name>` and forward runner\n\
                 flags verbatim after `--` (e.g. `dev test <name> -- -t \"my case\"`).\n\
               - targets flagged `skipped_no_harness` have no headless harness — add a\n\
                 vitest test (or a `*:headless` script) so they are really CI-covered."
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
        ErrorCode::SkippedIncompatible => {
            "An extension was skipped because its minimumApiVersion exceeds the host's\n\
             apiVersion, and `dev reload --strict` makes any such skip fatal.\n\
             \n\
             The daemon pre-filters the registry before launching the host: an\n\
             extension whose minimumApiVersion is higher than what the host provides is\n\
             dropped with a `Skipped:` note (one incompatible manifest would otherwise\n\
             take the WHOLE host down — verified host behavior). Normally a skip is\n\
             non-fatal (exit 0); `--strict` promotes it to exit 1 so a CI gate can't\n\
             read 'silently dropped one' as 'all good'.\n\
             \n\
             To fix:\n\
               - lower the extension's [extension].minimum_api_version to one the host\n\
                 supports, or upgrade Ableton Live, or\n\
               - drop --strict if a skipped (incompatible) extension is acceptable for\n\
                 this run. `rackabel dev status` lists every skipped extension and why."
        }
        ErrorCode::PinMismatch => {
            "An installed plugin's code does not match the pin recorded for it in\n\
             ~/.rackabel/plugins.lock.\n\
             \n\
             Every `plugin install` pins the resolved code by commit (for a clone) or\n\
             sha256 (for a downloaded asset/tarball). The lockfile is authoritative: at\n\
             install/verify time rackabel checks the bytes against the pin so a silently\n\
             changed upstream, a tampered download, or a stale cache can't slip in\n\
             (§5.4/§5.7). A mismatch is a validation failure (exit 4) so CI can gate on it.\n\
             \n\
             Note: pinning protects against TAMPERING and SILENT UPDATES — it keeps you on\n\
             the same code you installed, not on safe code.\n\
             \n\
             To fix:\n\
               - re-run the install to fetch the pinned bytes again, or\n\
               - if you intend to move to new code, pass `--force` to update past the pin\n\
                 (rackabel announces the change and, for a hook plugin, disables it until\n\
                 you re-`enable` — new code never runs under old consent, §5.7)."
        }
        ErrorCode::UpdateConflicts => {
            "`rackabel new --update` re-ran the template's 3-way merge and some files\n\
             could not be merged cleanly.\n\
             \n\
             `--update` re-renders the template at its new commit (re-using your saved\n\
             answers from .rackabel-template), treats your working tree as 'ours', and\n\
             merges. Files that changed in both the template and your tree get conflict\n\
             markers; clean files apply silently. Binary/generated files in\n\
             `[merge].exclude` (e.g. vendored SDK tarballs) are never text-merged (§5.5).\n\
             \n\
             To fix:\n\
               - open the files listed in the `help:` summary, resolve the\n\
                 <<<<<<< / ======= / >>>>>>> conflict markers, and save. `--update` is a\n\
                 deliberate developer action and never runs on the Persona-A happy path,\n\
                 so it never silently clobbers your setup."
        }
        ErrorCode::HookFailed => {
            "A lifecycle hook exited nonzero (or its run otherwise failed), and the hook\n\
             is an INFORMATIONAL one — post_build, on_reload, doctor_check, or\n\
             new_template.\n\
             \n\
             By design (§5.3) these hooks are logged and SKIPPED on failure: a crashing\n\
             third-party hook can never abort `build`, freeze the dev loop, or take down\n\
             `doctor`. (Only `pre_deploy` may veto its phase — that is RK1310, not this\n\
             code.) So this is not a fatal command outcome; it is the recorded reason a\n\
             hook contributed nothing this run.\n\
             \n\
             To fix:\n\
               - read the hook's output in the log (run with --raw for its full stderr),\n\
                 fix the hook script, or `rackabel plugin disable <name>` to stop running\n\
                 it. A project-local hook lives in your own `[hooks]` table — edit the\n\
                 script it points at."
        }
        ErrorCode::PreDeployVetoed => {
            "A `pre_deploy` hook exited nonzero, so the deploy was ABORTED.\n\
             \n\
             `pre_deploy` is the ONE lifecycle hook allowed to veto its phase (§5.3) — it\n\
             exists precisely to gate a deploy (a notarize/signing check, a policy gate).\n\
             A nonzero exit is the hook saying \"do not ship this\", so rackabel stops\n\
             before copying anything into the User Library. The frame names the hook and\n\
             its source.\n\
             \n\
             To fix:\n\
               - read the hook's output for WHY it refused, satisfy that gate, and rerun;\n\
                 or\n\
               - `rackabel plugin disable <name>` to remove the gate (its consent), or\n\
                 edit your project-local `[hooks].pre_deploy` script. The hook ran because\n\
                 you enabled it — enabling is standing consent (§5.7)."
        }
        ErrorCode::HookTimeout => {
            "A lifecycle hook exceeded its wall-clock timeout and was killed.\n\
             \n\
             Every hook runs under a timeout (default 30s, overridable per hook via\n\
             `[hooks.timeouts] <hook> = <ms>`, §5.3). On timeout rackabel sends SIGTERM,\n\
             then SIGKILL after a 5s grace, and treats the result as a hook failure. This\n\
             bounds the \"just never exit\" denial-of-service: an enabled `post_build` or\n\
             `on_reload` runs on EVERY save, and a hanging `pre_deploy` would block deploys\n\
             forever — the timeout makes the loop continue (or the deploy fail fast)\n\
             instead.\n\
             \n\
             A timed-out informational hook is logged + skipped; a timed-out `pre_deploy`\n\
             aborts the deploy (RK1310 semantics) with the timeout named.\n\
             \n\
             To fix:\n\
               - speed up the hook, or raise its budget in `[hooks.timeouts]` (the hook's\n\
                 `rackabel-plugin.toml`, or next to a project-local `[hooks]`), or\n\
               - `rackabel plugin disable <name>` if it is wedged. A hook that reads stdin\n\
                 forever is a common cause: rackabel writes ONE JSON object then closes\n\
                 stdin, so read to EOF and stop."
        }
        ErrorCode::HookApiUnsupported => {
            "An installed plugin's `rackabel-plugin.toml` declares a `hook_api` NEWER than\n\
             the hook contract this rackabel build supports.\n\
             \n\
             The tier-3 hook contract is versioned by its own integer, `hook_api` (§5.2),\n\
             separate from rackabel's product version and from the tier-2 env contract.\n\
             A plugin declaring `hook_api = N` expects the version-N stdin/stdout contract;\n\
             if your rackabel only speaks an older version, running its hooks could\n\
             misinterpret the contract, so rackabel refuses rather than guess. This is an\n\
             environment problem (exit 3): the machine's rackabel is too old.\n\
             \n\
             To fix:\n\
               - upgrade rackabel to a build that supports the plugin's `hook_api`, or\n\
               - `rackabel plugin migrate <name>` once a codemod ships for that bump, or\n\
               - `rackabel plugin disable <name>` to stop invoking its hooks meanwhile\n\
                 (a tier-2 PATH subcommand the same plugin ships still works)."
        }
        ErrorCode::MigrateUnsupported => {
            "`rackabel plugin migrate` was asked to migrate a plugin across a `hook_api`\n\
             bump this build has no codemod for.\n\
             \n\
             Hook-contract changes ship ONE at a time, each with a `plugin migrate` codemod\n\
             (§5.3, the ESLint-v9 lesson: never batch breaking plugin-contract changes).\n\
             Today the supported `hook_api` is 1 and no migrations exist yet: a plugin\n\
             declaring `hook_api = 1` reports \"nothing to migrate\" (success), and a plugin\n\
             declaring a HIGHER version has no codemod to run — this code is that clear\n\
             \"unsupported\" frame, not a crash.\n\
             \n\
             To fix:\n\
               - if the plugin targets a NEWER contract, upgrade rackabel to a build that\n\
                 ships the migration, then rerun `plugin migrate`; or\n\
               - if you authored the plugin, target the `hook_api` this rackabel supports.\n\
             Run `rackabel plugin migrate <name>` to see the detected vs supported\n\
             versions."
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
