//! `rackabel new` — scaffold a project (Extension or M4L device).
//!
//! This is the single biggest Persona-A lever (DESIGN §2 `new`, §4.7, §6.2). The
//! Extension path:
//!   1. runs an interactive wizard (kind / name / author / license / template) with
//!      bracketed Enter-to-accept defaults, fully flag-drivable, `--no-input`-safe;
//!   2. discovers the gated SDK+CLI toolkit (recursive, tarball or expanded folder,
//!      prefers expanded/newer) and, when missing, prints the §6.2 SDK-not-found
//!      guidance — never a dead-end, with remembered answers for the re-run;
//!   3. scaffolds the rackabel-form project (forked from create-extension; §4.7) and
//!      vendors the toolkit via `file:` deps;
//!   4. auto-builds when a usable node exists, else friendly-skips with a doctor pointer;
//!   5. git-inits by default (`--no-git` opts out) and prints the exact next command.
//!
//! The M4L `[device]` path is preserved verbatim (kept compiling and unchanged).

mod answers;
mod config;
mod scaffold;

use std::path::Path;

use crate::cli::{DeviceKindArg, NewArgs, ProjectKind};
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::MANIFEST_NAME;
use crate::max::patch::{self, PatchKind};
use crate::services::{node, toolkit};
use crate::ui;

use answers::RememberedAnswers;
use scaffold::ScaffoldData;

/// The default Extensions API version (SDK `EXTENSIONS_API_VERSIONS[0]`, SPEC A §2/§4).
const DEFAULT_API_VERSION: &str = "1.0.0";
/// The default license offered in the wizard (DESIGN §2).
const DEFAULT_LICENSE: &str = "MIT";
/// The single template offered in 0.2 (DESIGN §2 default working example).
const DEFAULT_TEMPLATE: &str = "default";

pub fn run(args: &NewArgs, ctx: &Ctx) -> CmdResult<()> {
    // `new --update` is a developer action over the CURRENT project (re-run the template's
    // copier-style 3-way merge, §5.5) — NOT a fresh scaffold — so it short-circuits before
    // kind resolution. The TEMPLATES agent fills the merge; the foundation routes it to a
    // clear boundary over the frozen `.rackabel-template` lockfile model.
    if args.update {
        return run_update(args, ctx);
    }

    // Resolve the kind. When unspecified and interactive, the wizard asks; under
    // --no-input (or with --yes) we default to Extension (the musician happy path).
    let kind = resolve_kind(args, ctx)?;
    match kind {
        ProjectKind::Extension => new_extension(args, ctx),
        ProjectKind::Device => new_device(args, ctx),
    }
}

/// `new --update` (DESIGN §5.5) — the copier-style 3-way template merge over the current
/// project. Reads `.rackabel-template`, re-renders old@oldcommit + new@newcommit (re-using
/// saved answers, prompting only for NEW prompts), and 3-way-merges against the user tree;
/// `--dry-run` prints the plan. An explicit developer action — never on the happy path.
fn run_update(args: &NewArgs, ctx: &Ctx) -> CmdResult<()> {
    crate::templates::update::run(ctx, args.dry_run, args.yes)
}

/// Resolve the project kind: explicit `--kind` wins; else prompt (or default Extension
/// under `--no-input`/`--yes`).
fn resolve_kind(args: &NewArgs, ctx: &Ctx) -> CmdResult<ProjectKind> {
    if let Some(k) = args.kind {
        return Ok(k);
    }
    if ctx.no_input || args.yes {
        return Ok(ProjectKind::Extension);
    }
    // Numbered pick-list (UX rule 2), never free-text. Default = Extension (index 0).
    let options = vec![
        "Live Extension".to_string(),
        "Max for Live device".to_string(),
    ];
    let idx = ui::prompt::select("What are you building?", &options, ctx)?;
    Ok(if idx == 1 {
        ProjectKind::Device
    } else {
        ProjectKind::Extension
    })
}

// ===========================================================================
// Extension path
// ===========================================================================

fn new_extension(args: &NewArgs, ctx: &Ctx) -> CmdResult<()> {
    // A `--template` ref renders a tier-1 template (§5.5): a git repo / local dir holding
    // a `rackabel-template.toml` whose `[prompts]` drive the wizard and whose files carry
    // `{{ key }}` placeholders. A REMOTE ref (gh:/@scope) is third-party code that `new`'s
    // auto-build would execute, so it hits the §5.7 confirmation first (local + the
    // built-in default skip it). The built-in default (no --template) takes the existing
    // happy path below.
    if let Some(tref) = &args.template {
        return new_from_template(tref, args, ctx);
    }

    // 1. The wizard — flag-driven, prompt-filled, --no-input-safe. Defaults are seeded
    //    from any remembered answers (the SDK-not-found re-run path).
    let wiz = run_wizard(args, ctx)?;

    // 2. Decide the target directory now (we need the name for it). The directory must
    //    not already exist (matches the device path + create-extension's empty-dir guard).
    let root = ctx.cwd.join(&wiz.dir_name);
    if root.exists() {
        return Err(RkError::of(
            ErrorCode::UsageError,
            format!("`{}` already exists", wiz.dir_name),
            "choose a different name, or remove the existing directory",
        )
        .at(root.display().to_string()));
    }

    // 3. Toolkit discovery. On failure we persist the wizard answers and print the
    //    §6.2 guidance — never a dead-end.
    if ctx.echo_on() {
        println!("  Looking for the Ableton Extensions toolkit…");
    }
    let tk = match discover_toolkit(args, ctx) {
        Ok(tk) => tk,
        Err(_not_found) => {
            // Remember the answers so the re-run picks them up (the §6.2 promise).
            answers::save(ctx, &wiz.dir_name, &wiz.remembered());
            return Err(sdk_not_found_error(&wiz.dir_name, args, ctx));
        }
    };
    if ctx.echo_on() {
        ui::frame::emit(
            ui::frame::Symbol::Good,
            &format!(
                "found the Ableton Extensions toolkit in {}",
                display_path(&tk.root, ctx)
            ),
            ctx,
        );
    }

    // 4. Create the project directory and scaffold the rackabel-form files.
    std::fs::create_dir_all(&root).map_err(|e| dir_err(&root, e))?;
    let data = ScaffoldData {
        package_name: scaffold::sanitize_package_name(&wiz.name),
        display_name: wiz.name.clone(),
        author: wiz.author.clone(),
        license: wiz.license.clone(),
        sdk_dep_basename: basename(&tk.sdk.path),
        cli_dep_basename: basename(&tk.cli.path),
        api_version: DEFAULT_API_VERSION.to_string(),
        minimal: args.minimal,
    };
    scaffold::render(&root, &data)?;

    // 5. Vendor the discovered toolkit into <root>/vendor (file: deps already point at it).
    toolkit::vendor_into(&tk, &root)?;
    if ctx.echo_on() {
        ui::frame::emit(
            ui::frame::Symbol::Good,
            &format!("added it to {}/ (no internet or npm needed)", wiz.dir_name),
            ctx,
        );
    }

    // 6. git init by default (unless --no-git or --minimal opts out per DESIGN §2:
    //    "default on for non-minimal").
    if resolve_git(args) {
        git_init(&root, ctx);
    }

    // The answers are spent now that the project exists.
    answers::clear(ctx, &wiz.dir_name);

    // 7. Auto-build when a usable node exists; else friendly-skip + doctor pointer.
    if ctx.echo_on() {
        ui::frame::emit(
            ui::frame::Symbol::Good,
            &format!("created {}/", wiz.dir_name),
            ctx,
        );
    }
    maybe_auto_build(&root, &wiz.dir_name, ctx);

    Ok(())
}

/// Scaffold from a tier-1 `--template` (§5.5). The directory name comes from the
/// positional `name` (the template's own `[prompts]` drive its file content). A remote ref
/// hits the §5.7 confirmation (we pass `--yes` as the consent). The template tree is
/// rendered with `{{ key }}` substitution, the `.rackabel-template` lockfile is written
/// (so `new --update` works), and git-init runs by default just like the built-in path.
fn new_from_template(tref: &str, args: &NewArgs, ctx: &Ctx) -> CmdResult<()> {
    // The directory name: the positional name (required for a template scaffold — the
    // template's prompts may also ask for a name, but the project FOLDER needs one now).
    let dir_name = match &args.name {
        Some(n) => dir_name_of(n),
        None => {
            return Err(RkError::of(
                ErrorCode::UsageError,
                "a project name is required when scaffolding from a template",
                "pass it as the first argument, e.g. `rackabel new <name> --template <ref>`",
            ));
        }
    };

    let root = ctx.cwd.join(&dir_name);
    if root.exists() {
        return Err(RkError::of(
            ErrorCode::UsageError,
            format!("`{dir_name}` already exists"),
            "choose a different name, or remove the existing directory",
        )
        .at(root.display().to_string()));
    }

    if ctx.echo_on() && crate::templates::is_remote_ref(tref) {
        println!("  Resolving template {tref}…");
    }

    // Render: resolve (+ remote confirm) + prompt + substitute + write the lockfile. On
    // any failure before files land, nothing is left behind; if files were partially
    // written the directory exists and the framed error names the problem.
    let outcome = crate::templates::render_into(tref, &root, args.yes, ctx)?;

    // git-init by default (unless --no-git / --minimal), mirroring the built-in path.
    if resolve_git(args) {
        git_init(&root, ctx);
    }

    if ctx.echo_on() {
        ui::frame::emit(
            ui::frame::Symbol::Good,
            &format!("created {dir_name}/ from template {tref}"),
            ctx,
        );
        if !outcome.answers.is_empty() {
            println!(
                "  (answers saved to .rackabel-template — `rackabel new --update` re-runs this template)"
            );
        }
        println!("  next:  cd {dir_name} && rackabel build");
    }
    Ok(())
}

/// The resolved wizard answers.
struct Wizard {
    name: String,
    /// The directory/slug name (sanitized for filesystem friendliness).
    dir_name: String,
    author: String,
    license: String,
    template: String,
}

impl Wizard {
    fn remembered(&self) -> RememberedAnswers {
        RememberedAnswers {
            kind: Some("extension".to_string()),
            name: Some(self.name.clone()),
            author: Some(self.author.clone()),
            license: Some(self.license.clone()),
            template: Some(self.template.clone()),
            minimal: None,
        }
    }
}

/// Run the interactive wizard (or resolve from flags under `--no-input`/`--yes`).
/// Defaults are seeded from remembered answers (keyed by the provided/positional name)
/// so the SDK-not-found re-run does not re-ask.
fn run_wizard(args: &NewArgs, ctx: &Ctx) -> CmdResult<Wizard> {
    // The name we use to look up remembered answers: the positional name if given.
    let remembered = args
        .name
        .as_deref()
        .and_then(|n| answers::load(ctx, &dir_name_of(n)));

    // `--yes` means "accept defaults" (D-3): like `--no-input` it never prompts for a
    // field that has a default, but unlike `--no-input` it is not a hard non-interactive
    // contract — it just takes the bracketed default. `ui::prompt::text` only auto-accepts
    // under `ctx.no_input`, so for the accept-defaults case we resolve the default here and
    // only reach an interactive prompt when neither `--yes` nor `--no-input` is set.
    let accept_defaults = args.yes || ctx.no_input;

    // -- name --
    let name = match &args.name {
        Some(n) => n.clone(),
        None => {
            // The name has no inferable default. Under --no-input (or --yes) there is
            // nothing to prompt for and nothing to invent, so this becomes a deterministic
            // usage error naming the positional argument (not a generic "pass it as a flag",
            // since the name is positional) — unless a remembered answer supplies one.
            let default = remembered.as_ref().and_then(|r| r.name.clone());
            if accept_defaults && default.is_none() {
                return Err(RkError::of(
                    ErrorCode::UsageError,
                    "a project name is required",
                    "pass it as the first argument, e.g. `rackabel new <name>` \
                     (running with --no-input, so I won't prompt for it)",
                ));
            }
            ui::prompt::text("Name", default.as_deref(), ctx)?
        }
    };
    let dir_name = dir_name_of(&name);

    // -- author -- default: remembered → git config → empty. Author is never a hard
    // requirement (UX rule 1: infer-and-echo, never hard-fail on a missing field) — an
    // unknown author is left empty and surfaces later in `validate` (RK4001), exactly
    // like `resolved_extension`'s inference. So when accepting defaults we fall back to
    // the resolved default (git config, else "") rather than prompting.
    let author_default = remembered
        .as_ref()
        .and_then(|r| r.author.clone())
        .or_else(crate::manifest::infer::infer_author_from_git)
        .unwrap_or_default();
    let author = if accept_defaults {
        author_default
    } else {
        ui::prompt::text("Author", Some(&author_default), ctx)?
    };

    // -- license -- default: remembered → MIT.
    let license_default = remembered
        .as_ref()
        .and_then(|r| r.license.clone())
        .unwrap_or_else(|| DEFAULT_LICENSE.to_string());
    let license = if accept_defaults {
        license_default
    } else {
        ui::prompt::text("License", Some(&license_default), ctx)?
    };

    // -- template -- one choice in 0.2; --template (local path) overrides the label.
    let template = args
        .template
        .clone()
        .or_else(|| remembered.as_ref().and_then(|r| r.template.clone()))
        .unwrap_or_else(|| DEFAULT_TEMPLATE.to_string());
    // We surface the template only as an echo, not a real prompt in 0.2 (there is a
    // single default template); a real pick-list lands with tier-1 templates in 0.4.
    // The "Enter to accept" hint only makes sense interactively, so we suppress it when
    // accepting defaults non-interactively (`--yes`/`--no-input`) — otherwise it leaks a
    // prompt-style line into a non-interactive run (which never waits for Enter).
    if ctx.echo_on() && !accept_defaults && args.template.is_none() {
        ui::frame::echo_resolved(
            "Template",
            "Default (a working right-click action)",
            "Enter to accept",
            ctx,
        );
    }

    Ok(Wizard {
        name,
        dir_name,
        author,
        license,
        template,
    })
}

/// Derive the project directory/slug name from a (possibly spaced) display name. We
/// reuse the package-name sanitizer so the slug is filesystem- and launcher-friendly
/// (the deploy slug is the dir basename, DESIGN §2). Falls back to "extension".
fn dir_name_of(name: &str) -> String {
    let s = scaffold::sanitize_package_name(name);
    if s.is_empty() {
        "extension".to_string()
    } else {
        s
    }
}

/// Discover the toolkit: `--sdk-dir` first (the deterministic path), else the default
/// search roots (`~/Downloads`, cwd, project root). The first root that yields BOTH the
/// SDK and CLI wins. `RK0201` if none do.
fn discover_toolkit(args: &NewArgs, ctx: &Ctx) -> CmdResult<toolkit::Toolkit> {
    if let Some(dir) = &args.sdk_dir {
        return toolkit::discover(dir);
    }
    let roots = toolkit::default_search_roots(None);
    let mut last: Option<RkError> = None;
    for root in &roots {
        if !root.exists() {
            continue;
        }
        match toolkit::discover(root) {
            Ok(tk) => return Ok(tk),
            Err(e) => last = Some(e),
        }
    }
    // No root worked. Surface a not-found error whose location names the cwd.
    Err(last.unwrap_or_else(|| toolkit::toolkit_not_found(&ctx.cwd)))
}

/// The §6.2 SDK-not-found error: numbered steps, the (centralized, updatable) beta URL,
/// the exact re-run command, the pick-file fallback, and the "answers remembered" line.
/// Never a dead-end.
fn sdk_not_found_error(dir_name: &str, args: &NewArgs, ctx: &Ctx) -> RkError {
    let url = config::extensions_beta_url();
    // The re-run command names the dir the user most plausibly used for the toolkit.
    let sdk_dir_hint = args
        .sdk_dir
        .as_ref()
        .map(|p| display_path(p, ctx))
        .unwrap_or_else(|| "~/Downloads".to_string());

    let help = format!(
        "It's a separate file from Ableton, only available if you've joined the\n\
         Extensions beta. Access is granted by Ableton and may not be instant —\n\
         once you have the toolkit file, come back and run the command below.\n\
         Here's how to get it:\n\
         \x20 1. Join / open the beta at:  {url}\n\
         \x20    (if that page has moved, search Ableton's site for \"Extensions beta\".\n\
         \x20     if you've just requested access, you may need to wait for approval.)\n\
         \x20 2. Download the toolkit file it gives you (it ends in .tgz).\n\
         \x20 3. Put it (or its folder) anywhere easy, e.g. your Downloads folder.\n\
         \x20    The .tgz, or an unzipped folder, either works — I'll find it.\n\
         Then run this once, pointing at where you saved it:\n\
         \x20  rackabel new {dir_name} --sdk-dir {sdk_dir_hint}\n\
         (or just run `rackabel new` again and pick the file when asked.)\n\
         Already downloaded it but still seeing this? Run `rackabel new` again\n\
         and choose \"find the toolkit file myself\" to point me straight at it.\n\
         Your answers above are remembered — nothing was lost."
    );

    RkError::of(
        ErrorCode::ToolkitNotFound,
        "Couldn't find the Ableton Extensions toolkit download.",
        help,
    )
}

/// Auto-build the just-scaffolded project iff a usable node exists. Otherwise the
/// friendly skip + doctor pointer (DESIGN §0, §6.2 aside) — never a raw "node not found".
fn maybe_auto_build(root: &Path, dir_name: &str, ctx: &Ctx) {
    if node::any_usable(ctx).is_none() {
        // The §6.2 "no Live / no node" aside: skip, don't dead-end.
        if ctx.echo_on() {
            println!("  (skipped the build — I couldn't find Ableton Live or a Node runtime yet.)");
            println!(
                "  next:  install Live Suite 12.4.5+ and enable the Extensions beta, then run"
            );
            println!("         `rackabel doctor` from inside {dir_name}/ — build and run happen");
            println!(
                "         once Live is present. Get Live: {} (Suite).",
                config::LIVE_DOWNLOAD_URL
            );
        }
        return;
    }

    // A usable node exists, but the project's dependencies (esbuild + the SDK/CLI
    // file: deps) are not installed yet — `new` vendors the tarballs and pins them in
    // package.json but does not run the install. Building now would fail with the scary
    // RK1301 "couldn't find esbuild" frame on EVERY brand-new project (DESIGN §1's
    // "first thing they see is error:" trap). So when node_modules is absent we
    // friendly-skip with the exact next steps instead of dead-ending on a red error.
    if !root.join("node_modules").exists() {
        if ctx.echo_on() {
            println!("  (skipped the build — the project's dependencies aren't installed yet.)");
            println!(
                "  next:  cd {dir_name} && npm install   # vendored offline, no internet needed"
            );
            println!("         then `rackabel build` (or `rackabel dev` to build + run in Live).");
        }
        return;
    }

    // A usable node exists and deps are installed: build the project. We reuse the
    // shared build entry so the banner + manifest generation + validation are identical
    // to `rackabel build`.
    let project = match crate::manifest::Project::discover(root) {
        Ok(p) => p,
        Err(_) => return, // We just wrote rackabel.toml; this should not happen.
    };
    let opts = crate::services::esbuild::BuildOptions::default();
    match crate::services::esbuild::build_extension(&project, &opts, ctx) {
        Ok(outcome) => {
            // The §6.2 happy-path next line.
            if ctx.echo_on() {
                let ms = outcome.elapsed.as_millis();
                ui::frame::emit(
                    ui::frame::Symbol::Good,
                    &format!("built your extension ({ms}ms)"),
                    ctx,
                );
                println!("  next:  cd {dir_name} && rackabel dev");
            }
        }
        Err(e) => {
            // A build failure post-scaffold is not a `new` dead-end: the project exists.
            // Print the framed build error as a note, but keep `new` successful (the
            // user can fix the source and `rackabel build`).
            if ctx.echo_on() {
                ui::frame::print_error(&e, ctx);
                println!("  the project was created — fix the above, then run `rackabel build`.");
                println!("  next:  cd {dir_name} && rackabel dev");
            }
        }
    }
}

// --- small helpers ----------------------------------------------------------

/// Resolve whether to git-init: `--no-git` off, `--git` on, else default (on for
/// non-minimal, off for minimal — DESIGN §2 "default on for non-minimal").
fn resolve_git(args: &NewArgs) -> bool {
    if args.no_git {
        return false;
    }
    if args.git {
        return true;
    }
    !args.minimal
}

/// `git init` in `root`, best-effort (a missing git is not a `new` failure; the project
/// is already created). Quiet on success.
fn git_init(root: &Path, ctx: &Ctx) {
    let out = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root)
        .output();
    if !matches!(out, Ok(ref o) if o.status.success()) && ctx.verbose && ctx.echo_on() {
        ui::frame::emit(
            ui::frame::Symbol::Warn,
            "skipped git init (git not available) — run `git init` yourself if you want one",
            ctx,
        );
    }
}

/// The basename of a path as a `String` (for the `file:` dep names).
fn basename(p: &Path) -> String {
    p.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string()
}

/// Render a path home-relative (`~/…`) when it lives under `ctx.home`, for friendly,
/// deterministic output (the §6.2 transcript shows `~/Downloads`).
fn display_path(p: &Path, ctx: &Ctx) -> String {
    if let Ok(rel) = p.strip_prefix(&ctx.home) {
        if rel.as_os_str().is_empty() {
            "~".to_string()
        } else {
            format!("~/{}", rel.display())
        }
    } else {
        p.display().to_string()
    }
}

fn dir_err(path: &Path, e: std::io::Error) -> RkError {
    RkError::new(
        ErrorCode::DeployCopyFailed,
        ExitClass::BuildRuntime,
        "could not create the project directory",
        "check write permissions for the parent directory, then retry",
    )
    .at(path.display().to_string())
    .raw(e.into())
}

// ===========================================================================
// M4L device path — preserved verbatim (behavior unchanged).
// ===========================================================================

fn new_device(args: &NewArgs, ctx: &Ctx) -> CmdResult<()> {
    let name = match &args.name {
        Some(n) => n.clone(),
        None => {
            return Err(RkError::of(
                ErrorCode::UsageError,
                "a device project needs a name",
                "pass it: `rackabel new <name> --kind device`",
            ));
        }
    };

    let dev_kind = args.device_kind.unwrap_or(DeviceKindArg::AudioEffect);

    let root = ctx.cwd.join(&name);
    if root.exists() {
        return Err(RkError::of(
            ErrorCode::UsageError,
            format!("`{name}` already exists"),
            "choose a different name or remove the existing directory",
        )
        .at(root.display().to_string()));
    }

    let src = root.join("src");
    std::fs::create_dir_all(&src).map_err(io_err(&src))?;

    let entry = format!("src/{name}.maxpat");
    let manifest = format!(
        "[device]\nname = \"{name}\"\nkind = \"{}\"\nentry = \"{entry}\"\n",
        manifest_name(dev_kind)
    );
    std::fs::write(root.join(MANIFEST_NAME), manifest).map_err(io_err(&root))?;

    let patch_json = serde_json::to_string_pretty(&patch::starter_patch(patch_kind(dev_kind)))
        .expect("starter patch serializes");
    std::fs::write(root.join(&entry), patch_json).map_err(io_err(&root))?;

    std::fs::write(
        root.join(".gitignore"),
        "/build/\n.DS_Store\n*.maxpat.bak\n",
    )
    .map_err(io_err(&root))?;

    println!("Created `{name}` ({})", manifest_name(dev_kind));
    println!("\n  cd {name}");
    println!("  rackabel build      # assemble the .amxd");
    println!("  rackabel deploy     # copy it into Ableton's User Library");
    Ok(())
}

fn patch_kind(k: DeviceKindArg) -> PatchKind {
    match k {
        DeviceKindArg::AudioEffect => PatchKind::AudioEffect,
        DeviceKindArg::MidiEffect => PatchKind::MidiEffect,
        DeviceKindArg::Instrument => PatchKind::Instrument,
    }
}

fn manifest_name(k: DeviceKindArg) -> &'static str {
    match k {
        DeviceKindArg::AudioEffect => "audio-effect",
        DeviceKindArg::MidiEffect => "midi-effect",
        DeviceKindArg::Instrument => "instrument",
    }
}

fn io_err(path: &Path) -> impl Fn(std::io::Error) -> RkError {
    let path = path.to_path_buf();
    move |e| {
        RkError::new(
            ErrorCode::DeployCopyFailed,
            ExitClass::BuildRuntime,
            "could not write the project files",
            "check write permissions for the target directory",
        )
        .at(path.display().to_string())
        .raw(e.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_name_sanitizes_display_name() {
        assert_eq!(dir_name_of("Clip Renamer"), "clip-renamer");
        assert_eq!(dir_name_of("clip-renamer"), "clip-renamer");
        assert_eq!(dir_name_of("***"), "extension");
    }

    #[test]
    fn git_default_is_on_for_non_minimal_off_for_minimal() {
        let base = base_args();
        assert!(resolve_git(&base));

        let minimal = NewArgs {
            minimal: true,
            ..base_args()
        };
        assert!(!resolve_git(&minimal));

        let forced = NewArgs {
            minimal: true,
            git: true,
            ..base_args()
        };
        assert!(resolve_git(&forced));

        let off = NewArgs {
            no_git: true,
            ..base_args()
        };
        assert!(!resolve_git(&off));
    }

    fn base_args() -> NewArgs {
        NewArgs {
            name: None,
            kind: None,
            device_kind: None,
            template: None,
            minimal: false,
            yes: false,
            update: false,
            dry_run: false,
            sdk_dir: None,
            git: false,
            no_git: false,
        }
    }
}
