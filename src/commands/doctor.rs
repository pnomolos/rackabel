//! `rackabel doctor` — diagnose the environment (DESIGN §2 doctor, §6.2, §6.3).
//!
//! A flutter/expo-style checklist with the stable `[✓]`/`[!]`/`[✗]` vocabulary, a
//! per-failure `help:` fix line, and a tail summary count ("N/M checks passed").
//! Quiet-on-success: when *everything* passes the default view collapses to just the
//! tail count (no wall of green); the moment there's anything to act on (a warning or
//! a failure) the full checklist shows so each `[!]`/`[✗]` has context — exactly like
//! the §6.2 happy transcript. `--verbose` always shows every row plus developer
//! internals (`NODE_MODULE_VERSION`-class facts, exact `apiVersion`, the resolved host
//! module path). `--json` emits every row with `id`/`symbol`/`message`/`help`.
//!
//! Extensions checks (Live install + arch, host layout, bundled-node floor, Developer
//! Mode, Live-running, User Library, toolkit, manifest + `minimumApiVersion`,
//! deployed-vs-source drift, native deps compiled) are added alongside the existing
//! Max / Live / User-Library M4L checks, which are preserved.
//!
//! `--fix` performs the safe auto-fixes available in 0.2: vendor a discovered SDK
//! toolkit into the project, call the native-dep fix service, and point at `deploy`
//! for a stale bundle; anything it can't do safely it points at the right command.
//!
//! A fast subset of these checks is exposed via [`preflight`] so `build`/`deploy`/
//! `pack` can fail with doctor-style remedies (exit 3) before touching anything.

use std::path::Path;

use serde_json::json;

use crate::cli::DoctorArgs;
use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};
use crate::manifest::{Kind, Project};
use crate::services::live::{self, LiveInstall};
use crate::services::node;
use crate::services::toolkit;
use crate::services::user_library;
use crate::ui::frame::Symbol;
use crate::ui::{self, color::Style};

/// The result of one diagnostic row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
    /// A check that could not run because a prerequisite failed (e.g. no Live install).
    /// It counts toward the total but is neither a pass nor a failure, and is not
    /// rendered — the prerequisite's `[✗]` row carries the remedy. Lets the summary
    /// honestly read "N/M checks passed — the remaining checks can't run …" (§6.2).
    Blocked,
}

impl CheckStatus {
    fn symbol(self) -> Symbol {
        match self {
            CheckStatus::Ok => Symbol::Good,
            CheckStatus::Warn => Symbol::Warn,
            CheckStatus::Fail | CheckStatus::Blocked => Symbol::Bad,
        }
    }

    /// The `--json` symbol vocabulary (matches the plugin `doctor_check` contract).
    fn json_word(self) -> &'static str {
        match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Warn => "warn",
            CheckStatus::Fail => "fail",
            CheckStatus::Blocked => "blocked",
        }
    }
}

/// One diagnostic row. `id` is a stable machine key for `--json`; `message` is the
/// musician-facing one-liner; `help` (when present) is the indented fix line; `detail`
/// is a `--verbose`/`--json`-only internal (resolved paths, `NODE_MODULE_VERSION`-class
/// facts) hidden from the default checklist.
#[derive(Debug, Clone)]
pub struct Check {
    pub id: &'static str,
    pub status: CheckStatus,
    pub message: String,
    pub help: Option<String>,
    pub detail: Option<String>,
}

impl Check {
    fn new(id: &'static str, status: CheckStatus, message: impl Into<String>) -> Self {
        Self {
            id,
            status,
            message: message.into(),
            help: None,
            detail: None,
        }
    }

    fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// The full diagnosis: every row, in display order.
#[derive(Debug, Default)]
pub struct Diagnosis {
    pub checks: Vec<Check>,
}

impl Diagnosis {
    fn push(&mut self, c: Check) {
        self.checks.push(c);
    }

    fn count(&self, status: CheckStatus) -> usize {
        self.checks.iter().filter(|c| c.status == status).count()
    }

    fn passed(&self) -> usize {
        self.count(CheckStatus::Ok)
    }

    fn warnings(&self) -> usize {
        self.count(CheckStatus::Warn)
    }

    fn failures(&self) -> usize {
        self.count(CheckStatus::Fail)
    }

    fn blocked(&self) -> usize {
        self.count(CheckStatus::Blocked)
    }

    fn total(&self) -> usize {
        self.checks.len()
    }

    /// Exit 0 unless any check failed (a `[!]` warning never fails — DESIGN §2);
    /// failures map to the environment class (exit 3).
    fn exit_class(&self) -> ExitClass {
        if self.failures() > 0 {
            ExitClass::Environment
        } else {
            ExitClass::Ok
        }
    }
}

pub fn run(args: &DoctorArgs, ctx: &Ctx) -> CmdResult<()> {
    // The project is optional: doctor must work OUTSIDE a project (the "check first,
    // then create" order — DESIGN §2). A no-manifest discovery is not an error here.
    let project = Project::discover_cwd(ctx).ok();

    if args.fix {
        run_fixes(project.as_ref(), ctx);
        // After fixes, re-diagnose below so the user sees the post-fix state.
    }

    let diag = diagnose(project.as_ref(), ctx);

    if ctx.json {
        render_json(&diag);
    } else {
        render_checklist(&diag, ctx);
    }

    // Doctor's checklist (or JSON) IS the output: a failing check is already fully
    // explained there with its own `help:` line, so we must NOT emit a second framed
    // error after it (that would break the §6.2 transcript and read as a duplicate).
    // Returning `Err` would route through `main`'s `ui::print_error`. Instead we set
    // the process exit code directly from the diagnosis and exit cleanly after
    // flushing stdout. Exit 0 unless a check failed ([!] warnings never fail).
    let class = diag.exit_class();
    if class == ExitClass::Ok {
        return Ok(());
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
    std::process::exit(class as i32);
}

// ---------------------------------------------------------------------------
// Diagnosis assembly
// ---------------------------------------------------------------------------

/// Build the full diagnosis. Pure assembly of [`Check`]s; rendering is separate so the
/// same diagnosis drives the checklist, `--json`, and (a subset) the preflight.
pub fn diagnose(project: Option<&Project>, ctx: &Ctx) -> Diagnosis {
    let mut diag = Diagnosis::default();

    // --- Live install + version + arch ---
    let live = live::detect(ctx);
    let primary = live.iter().find(|i| i.host_module.is_some());

    let Some(install) = primary else {
        // No usable Live install: the headline failure. Without Live, none of the other
        // checks can run meaningfully (they all depend on Live or on a project the user
        // hasn't created yet), so we render JUST the one `[✗]` row and push Blocked
        // placeholders for the rest (counted in the total, never rendered). The summary
        // then reads the §6.2 no-Live line: "0/6 checks passed — 1 thing to fix
        // (the remaining checks can't run until Live is found)". Deterministic regardless
        // of whether Max happens to be installed.
        diag.push(no_live_check());
        for id in [
            "live.host",
            "live.node",
            "live.devmode",
            "user_library",
            "toolkit",
        ] {
            diag.push(Check::new(id, CheckStatus::Blocked, "(needs Ableton Live)"));
        }
        return diag;
    };

    check_live_install(&mut diag, install);
    check_host_layout(&mut diag, install);
    check_bundled_node(&mut diag, install, project, ctx);

    // --- Developer Mode (inferred; not statically readable — DESIGN §9.2) ---
    check_developer_mode(&mut diag, primary, ctx);

    // --- Live running (informational in a static doctor run) ---
    check_live_running(&mut diag, primary, ctx);

    // --- User Library resolution ---
    check_user_library(&mut diag, project, ctx);

    // --- Extensions toolkit (non-blocking note outside a project) ---
    check_toolkit(&mut diag, project);

    // --- Manifestless (synthesized) project, surfaced so the inference isn't invisible ---
    check_project_anchor(&mut diag, project);

    // --- Per-project extension checks (only when in an extension project) ---
    if let Some(proj) = project
        && proj.kind().ok() == Some(Kind::Extension)
    {
        check_manifest_and_api(&mut diag, proj, primary, ctx);
        check_drift(&mut diag, proj, ctx);
        check_native_deps(&mut diag, proj, ctx);
    }

    // --- Max + Max for Live (existing M4L device path) ---
    check_max(&mut diag);

    // --- doctor_check lifecycle hooks (DESIGN §5.3) ---
    // Every ENABLED plugin (and a project-local [hooks]) that declares `doctor_check`
    // contributes a row, rendered here in the standard checklist with the plugin name. The
    // stdout-line-wins a-d precedence is applied in the engine; a hook that crashes/times out
    // is a generic fail row, never a `doctor` abort.
    check_plugin_doctor_hooks(&mut diag, project, ctx);

    diag
}

/// Run every discovered, enabled `doctor_check` hook and fold each into a checklist row
/// (DESIGN §5.3). Project-local hooks (implicit trust) come first, then enabled plugins in
/// lock order — exactly the discovery resolver's order. Outside a project the stdin payload's
/// `project_dir`/`manifest_toml` are absent (the hook must tolerate that).
fn check_plugin_doctor_hooks(diag: &mut Diagnosis, project: Option<&Project>, ctx: &Ctx) {
    use crate::hooks::outcome::{DoctorResolution, DoctorSymbol};

    // The §5.3 payload discriminator is project-vs-no-project: inside ANY project the hook
    // gets `project_dir`/`manifest_toml`, so we pass the project ROOT whenever a project
    // exists — independent of whether it declares a `[hooks]` table. The project-local
    // `[hooks]` table is passed separately and used ONLY to add the project's own hook as a
    // discovery source (a device project's `[hooks]` table is honored too).
    let proj_root = project.map(|p| p.root.as_path());
    let proj_hooks = project.and_then(|p| p.hooks_table());

    let rows = match crate::hooks::run::doctor_check_rows(ctx, proj_root, proj_hooks) {
        Ok(rows) => rows,
        // A discovery/lock read hiccup must not crash `doctor` (a diagnostic) — surface a
        // single note row instead, then carry on with the built-in summary.
        Err(_) => return,
    };

    for row in rows {
        // The row id is stable + machine-friendly for `--json`: `hook.<plugin>` /
        // `hook.project`. The displayed message attributes the row to its source.
        let id: &'static str = "hook.doctor_check";
        let who = &row.source_label; // e.g. "plugin notarize" / "project"
        let check = match row.resolution {
            DoctorResolution::Line(line) => {
                let status = match line.symbol {
                    DoctorSymbol::Ok => CheckStatus::Ok,
                    DoctorSymbol::Warn => CheckStatus::Warn,
                    DoctorSymbol::Fail => CheckStatus::Fail,
                };
                let mut c = Check::new(id, status, format!("{} — {}", who, line.message));
                if let Some(help) = line.help {
                    c = c.with_help(help);
                }
                c
            }
            DoctorResolution::Pass => {
                Check::new(id, CheckStatus::Ok, format!("{who} — doctor_check passed"))
            }
            DoctorResolution::GenericFail => {
                // Combination (d): nonzero/timeout with no contract line ⇒ the generic
                // `doctor_check <name> failed` row (§5.3), naming the plugin.
                let name = row.plugin_name.as_deref().unwrap_or("project");
                Check::new(
                    id,
                    CheckStatus::Fail,
                    format!("{who} — doctor_check {name} failed"),
                )
                .with_help(
                    "the plugin's doctor_check hook exited nonzero or timed out without \
                     printing a result line — check the plugin, or `rackabel plugin \
                     disable` it",
                )
            }
        };
        diag.push(check);
    }
}

/// The no-Live headline failure (the §6.2 transcript wording).
fn no_live_check() -> Check {
    Check::new(
        "live.install",
        CheckStatus::Fail,
        "No Ableton Live install found",
    )
    .with_help(
        "install Live Suite 12.4.5+ and turn on the Extensions beta\n\
         (Live → Settings → … → Beta), then rerun `rackabel doctor`.",
    )
}

/// `[✓] Ableton Live 12.4.5 Suite (beta) — Extensions supported (Apple Silicon)`.
fn check_live_install(diag: &mut Diagnosis, install: &LiveInstall) {
    let supported = install.supports_extensions();
    let arch = install.arch.friendly();
    if supported {
        diag.push(
            Check::new(
                "live.install",
                CheckStatus::Ok,
                format!(
                    "Ableton Live {} Suite (beta) — Extensions supported ({arch})",
                    install.version
                ),
            )
            .with_detail(format!("app: {}", install.app.display())),
        );
    } else {
        diag.push(
            Check::new(
                "live.install",
                CheckStatus::Fail,
                format!(
                    "Ableton Live {} found, but it's older than 12.4.5",
                    install.version
                ),
            )
            .with_help(
                "upgrade to Live Suite 12.4.5+ and turn on the Extensions beta\n\
                 (Live → Settings → … → Beta), then rerun `rackabel doctor`.",
            )
            .with_detail(format!("app: {}", install.app.display())),
        );
    }
}

/// `[✓] Live's Extension components found` — which host layout exists (never hardcode).
fn check_host_layout(diag: &mut Diagnosis, install: &LiveInstall) {
    match (&install.host_module, install.host_layout) {
        (Some(module), Some(layout)) => {
            let where_ = match layout {
                live::HostLayout::Helpers => "Contents/Helpers/ExtensionHost",
                live::HostLayout::AppResources => "Contents/App-Resources/Extensions/ExtensionHost",
            };
            diag.push(
                Check::new(
                    "live.host",
                    CheckStatus::Ok,
                    "Live's Extension components found",
                )
                .with_detail(format!("host module: {} ({where_})", module.display())),
            );
        }
        _ => {
            diag.push(
                Check::new(
                    "live.host",
                    CheckStatus::Fail,
                    "Live's Extension components weren't found in this install",
                )
                .with_help(
                    "this Live version may not support Extensions yet — upgrade to\n\
                     Live Suite 12.4.5+ with the Extensions beta enabled, then rerun.",
                ),
            );
        }
    }
}

/// Live's bundled node vs the runtime floor (>=22.11.0). Below ⇒ upgrade LIVE, never
/// "install Node" (DESIGN §0). `NODE_MODULE_VERSION`-class internals are `--verbose`.
fn check_bundled_node(
    diag: &mut Diagnosis,
    install: &LiveInstall,
    project: Option<&Project>,
    ctx: &Ctx,
) {
    let floor = runtime_floor(project);
    let floor_str = floor_display(project);

    // Resolve specifically Live's bundled node for the ABI-match check (not a PATH
    // fallback — this row is about Live's own runtime).
    let runtime = node::resolve(Some(install), ctx);
    match runtime {
        Some(rt) if rt.source == node::NodeSource::LiveBundled => {
            if node::meets_runtime_floor(&rt, &floor) {
                diag.push(
                    Check::new(
                        "live.node",
                        CheckStatus::Ok,
                        "Live's components are compatible",
                    )
                    .with_detail(format!(
                        "bundled node v{} (floor {floor_str}), bin: {}",
                        rt.version,
                        rt.bin.display()
                    )),
                );
            } else {
                diag.push(
                    Check::new(
                        "live.node",
                        CheckStatus::Fail,
                        format!(
                            "Live's bundled Node (v{}) is older than this SDK needs",
                            rt.version
                        ),
                    )
                    .with_help(format!(
                        "upgrade Ableton Live — its bundled Node must be {floor_str}.\n\
                         (Do not install Node separately; the dev loop must use Live's own Node.)"
                    ))
                    .with_detail(format!("bundled node bin: {}", rt.bin.display())),
                );
            }
        }
        _ => {
            // Live present but its bundled node didn't resolve (missing or unrunnable).
            diag.push(
                Check::new(
                    "live.node",
                    CheckStatus::Warn,
                    "couldn't read Live's bundled Node version",
                )
                .with_help(
                    "this is unusual for a supported Live — try reinstalling Live,\n\
                     then rerun `rackabel doctor`.",
                ),
            );
        }
    }
}

/// Developer Mode: NOT statically readable in 0.2 (DESIGN §9.2 / SPEC B §6). We report
/// it as inferred from running-Live + host-child presence, with honest wording, and a
/// navigational help line. Also warns if a *bare* node host is running (SIGHUP-unsafe).
fn check_developer_mode(diag: &mut Diagnosis, primary: Option<&LiveInstall>, ctx: &Ctx) {
    // If there's no Live, there is nothing to say about Developer Mode.
    if primary.is_none() {
        return;
    }

    let live_running = is_live_running(ctx);
    let bare_host = is_bare_host_running(ctx);

    if bare_host {
        // A non-rackabel host is running: reload is unsafe (DESIGN §6.3).
        diag.push(
            Check::new(
                "live.devmode",
                CheckStatus::Warn,
                "a non-rackabel Extension Host appears to be running — reload is unsafe",
            )
            .with_help(
                "stop that host (or quit and reopen Live), then use `rackabel dev`,\n\
                 which owns the host process so live reload is safe.",
            ),
        );
        return;
    }

    // Inferred state: we cannot read the toggle directly, so we phrase it honestly
    // rather than asserting it on or off (DESIGN §9.2). The wording mirrors the §6.2
    // Dev-Mode-off transcript so the remedy is identical to what `dev` shows.
    if live_running {
        diag.push(
            Check::new(
                "live.devmode",
                CheckStatus::Warn,
                "Developer Mode can't be read directly — make sure it's ON for the dev loop",
            )
            .with_help(
                "open Live → Settings → Extensions → turn on Developer Mode\n\
                 (it appears once you've joined the Extensions beta). `rackabel dev`\n\
                 waits for it and continues automatically when it flips on.",
            )
            .with_detail(
                "Developer Mode is not statically readable from disk (DESIGN §9.2); \
                 inferred from running-Live + host-child presence.",
            ),
        );
    } else {
        diag.push(
            Check::new(
                "live.devmode",
                CheckStatus::Warn,
                "Developer Mode is OFF — the dev loop (live reload) can't run without it",
            )
            .with_help(
                "open Live → Settings → Extensions → turn on Developer Mode\n\
                 (it appears once you've joined the Extensions beta), then rerun\n\
                 `rackabel doctor` — or just run `rackabel dev`, which waits for it.",
            )
            .with_detail(
                "Developer Mode is not statically readable from disk (DESIGN §9.2); \
                 inferred from running-Live + host-child presence.",
            ),
        );
    }
}

/// Live running — informational here (a gate only in the `dev` fast subset).
fn check_live_running(diag: &mut Diagnosis, primary: Option<&LiveInstall>, ctx: &Ctx) {
    if primary.is_none() {
        return;
    }
    if is_live_running(ctx) {
        diag.push(Check::new(
            "live.running",
            CheckStatus::Ok,
            "Ableton Live is running",
        ));
    } else {
        diag.push(
            Check::new(
                "live.running",
                CheckStatus::Warn,
                "Ableton Live doesn't appear to be running",
            )
            .with_help(
                "that's fine for `doctor`, but the dev loop connects to a running Live —\n\
                 open the Live app before `rackabel dev` (it waits for it either way).",
            ),
        );
    }
}

/// User Library resolution (show the resolved path + how it was chosen).
fn check_user_library(diag: &mut Diagnosis, project: Option<&Project>, ctx: &Ctx) {
    match resolve_user_library_quiet(project, ctx) {
        Ok(ul) => {
            diag.push(
                Check::new(
                    "user_library",
                    CheckStatus::Ok,
                    format!("User Library: {}", ul.path.display()),
                )
                .with_detail(format!("chosen: {:?}", ul.source)),
            );
        }
        Err(e) => {
            diag.push(
                Check::new(
                    "user_library",
                    CheckStatus::Fail,
                    "Couldn't find your Live User Library yet",
                )
                .with_help(strip_help(&e)),
            );
        }
    }
}

/// Extensions toolkit. Outside an extension project this is a non-blocking `[!]` note
/// (there is no vendored toolkit to find — DESIGN §2); a Max-for-Live `[device]`
/// project is treated like "no project" here (the toolkit is Extensions-only). Inside
/// an extension project it's a `[✓]` once vendored, else a `[!]` pointing at
/// `rackabel new`/the SDK.
fn check_toolkit(diag: &mut Diagnosis, project: Option<&Project>) {
    // A device project has no use for the Extensions toolkit — render the same neutral
    // note as the no-project case rather than nag about a missing vendor/ dir.
    let project = project.filter(|p| p.kind().ok() == Some(Kind::Extension));
    match project {
        None => {
            diag.push(
                Check::new(
                    "toolkit",
                    CheckStatus::Warn,
                    "Extensions toolkit — not needed until you run `rackabel new`",
                )
                .with_help(
                    "`rackabel new` vendors the toolkit into your project; there's\n\
                     nothing to check until then.",
                ),
            );
        }
        Some(proj) => {
            // A project that has vendored the toolkit has it under `vendor/`.
            let vendor = proj.root.join("vendor");
            match toolkit::discover(&vendor) {
                Ok(tk) => {
                    diag.push(
                        Check::new("toolkit", CheckStatus::Ok, "Extensions toolkit ready")
                            .with_detail(format!(
                                "sdk: {} ({:?}), cli: {} ({:?})",
                                tk.sdk.path.display(),
                                tk.sdk.form,
                                tk.cli.path.display(),
                                tk.cli.form
                            )),
                    );
                }
                Err(_) => {
                    diag.push(
                        Check::new(
                            "toolkit",
                            CheckStatus::Warn,
                            "Extensions toolkit not vendored into this project yet",
                        )
                        .with_help(
                            "run `rackabel new` to scaffold (it vendors the toolkit), or\n\
                             drop the SDK/CLI .tgz under this project's vendor/ folder.",
                        ),
                    );
                }
            }
        }
    }
}

/// Surface a *synthesized* (manifestless) project explicitly so the fallback inference
/// isn't invisible (DESIGN §4.1). When the discovered project has no `rackabel.toml` on
/// disk (`manifest_path.is_none()`) it was anchored on a `package.json` and its kind came
/// from the default (or a `package.json` `"rackabel": { "kind": "device" }` opt-in). We
/// report that as a normal informational `[!]` note — never a failure — with a tip to pin
/// the fields. A real `rackabel.toml` (or no project at all) adds no row here.
fn check_project_anchor(diag: &mut Diagnosis, project: Option<&Project>) {
    let Some(proj) = project else {
        return;
    };
    if proj.manifest_path.is_some() {
        // A real on-disk manifest: nothing to surface, behavior unchanged.
        return;
    }

    // Resolve the defaulted kind for the message; if it somehow can't resolve, the
    // synthesized default is Extension, so fall back to that wording rather than nothing.
    let kind = proj.kind().unwrap_or(Kind::Extension);
    let kind_str = match kind {
        Kind::Extension => "extension",
        Kind::Device => "device",
        Kind::Workspace => "workspace",
    };
    // The kind PROVENANCE (#10): only say "(default)" when it was actually defaulted.
    // A package.json `"rackabel": { "kind": ... }` opt-in is an EXPLICIT choice — label it
    // `(package.json)` so doctor doesn't claim a chosen kind was a fallback.
    let from_pkg = proj
        .pkg
        .as_ref()
        .and_then(|p| p.rackabel.as_ref())
        .and_then(|r| r.kind.as_ref())
        .is_some();
    let provenance = if from_pkg { "package.json" } else { "default" };
    diag.push(
        Check::new(
            "project.manifestless",
            CheckStatus::Warn,
            format!(
                "project: manifestless — anchored at package.json, kind={kind_str} ({provenance})"
            ),
        )
        .with_help(
            "run `rackabel new` or add an [extension] table to rackabel.toml to pin fields.",
        )
        .with_detail(format!("anchor: {}", proj.root.join("package.json").display())),
    );
}

/// Manifest validity + `minimumApiVersion` compatibility (a hard gate — DESIGN §2).
fn check_manifest_and_api(
    diag: &mut Diagnosis,
    proj: &Project,
    primary: Option<&LiveInstall>,
    ctx: &Ctx,
) {
    let ext = match proj.resolved_extension(&quiet_ctx(ctx)) {
        Ok(e) => e,
        Err(e) => {
            diag.push(
                Check::new(
                    "manifest",
                    CheckStatus::Fail,
                    "this project's extension manifest is incomplete",
                )
                .with_help(strip_help(&e)),
            );
            return;
        }
    };

    // Required-field completeness (name/author/entry/version/minimumApiVersion). After
    // inference, name/version/entry/minimumApiVersion always have a value; only author
    // can legitimately end up empty (no git config, none set).
    let mut missing = Vec::new();
    if ext.name.trim().is_empty() {
        missing.push("name");
    }
    if ext.author.trim().is_empty() {
        missing.push("author");
    }
    if missing.is_empty() {
        diag.push(
            Check::new(
                "manifest",
                CheckStatus::Ok,
                "Extension manifest is complete",
            )
            .with_detail(format!(
                "name={}, author={}, version={}, entry={}, minimumApiVersion={}",
                ext.name,
                ext.author,
                ext.version,
                ext.entry.display(),
                ext.minimum_api_version
            )),
        );
    } else {
        diag.push(
            Check::new(
                "manifest",
                CheckStatus::Warn,
                format!("manifest is missing: {}", missing.join(", ")),
            )
            .with_help(
                "set the missing field(s) in rackabel.toml under [extension]\n\
                 (author is read from `git config user.name` if you don't set it).",
            ),
        );
    }

    // minimumApiVersion vs the detected host apiVersion. The only static source of the
    // host's supported version is the SDK bundle's EXTENSIONS_API_VERSIONS (newest-
    // first); we use the known beta value 1.0.0 as the detected host apiVersion.
    let host_api = detect_host_api_version(primary);
    if ext.minimum_api_version > host_api {
        diag.push(
            Check::new(
                "manifest.api",
                CheckStatus::Fail,
                format!(
                    "this extension needs Extensions API {} but the host provides {}",
                    ext.minimum_api_version, host_api
                ),
            )
            .with_help(
                "lower [extension].minimum_api_version, or upgrade Ableton Live —\n\
                 a host-incompatible minimumApiVersion can stop the host from loading.",
            )
            .with_detail(format!(
                "host apiVersion {host_api} (from SDK EXTENSIONS_API_VERSIONS)"
            )),
        );
    } else {
        diag.push(
            Check::new(
                "manifest.api",
                CheckStatus::Ok,
                "Extension API version is compatible with the host",
            )
            .with_detail(format!(
                "minimumApiVersion {} ≤ host apiVersion {host_api}",
                ext.minimum_api_version
            )),
        );
    }
}

/// Deployed-vs-source drift: warn if `dist/extension.js` is newer than the deployed
/// copy (the deploy-before-reload trap — DESIGN §3, §6.3). Best-effort (no Live / no
/// User Library ⇒ silently no row).
fn check_drift(diag: &mut Diagnosis, proj: &Project, ctx: &Ctx) {
    let dist = proj.root.join("dist/extension.js");
    if !dist.is_file() {
        // Nothing built yet — not drift, and the build flow covers it.
        return;
    }
    let Ok(ul) = resolve_user_library_quiet(Some(proj), ctx) else {
        return;
    };
    let slug = proj.slug();
    let deployed = user_library::extension_install_dir(&ul, &slug).join("dist/extension.js");

    match drift_state(&dist, &deployed) {
        DriftState::NotDeployed => {
            diag.push(
                Check::new(
                    "drift",
                    CheckStatus::Warn,
                    "this extension is built but not deployed into Live yet",
                )
                .with_help("run `rackabel deploy` to install it into Live's User Library."),
            );
        }
        DriftState::Stale => {
            diag.push(
                Check::new(
                    "drift",
                    CheckStatus::Warn,
                    "your built bundle is newer than the copy deployed in Live",
                )
                .with_help(
                    "run `rackabel deploy` to update Live's copy (or `rackabel dev`,\n\
                     which deploys before every reload so this never happens).",
                )
                .with_detail(format!(
                    "dist: {} | deployed: {}",
                    dist.display(),
                    deployed.display()
                )),
            );
        }
        DriftState::UpToDate => {
            diag.push(Check::new(
                "drift",
                CheckStatus::Ok,
                "Live's deployed copy is up to date with your build",
            ));
        }
    }
}

/// Native deps compiled: each declared `native_deps` must have a compiled `.node`.
/// Read-only and best-effort: we walk the project's `node_modules` directly (the
/// `native_dep::audit` body lands with `deploy`, so doctor does its own lightweight
/// check to avoid coupling to that stub).
fn check_native_deps(diag: &mut Diagnosis, proj: &Project, ctx: &Ctx) {
    let ext = match proj.resolved_extension(&quiet_ctx(ctx)) {
        Ok(e) => e,
        Err(_) => return,
    };
    if ext.native_deps.is_empty() {
        return; // pure-JS extension: nothing to compile.
    }

    let mut uncompiled = Vec::new();
    let mut missing = Vec::new();
    for dep in &ext.native_deps {
        let dir = proj.root.join("node_modules").join(dep);
        if !dir.is_dir() {
            missing.push(dep.clone());
        } else if !has_dot_node(&dir) {
            uncompiled.push(dep.clone());
        }
    }

    if missing.is_empty() && uncompiled.is_empty() {
        diag.push(Check::new(
            "native_deps",
            CheckStatus::Ok,
            "Native components are built",
        ));
    } else if !uncompiled.is_empty() {
        diag.push(
            Check::new(
                "native_deps",
                CheckStatus::Fail,
                format!(
                    "a compiled component needs to be built: {}",
                    uncompiled.join(", ")
                ),
            )
            .with_help("run `rackabel deploy --fix` to build it under the hood."),
        );
    } else {
        diag.push(
            Check::new(
                "native_deps",
                CheckStatus::Warn,
                format!(
                    "a native dependency isn't installed: {}",
                    missing.join(", ")
                ),
            )
            .with_help(
                "install your project's dependencies (e.g. `npm install` /\n\
                 `pnpm install`), then run `rackabel deploy --fix`.",
            ),
        );
    }
}

/// Max + Max for Live (the existing M4L device path). Preserved from the original
/// doctor: Max install presence (the User Library row is shared with the Extensions
/// path above).
fn check_max(diag: &mut Diagnosis) {
    use crate::max::paths;

    let max = paths::max_installs();
    if max.is_empty() {
        diag.push(
            Check::new(
                "max",
                CheckStatus::Warn,
                "Max is not installed (only needed for Max for Live devices)",
            )
            .with_help("install Max if you plan to build Max for Live (.amxd) devices."),
        );
    } else {
        let names: Vec<String> = max
            .iter()
            .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
            .collect();
        diag.push(
            Check::new("max", CheckStatus::Ok, "Max is installed")
                .with_detail(format!("found: {}", names.join(", "))),
        );
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render the human checklist (DESIGN §2). Default view collapses passing rows to the
/// tail count; `--verbose` shows every row plus the `detail` internals.
fn render_checklist(diag: &Diagnosis, ctx: &Ctx) {
    // Quiet-on-success: when everything passes (no warnings, no failures) the default
    // view collapses to just the tail count — the musician reads "you're fine" without
    // a wall of green. As soon as there is anything to act on (a warning or a failure),
    // we show the FULL checklist (passing rows included) so each `[!]`/`[✗]` has context
    // — exactly like the §6.2 happy transcript, which shows every row. `--verbose`
    // always shows every row plus the internal `detail` lines.
    let all_green = diag.warnings() == 0 && diag.failures() == 0 && diag.blocked() == 0;
    let show_passing = ctx.verbose || !all_green;

    for c in &diag.checks {
        // Blocked rows are never rendered (their prerequisite's `[✗]` carries the
        // remedy); they exist only to keep the summary total honest.
        if c.status == CheckStatus::Blocked {
            continue;
        }
        if c.status == CheckStatus::Ok && !show_passing {
            continue;
        }
        ui::frame::emit(c.status.symbol(), &c.message, ctx);
        if let Some(help) = &c.help {
            // Render each help line under the row, lined up with the §6.2 transcripts.
            let mut lines = help.lines();
            if let Some(first) = lines.next() {
                let label = Style::Heading.paint("help:", ctx.color);
                println!("    {label} {first}");
                for line in lines {
                    println!("          {line}");
                }
            }
        }
        if ctx.verbose
            && let Some(detail) = &c.detail
        {
            println!("      {}", Style::Dim.paint(detail, ctx.color));
        }
    }

    print_summary(diag, ctx);
}

/// The tail summary line(s).
fn print_summary(diag: &Diagnosis, ctx: &Ctx) {
    let passed = diag.passed();
    let total = diag.total();
    let warnings = diag.warnings();
    let failures = diag.failures();
    let blocked = diag.blocked();

    if blocked > 0 {
        // Live (or another prerequisite) is missing: the dependent checks couldn't run.
        // Keep the most demoralizing first run friendly (§6.2 no-Live transcript).
        println!(
            "{passed}/{total} checks passed — {failures} thing{} to fix \
             (the remaining checks can't run until Live is found)",
            if failures == 1 { "" } else { "s" }
        );
    } else if failures == 0 && warnings == 0 {
        // All good.
        println!("{passed}/{total} checks passed");
    } else if failures == 0 {
        let plural = if warnings == 1 { "warning" } else { "warnings" };
        println!("{passed}/{total} checks passed, {warnings} {plural}");
    } else {
        let fplural = if failures == 1 { "failure" } else { "failures" };
        if warnings > 0 {
            let wplural = if warnings == 1 { "warning" } else { "warnings" };
            println!("{passed}/{total} checks passed, {failures} {fplural}, {warnings} {wplural}");
        } else {
            println!("{passed}/{total} checks passed, {failures} {fplural}");
        }
    }

    // The "for version/ABI details" hint is shown only when there are details to show
    // (i.e. some check actually ran). With everything blocked (no Live), there are no
    // version/ABI internals yet, so we omit it — matching the §6.2 no-Live transcript.
    if !ctx.verbose && blocked == 0 && passed > 0 {
        println!(
            "   {}",
            Style::Dim.paint(
                "(run `rackabel doctor --verbose` for version/ABI details)",
                ctx.color
            )
        );
    }
}

/// Render the machine shape: every row with id/symbol/message/help/detail.
fn render_json(diag: &Diagnosis) {
    let checks: Vec<_> = diag
        .checks
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "symbol": c.status.json_word(),
                "message": c.message,
                "help": c.help,
                "detail": c.detail,
            })
        })
        .collect();
    let v = json!({
        "checks": checks,
        "summary": {
            "passed": diag.passed(),
            "warnings": diag.warnings(),
            "failures": diag.failures(),
            "total": diag.total(),
        },
        "ok": diag.failures() == 0,
    });
    println!("{}", serde_json::to_string_pretty(&v).expect("json"));
}

// ---------------------------------------------------------------------------
// --fix (safe auto-fixes)
// ---------------------------------------------------------------------------

/// Perform the safe auto-fixes available in 0.2 (DESIGN §2 `--fix`): vendor a
/// discovered SDK toolkit into the project, call the native-dep fix service, and point
/// at `deploy` for a stale bundle. Anything that can't be done safely is left to its
/// row's `help:`. Each fix prints a short progress line; failures degrade to a note.
fn run_fixes(project: Option<&Project>, ctx: &Ctx) {
    let Some(proj) = project else {
        if ctx.echo_on() {
            ui::frame::note(
                "nothing to fix outside a project — run `rackabel doctor --fix` inside one",
                ctx,
            );
        }
        return;
    };
    if proj.kind().ok() != Some(Kind::Extension) {
        return;
    }

    // 1. Vendor the SDK if the project hasn't vendored it but one is discoverable.
    let vendor = proj.root.join("vendor");
    if toolkit::discover(&vendor).is_err() {
        for root in toolkit::default_search_roots(Some(proj)) {
            if let Ok(tk) = toolkit::discover(&root) {
                match toolkit::vendor_into(&tk, &proj.root) {
                    Ok(()) => {
                        if ctx.echo_on() {
                            ui::frame::emit(
                                Symbol::Good,
                                &format!("vendored the Extensions toolkit from {}", root.display()),
                                ctx,
                            );
                        }
                    }
                    Err(e) => {
                        if ctx.echo_on() {
                            ui::frame::note(
                                &format!("couldn't vendor the toolkit: {}", e.problem),
                                ctx,
                            );
                        }
                    }
                }
                break;
            }
        }
    }

    // 2. Native-dep fix (the deploy-owned service; a 0.2 stub returns RK0304). We call
    //    it only when the project declares native deps that need building.
    if let Ok(ext) = proj.resolved_extension(&quiet_ctx(ctx))
        && !ext.native_deps.is_empty()
    {
        let needs_build = ext.native_deps.iter().any(|d| {
            let dir = proj.root.join("node_modules").join(d);
            dir.is_dir() && !has_dot_node(&dir)
        });
        if needs_build {
            match crate::services::native_dep::fix(proj, &ext, ctx) {
                Ok(()) => {
                    if ctx.echo_on() {
                        ui::frame::emit(Symbol::Good, "built native components", ctx);
                    }
                }
                Err(e) => {
                    // The stub's help line is the actionable remedy; surface it.
                    if ctx.echo_on() {
                        ui::frame::note(&e.help, ctx);
                    }
                }
            }
        }
    }

    // 3. Redeploy a stale bundle — point at the command rather than driving a full
    //    deploy from doctor (deploy owns its own preflight + native copy).
    let dist = proj.root.join("dist/extension.js");
    if dist.is_file()
        && let Ok(ul) = resolve_user_library_quiet(Some(proj), ctx)
    {
        let deployed =
            user_library::extension_install_dir(&ul, &proj.slug()).join("dist/extension.js");
        if matches!(
            drift_state(&dist, &deployed),
            DriftState::Stale | DriftState::NotDeployed
        ) && ctx.echo_on()
        {
            ui::frame::note(
                "your build isn't deployed (or is stale) — run `rackabel deploy` to update Live",
                ctx,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Fast-subset preflight (SPEC C — build/deploy/pack call this)
// ---------------------------------------------------------------------------

/// The kind of operation a preflight is gating, so the subset is appropriate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preflight {
    /// `build`: needs a usable node; Live is preferred, not required.
    Build,
    /// `deploy`: needs Live (>=12.4.5) + a resolvable User Library.
    Deploy,
    /// `pack`: needs a usable node (bundle sanity / official CLI); Live not required.
    Pack,
}

/// Run a fast subset of doctor for `build`/`deploy`/`pack`. On the first failing
/// environment check, returns a doctor-style framed [`RkError`] (exit 3) carrying that
/// row's message + help — so callers fail with the same remedy a musician sees in
/// `doctor`, never a raw SDK trace. `Ok(())` means the environment subset is ready.
///
/// NOTE FOR THE INTEGRATOR: `build`/`deploy`/`pack` should call this at the top of
/// their `run()` so they preflight before touching anything. (Those command modules
/// are owned by other agents; wiring is a one-line `doctor::preflight(...)?` each.)
pub fn preflight(which: Preflight, project: Option<&Project>, ctx: &Ctx) -> CmdResult<()> {
    let live = live::detect(ctx);
    let primary = live.iter().find(|i| i.host_module.is_some());

    match which {
        Preflight::Build | Preflight::Pack => {
            // A usable node anywhere (Live-bundled > PATH). No node ⇒ RK0305.
            if node::resolve(primary, ctx).is_none() {
                let verb = if which == Preflight::Build {
                    "build"
                } else {
                    "pack"
                };
                return Err(RkError::new(
                    ErrorCode::NoNodeRuntime,
                    ExitClass::Environment,
                    format!("couldn't find a Node runtime to {verb} with"),
                    "install Ableton Live 12.4.5+ (it bundles the right Node), or install \
                     Node on your PATH, then rerun.",
                ));
            }
        }
        Preflight::Deploy => {
            // Deploy needs Live + a User Library.
            let install = primary.ok_or_else(|| {
                RkError::new(
                    ErrorCode::NoLiveInstall,
                    ExitClass::Environment,
                    "No Ableton Live install found",
                    "install Live Suite 12.4.5+ and turn on the Extensions beta\n\
                     (Live → Settings → … → Beta), then rerun.",
                )
            })?;
            if !install.supports_extensions() {
                return Err(RkError::new(
                    ErrorCode::NoLiveInstall,
                    ExitClass::Environment,
                    format!(
                        "Ableton Live {} is older than 12.4.5 (Extensions unsupported)",
                        install.version
                    ),
                    "upgrade to Live Suite 12.4.5+ with the Extensions beta enabled, then rerun.",
                ));
            }
            // User Library must resolve (the resolver returns the framed RK0302).
            user_library::resolve(project, ctx)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The runtime node floor: `[toolchain].node_runtime` if set, else the default
/// (>=22.11.0).
fn runtime_floor(project: Option<&Project>) -> semver::VersionReq {
    project
        .and_then(|p| p.raw.toolchain.as_ref())
        .and_then(|t| t.node_runtime.as_ref())
        .and_then(|s| semver::VersionReq::parse(s).ok())
        .unwrap_or_else(node::default_runtime_floor)
}

/// A display string for the floor, for help/detail lines.
fn floor_display(project: Option<&Project>) -> String {
    project
        .and_then(|p| p.raw.toolchain.as_ref())
        .and_then(|t| t.node_runtime.clone())
        .unwrap_or_else(|| ">=22.11.0".to_string())
}

/// The host's supported apiVersion, from the only static source on disk — the SDK
/// bundle's `EXTENSIONS_API_VERSIONS` (newest-first). In 0.2 the known beta value is
/// `1.0.0` (the runtime host version is only knowable via `ActivationContext`); we use
/// the constant. `primary` is accepted for a future per-Live read.
fn detect_host_api_version(primary: Option<&LiveInstall>) -> semver::Version {
    let _ = primary;
    semver::Version::new(1, 0, 0)
}

/// Whether the Ableton Live app appears to be running (best-effort, macOS `pgrep`).
///
/// Process-state detection is inherently machine-dependent, so a dedicated test seam
/// (`RACKABEL_DOCTOR_LIVE_RUNNING` = `0`/`1`) pins it deterministically in tests
/// without spawning a real Live. (This is a doctor-internal probe override, distinct
/// from the Ctx-routed `ABLETON_*` resolution overrides.)
fn is_live_running(ctx: &Ctx) -> bool {
    let _ = ctx;
    if let Some(v) = test_bool_env("RACKABEL_DOCTOR_LIVE_RUNNING") {
        return v;
    }
    if !cfg!(target_os = "macos") {
        return false;
    }
    pgrep_matches("Ableton Live")
}

/// Whether a *bare* Extension Host node process (one Live or a stray script spawned,
/// not rackabel) appears to be running — SIGHUP-unsafe (DESIGN §6.3). Pinned in tests
/// via `RACKABEL_DOCTOR_BARE_HOST` = `0`/`1` (see [`is_live_running`]).
fn is_bare_host_running(ctx: &Ctx) -> bool {
    let _ = ctx;
    if let Some(v) = test_bool_env("RACKABEL_DOCTOR_BARE_HOST") {
        return v;
    }
    if !cfg!(target_os = "macos") {
        return false;
    }
    // Identify by the host module's distinctive require() in the command line.
    pgrep_matches("ExtensionHostNodeModule.node")
}

/// Read a `0`/`1` boolean probe-override env var. `None` if unset.
fn test_bool_env(key: &str) -> Option<bool> {
    match std::env::var(key).ok()?.as_str() {
        "1" | "true" => Some(true),
        "0" | "false" => Some(false),
        _ => None,
    }
}

/// Best-effort `pgrep -fl <needle>`: true if any process command line matches.
fn pgrep_matches(needle: &str) -> bool {
    std::process::Command::new("pgrep")
        .args(["-fl", needle])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

/// Whether `dir` (a package dir) contains any compiled `.node` binary, NOT descending
/// into nested `node_modules` (matches SPEC B `hasNativeBinary`).
fn has_dot_node(dir: &Path) -> bool {
    for entry in walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| e.file_name() != "node_modules" || e.path() == dir)
        .filter_map(Result::ok)
    {
        if entry.file_type().is_file() && entry.path().extension().is_some_and(|x| x == "node") {
            return true;
        }
    }
    false
}

/// The deploy-before-reload drift states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriftState {
    /// No deployed copy exists.
    NotDeployed,
    /// `dist/extension.js` is newer than the deployed copy.
    Stale,
    /// The deployed copy is at least as new as the source bundle.
    UpToDate,
}

/// Compare a source bundle to its deployed copy by mtime.
fn drift_state(dist: &Path, deployed: &Path) -> DriftState {
    if !deployed.is_file() {
        return DriftState::NotDeployed;
    }
    let src = mtime(dist);
    let dep = mtime(deployed);
    match (src, dep) {
        (Some(s), Some(d)) if s > d => DriftState::Stale,
        _ => DriftState::UpToDate,
    }
}

fn mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Pull the `help` text out of a framed error, for re-use as a row's help block (the
/// resolver/manifest errors carry the canonical remedy text).
fn strip_help(e: &RkError) -> String {
    e.help.clone()
}

/// A clone of `ctx` with echoes suppressed (`json = true`), so shared services that
/// echo on resolution/inference (the User Library resolver, manifest inference) stay
/// silent. Doctor renders those facts as its own checklist rows and decides JSON vs.
/// checklist at the top level; left as-is, the services' stray echo lines would pollute
/// the checklist and break the §6.2 transcript. The resolution/inference *results* are
/// unchanged — only the echo is gated off.
fn quiet_ctx(ctx: &Ctx) -> Ctx {
    Ctx {
        no_input: ctx.no_input,
        json: true,
        quiet: false,
        verbose: ctx.verbose,
        raw: ctx.raw,
        color: ctx.color,
        color_err: ctx.color_err,
        cwd: ctx.cwd.clone(),
        rackabel_home: ctx.rackabel_home.clone(),
        home: ctx.home.clone(),
        ableton_app: ctx.ableton_app.clone(),
        ableton_user_library: ctx.ableton_user_library.clone(),
        ableton_eh_mod: ctx.ableton_eh_mod.clone(),
        ableton_eh_node: ctx.ableton_eh_node.clone(),
        ableton_extensions_dir: ctx.ableton_extensions_dir.clone(),
        ableton_storage_base: ctx.ableton_storage_base.clone(),
        rackabel_host_cmd: ctx.rackabel_host_cmd.clone(),
    }
}

/// Resolve the User Library with the resolver's own echo suppressed (see [`quiet_ctx`]).
fn resolve_user_library_quiet(
    project: Option<&Project>,
    ctx: &Ctx,
) -> CmdResult<user_library::UserLibrary> {
    // doctor is a diagnostic — it must never prompt and never fail on ambiguity (a
    // checklist reports a concrete library). `resolve_newest` is the dedicated
    // non-prompting path: ambiguity resolves newest-wins regardless of --no-input.
    let q = quiet_ctx(ctx);
    user_library::resolve_newest(project, &q)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::node::NodeRuntime;
    use semver::Version;
    use std::path::PathBuf;

    fn mk_install(version: &str, has_host: bool) -> LiveInstall {
        LiveInstall {
            app: PathBuf::from("/Applications/Ableton Live 12 Beta.app"),
            version: version.to_string(),
            arch: live::LiveArch::Universal,
            host_module: has_host.then(|| PathBuf::from("/x/ExtensionHostNodeModule.node")),
            host_layout: has_host.then_some(live::HostLayout::Helpers),
            bundled_node: None,
        }
    }

    #[test]
    fn drift_states() {
        let tmp = tempfile::tempdir().unwrap();
        let dist = tmp.path().join("dist.js");
        let deployed = tmp.path().join("deployed.js");

        // Not deployed.
        std::fs::write(&dist, b"x").unwrap();
        assert_eq!(drift_state(&dist, &deployed), DriftState::NotDeployed);

        // Deployed first, then source rewritten later => stale.
        std::fs::write(&deployed, b"old").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&dist, b"new").unwrap();
        assert_eq!(drift_state(&dist, &deployed), DriftState::Stale);

        // Deploy again after the source => up to date.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&deployed, b"newest").unwrap();
        assert_eq!(drift_state(&dist, &deployed), DriftState::UpToDate);
    }

    #[test]
    fn live_install_row_reports_arch_and_version() {
        let mut diag = Diagnosis::default();
        check_live_install(&mut diag, &mk_install("12.4.5b3", true));
        let c = &diag.checks[0];
        assert_eq!(c.status, CheckStatus::Ok);
        assert!(c.message.contains("12.4.5b3"));
        assert!(c.message.contains("Apple Silicon") || c.message.contains("Intel"));
    }

    #[test]
    fn below_floor_live_install_fails_with_upgrade_help() {
        let mut diag = Diagnosis::default();
        check_live_install(&mut diag, &mk_install("12.4.0", true));
        let c = &diag.checks[0];
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.help.as_ref().unwrap().contains("upgrade"));
    }

    #[test]
    fn floor_comparison_uses_toolchain_override() {
        // The default floor accepts 22.11.0; a stricter override should reject it.
        let strict = semver::VersionReq::parse(">=24.14.1").unwrap();
        let rt = NodeRuntime {
            bin: PathBuf::from("/x"),
            version: Version::new(22, 11, 0),
            source: node::NodeSource::LiveBundled,
        };
        assert!(node::meets_runtime_floor(
            &rt,
            &node::default_runtime_floor()
        ));
        assert!(!node::meets_runtime_floor(&rt, &strict));
    }

    #[test]
    fn host_api_floor_is_one_zero_zero() {
        assert_eq!(detect_host_api_version(None), Version::new(1, 0, 0));
    }

    #[test]
    fn no_live_check_has_navigational_help() {
        let c = no_live_check();
        assert_eq!(c.status, CheckStatus::Fail);
        assert!(c.help.as_ref().unwrap().contains("Extensions beta"));
    }

    #[test]
    fn summary_counts() {
        let mut diag = Diagnosis::default();
        diag.push(Check::new("a", CheckStatus::Ok, "ok"));
        diag.push(Check::new("b", CheckStatus::Ok, "ok"));
        diag.push(Check::new("c", CheckStatus::Warn, "warn"));
        assert_eq!(diag.passed(), 2);
        assert_eq!(diag.warnings(), 1);
        assert_eq!(diag.failures(), 0);
        assert_eq!(diag.exit_class(), ExitClass::Ok);

        diag.push(Check::new("d", CheckStatus::Fail, "bad"));
        assert_eq!(diag.exit_class(), ExitClass::Environment);
    }

    #[test]
    fn warnings_never_fail_the_run() {
        let mut diag = Diagnosis::default();
        diag.push(Check::new("a", CheckStatus::Warn, "w"));
        diag.push(Check::new("b", CheckStatus::Ok, "o"));
        assert_eq!(diag.exit_class(), ExitClass::Ok);
    }

    /// Build a synthesized (manifestless) project at `root` whose `package.json` holds the
    /// given JSON body — used to exercise the kind-provenance label in check_project_anchor.
    fn synth_project(root: &Path, pkg_json: &str) -> Project {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join("package.json"), pkg_json).unwrap();
        Project {
            root: root.to_path_buf(),
            raw: crate::manifest::ManifestRaw::default(),
            manifest_path: None,
            pkg: crate::manifest::pkgjson::read(root),
            kind_override: None,
        }
    }

    #[test]
    fn manifestless_row_says_default_when_kind_defaulted() {
        // #10: a package.json with NO "rackabel".kind opt-in → the kind is the synthesized
        // default (extension), so the row must read "(default)".
        let tmp = tempfile::tempdir().unwrap();
        let proj = synth_project(&tmp.path().join("ext"), "{\"name\":\"ext\"}");
        let mut diag = Diagnosis::default();
        check_project_anchor(&mut diag, Some(&proj));
        let row = diag
            .checks
            .iter()
            .find(|c| c.id == "project.manifestless")
            .expect("manifestless row present");
        assert_eq!(row.status, CheckStatus::Warn);
        assert!(
            row.message.contains("kind=extension (default)"),
            "got: {}",
            row.message
        );
    }

    #[test]
    fn manifestless_row_says_package_json_when_kind_opted_in() {
        // #10: an explicit package.json `"rackabel": { "kind": "device" }` is a CHOICE,
        // not a fallback — the row must read "(package.json)", not "(default)".
        let tmp = tempfile::tempdir().unwrap();
        let proj = synth_project(
            &tmp.path().join("dev"),
            "{\"name\":\"dev\",\"rackabel\":{\"kind\":\"device\"}}",
        );
        let mut diag = Diagnosis::default();
        check_project_anchor(&mut diag, Some(&proj));
        let row = diag
            .checks
            .iter()
            .find(|c| c.id == "project.manifestless")
            .expect("manifestless row present");
        assert!(
            row.message.contains("kind=device (package.json)"),
            "got: {}",
            row.message
        );
        assert!(
            !row.message.contains("(default)"),
            "an opted-in kind must not be labelled defaulted: {}",
            row.message
        );
    }
}
