# DEVIATIONS

This file records every place where rackabel's implementation deliberately diverges
from `docs/DESIGN.md` or from the ground-truth toolchain (the official
`@ableton-extensions/*` tarballs and the Arclight launcher scripts). Each entry
names the DESIGN section (or ground-truth source), what was done instead, and why.

Format: one entry per deviation. Append; never silently drop a spec behavior.

---

## 0.2 foundation

### D-1. Error UX vs. the official CLI (DESIGN §6.1 / SPEC A §6.2)

The official `extensions-cli` emits bare one/two-line stderr messages and exits `1`
for everything (usage, validation, environment). rackabel instead uses the three-part
error frame (`error:` / `--> ` / `help:`) and the DESIGN §7 exit-code taxonomy
(usage=2, environment=3, validation=4, build/runtime=1). The official messages are
translated as *content*, not copied as *format*. This is an intentional, spec-mandated
deviation from the official tool.

### D-2. Bare invocation / help exit codes (DESIGN §7 / SPEC A §6.3)

The official `extensions-cli` prints help to stdout but exits `1` on bare invocation.
rackabel follows DESIGN: `--help`/`--version` exit `0`; a bare invocation or an
unknown subcommand is a clap usage error and exits `2`. Recorded as an intentional
divergence from the official exit-`1` behavior.

### D-3. `--no-input` removes the default-accept fallback (DESIGN §7)

The official generator has no answer-level non-interactive mode (only `--skip-install`
and an empty-dir guard); it always prompts. rackabel's global `--no-input` forces
non-interactive mode on every command and turns any branch that would prompt into a
deterministic error (usage `2` for a missing answer, environment `3` for an
environment block), never a silent default-accept. `--yes` still means "accept
defaults"; `--no-input` means "do not prompt and do not invent an answer."

### D-4. User Library pick under `--no-input` (DESIGN deploy / SPEC B §3)

`deploy-extension.js`'s `resolveUserLibrary` silently picks the newest-mtime library
when several exist. rackabel keeps the numbered pick-list for interactive mode, but
under `--no-input` it deterministically picks the newest **and echoes which** (matching
`dev-launch.sh`'s "no TTY → first" behavior) rather than erroring. `RK0301` is reserved
for a genuinely un-pickable case; the newest rule always resolves, so it is not raised
by the User Library resolver in 0.2.

### D-5. `minimum_api_version` inference source (DESIGN §4.2 / SPEC A §2)

The authoritative on-disk source of supported API versions is the SDK bundle's
`EXTENSIONS_API_VERSIONS` constant (`["1.0.0"]`). In the foundation, the manifest
resolver does not yet read the vendored SDK bundle; when `minimum_api_version` is
absent it defaults to `1.0.0` and echoes that it was inferred. Reading the value from
the vendored SDK is the `build`/`new` owner's responsibility (they hold a resolved
`Toolkit`). No behavior is dropped — only the *source* of the default is the constant,
not the bundle, until those commands land.

### D-6. `home` crate replaces `std::env::home_dir` (SPEC C §6)

`src/max/paths.rs` switched from the deprecation-prone `std::env::home_dir` to the
`home` crate. The resolved path is identical; only the deprecation warning is removed.
The M4L `[device]` paths are otherwise unchanged.

### D-7. Crate-wide `#![allow(dead_code)]` during the parallel build-out

rackabel is a binary crate. The foundation freezes a public service/manifest/ui API
that the five parallel command-owner branches consume, but until those bodies land
the items read as `dead_code` even though they are the load-bearing surface. A
crate-level allow (with a comment in `main.rs`) keeps `clippy -D warnings` green
during the parallel phase; it should be tightened once the command bodies exercise
the surface.

---

## 0.2 build

### D-8. Bundle `>10KB` sanity check is warn-only, not a hard failure (DESIGN §2 / SPEC A §5)

SPEC A §5 / `pack-extension.js` treats a sub-10KB `dist/extension.js` as a hard
error, because a real extension always bundles the SDK (verified: the public
extensions bundle to 45KB–258KB). A *minimal* or SDK-less project — a test fixture,
or a future `new --minimal` skeleton, or an extension that legitimately imports
nothing from the SDK — can produce a smaller bundle that still passes `node --check`
and is perfectly valid. So at **build** time rackabel keeps the 10KB floor as a
**non-fatal warning** (the `[!]` line), not a build failure: a valid, parseable small
bundle is not a build error. The hard `node --check` gate (RK1303) is unchanged.
`pack`/`validate` may apply the floor more strictly for distribution artifacts; that
is the pack/validate owners' call. DESIGN §2 explicitly anticipates this ("verify
against SPEC A; deviate via DEVIATIONS.md if the template bundle is legitimately
smaller").

### D-9. esbuild + `tsc` are driven through the project toolchain, not a vendored copy (DESIGN §2, §4.6 / SPEC B §2)

rackabel owns the esbuild *invocation* (so it can bake the polyfill banner the
official `build.ts` omits), but esbuild itself is resolved from the **project's**
`node_modules` via `require.resolve("esbuild", { paths: [projectRoot] })` (which
handles both the npm-hoisted and pnpm `.pnpm/...` layouts), run inside a one-shot
`node -e` process — matching the arclight `scripts/build-extension.js` JS-API model
(SPEC B §2). Likewise `--typecheck` runs the project's pinned `typescript`'s `tsc
--noEmit`. This keeps the dev/CI/build environments byte-identical to what `npm
install` produced and avoids shipping a second esbuild. A missing node is an
environment error (RK0305, "install Live / Node"); a missing esbuild/typescript is a
build error (RK1301/RK1302) with an `npm install` remedy — never a raw
module-not-found or "node not found".

### D-10. Build hash is a non-cryptographic FNV-1a, not a content digest (DESIGN §2)

DESIGN §2 asks for a short build hash so "did it actually rebuild?" is never a
mystery. rackabel uses a 64-bit FNV-1a of the bundle bytes rendered as 12 hex chars.
Change-detection is the only requirement (not integrity/security), so this avoids
pulling in a SHA crate. If a cryptographic digest is ever needed for distribution, it
can be added without changing the build-hash contract.

---

## 0.2 doctor

### D-11. Developer Mode is reported as inferred/unverified, never asserted (DESIGN §2 / §9.2 / SPEC B §6)

Developer Mode is **not statically readable** from disk in 0.2 (DESIGN §9.2 marks the
IPC unverified; no ground-truth script reads a prefs file for it). `doctor` therefore
does **not** claim Developer Mode is on or off. It emits an honest, navigational `[!]`
row (never a `[✓]`/`[✗]`) carrying the §6.2 "turn on Developer Mode" remedy, and — only
under `--verbose`/`--json` — a `detail` line stating the limitation explicitly. The
state is *inferred* behaviorally (running-Live presence + the absence/presence of a
bare host child), which also drives the SIGHUP-unsafe "a non-rackabel Extension Host is
running" warning (DESIGN §6.3). The actual flip is detected by `dev`'s poll-until-toggle
(0.3), not by doctor. Resolves the foundation's "Developer Mode detection is
behavioral/unverified" deferred item.

### D-12. doctor exits via `process::exit`, not a returned `RkError` (DESIGN §6.1 / §7)

Every other command surfaces a failure as a returned `RkError` that `main` renders with
the three-part frame. doctor's checklist (or `--json`) **is** its output: a failing
check is already fully explained inline with its own `help:` line, so returning a framed
error would print a duplicate frame after the checklist and break the §6.2 transcript.
doctor instead reads the exit class off the diagnosis (environment=3 when any check
fails, 0 otherwise — `[!]` warnings never fail) and calls `std::process::exit` after
flushing stdout. The exit-code taxonomy (§7) is unchanged; only the rendering path
differs, and it is contained entirely within the doctor command.

### D-13. Quiet-on-success collapses only when *all* checks pass (DESIGN §2)

DESIGN §2 says "passes collapse to a count unless `--verbose`," but the §6.2 happy
transcript shows every `[✓]` row alongside the `[!]` ones. rackabel reconciles this:
when the run is all-green (no warnings/failures/blocked) the default view collapses to
just the tail count; as soon as there's anything to act on, the **full** checklist
(passing rows included) shows so each `[!]`/`[✗]` has context — matching the transcript.
`--verbose` always shows every row plus the internal `detail` lines.

### D-14. Native-dep "compiled" check is a doctor-local walk, not `native_dep::audit` (SPEC C §3.8)

The frozen `native_dep::audit` service body lands with `deploy` (it is a stub that
errors in 0.2). To avoid coupling the doctor row to that stub, doctor does its own
read-only, best-effort `.node`-presence walk over `<project>/node_modules/<dep>` for
each declared `native_deps` (using the same "don't descend into nested node_modules"
rule as SPEC B's `hasNativeBinary`). When `deploy` lands its `audit`, doctor can be
re-pointed at it; the user-facing remedy ("run `rackabel deploy --fix`") is identical.

### D-15. Process-state probes have a test seam (SPEC C §5 testability contract)

Live-running and bare-host detection use `pgrep` (macOS), which is inherently
machine-dependent. To honor the testability contract (tests never depend on real
machine state), doctor reads two `0`/`1` probe-override env vars —
`RACKABEL_DOCTOR_LIVE_RUNNING` and `RACKABEL_DOCTOR_BARE_HOST` — that pin those probes
deterministically in tests. These are doctor-internal probe overrides, distinct from the
Ctx-routed `ABLETON_*` resolution overrides, and have no effect when unset (real `pgrep`
behavior on a user's machine).
---

## 0.2 validate + explain

### D-16. Host apiVersion is a known constant, not read from the host binary (DESIGN §2 / SPEC A §2)

DESIGN §2's validate rule is `minimumApiVersion ≤ detected host apiVersion`. The
host's *actual* supported apiVersion is only knowable at runtime
(`ActivationContext.hostApiVersion`); it cannot be read from the binary
`ExtensionHostNodeModule.node` on disk. The only static source is the SDK bundle's
`EXTENSIONS_API_VERSIONS` constant (`["1.0.0"]`, SPEC A §2). So `validate` treats a
*detected Live install* (one whose host module exists) as evidence the host supports
that known apiVersion (`1.0.0`) and compares against it; when **no Live is found** it
**skips-with-note** rather than failing or guessing (exactly as DESIGN §2 directs:
"via foundation services; skip-with-note when no Live found"). Reading the precise
host apiVersion at runtime is a 0.3 dev-host concern; reading the SDK bundle's
constant from a vendored toolkit is the build/new owner's job (they hold a resolved
`Toolkit`). No spec behavior is dropped — only the *source* of the host version is the
known constant, not the live host, until 0.3.

### D-17. Stable-identifier drift is not yet machine-checkable (DESIGN §2 / SPEC A §2)

DESIGN §2 wants validate to flag a command id that was present in the last packed
manifest and has since been removed/renamed. SPEC A §2 establishes that commands and
context-menu actions are **registered in code at runtime** and are **not declared in
the manifest** — the on-disk manifest has only name/author/entry/version/
minimumApiVersion. There is therefore no on-disk command list to diff, and the last
packed *manifest* is not retained (only the last packed *version* is in
`.rackabel/state.toml`). validate implements what is checkable and reports the
identifier-drift rule honestly as a **skip** ("not checkable yet") rather than a false
pass; `rackabel explain RK4005` documents the compatibility contract so authors keep
old ids working. Full command-id drift detection waits on a mechanism to capture the
registered ids (e.g. a build-time scan or a packed sidecar), a 0.3+ item.

### D-18. validate's native-`.node` check is a lightweight presence test, not the full graph walk (DESIGN §2 / SPEC B §3 / SPEC C §3.8)

DESIGN §2 includes "native `.node` files present and matching the target". The full
pnpm-aware dependency-graph walk + `.node` assertion (`services::native_dep::audit`,
SPEC C §3.8) is owned by `deploy` and is still a foundation stub that returns
"not implemented". To avoid `validate` depending on an unimplemented service, the
native-`.node` rule performs a self-contained, **read-only** check: for each declared
`[extension.build].native_deps`, it confirms `node_modules/<dep>` exists and contains
at least one `*.node` (not descending into nested `node_modules`, mirroring the
ground-truth `hasNativeBinary`). It does **not** walk the transitive optionalDeps
graph or verify the prebuild matches the target arch — those are deploy/pack
responsibilities. When deploy's `audit` lands, validate should call it for the full
check; until then this presence test catches the common "install scripts didn't run,
so there's no compiled binary" footgun (SPEC B §3) without false-passing.

---

## 0.2 deploy

### D-19. `native_dep::fix` IS implemented (no slip) — but depends on pnpm on PATH (DESIGN §3.7 / SPEC C §3.8)

The foundation shipped `native_dep::fix` as an `RK0304` stub. The deploy branch
replaced it with the real behavior: locate `pnpm` on PATH, run `pnpm approve-builds`
then `pnpm rebuild <dep…>` under the hood, then re-audit that the `.node` binaries now
exist (SPEC B §3). The raw `pnpm` commands are shown only under `--verbose`; the
Persona-A-facing string is always plain English (DESIGN §3.7) — a missing `.node` says
"this extension uses a compiled component that needs to be built — run `rackabel
deploy --fix`", never a bare `pnpm` command. **Deviation:** the spec implies rackabel
"owns the package manager"; in 0.2 it only *drives* pnpm and requires pnpm to be on
PATH (the official scaffold sets projects up with pnpm). If pnpm is absent, `--fix`
fails with a plain-English environment error pointing the developer at installing pnpm
— still no raw module/tool-not-found. A managed/bundled pnpm is a later milestone.

### D-20. `extra_dist_files` ARE copied on deploy (unifying deploy with pack) (SPEC B §3)

Resolved the deferred item: the Arclight `deploy-extension.js` helper copies only
`manifest.json` + `dist/extension.js` (lidal overrides this for `editor-client.js`);
rackabel's deploy copies `[extension.build].extra_dist_files` into `<dest>/dist/` too,
matching pack and lidal's intent so the deployed and packed dist trees are identical. A
declared extra dist file that is missing on disk is a warn-and-skip (parity with
`pack-extension.js`), not a hard failure.

### D-21. build-if-stale uses an mtime check, not the recorded build hash (DESIGN §2 deploy)

DESIGN §2 says deploy runs "build (if stale)". The build owner records a content hash
in `.rackabel/state.toml`, but that hash function is private to `services::esbuild`.
Rather than couple deploy to another module's internal hashing, deploy decides
staleness by comparing the built bundle's mtime against the newest mtime under `src/`
and `rackabel.toml` (which feeds the generated manifest). A freshly-built bundle is
always fresh; a missing bundle/manifest, or any newer source, triggers a rebuild. This
is the classic, self-contained build-if-stale check and never ships a stale bundle (the
deploy-before-reload trap, DESIGN §3). Note this is a *stale* check that prefers
rebuilding; it does not detect a manual edit of the bundle itself (the SDK never wants
that anyway, since the bundle is generated).

### D-22. `--undo` safety contract: refuse a folder without a `manifest.json` (DESIGN §2 deploy)

`--undo` removes `<UserLibrary>/Extensions/<slug>`. To avoid ever `rm -rf`-ing an
unrelated user folder that happens to share the slug, deploy refuses (framed error,
exit 1) when the target exists but does not contain a `manifest.json` — the always-
written member of a rackabel deploy. A not-deployed target is a clean no-op success
(the desired end state already holds), not an error. The spec describes undo as "the
discoverable cleanup path"; this adds the safety gate the spec implies but does not
spell out.

### D-23. `deploy --release` calls `validate::run` directly (DESIGN §2 deploy)

`deploy --release` runs `validate` first and fails the deploy on any validation error,
per DESIGN §2. It does this by calling `crate::commands::validate::run(...)` so it
automatically picks up the real validator once the validate+explain owner lands it.
**Integration note:** while `commands::validate::run` is still the foundation stub
(which returns a build/runtime "not implemented" error), `deploy --release` will
surface that stub error rather than a real validation pass. No behavior is dropped —
the wiring is correct and resolves itself when validate lands; the integrator should
verify the exit-4 path once validate is real. (`--release` is not exercised in the
deploy tests for exactly this reason.)

---

## 0.2 pack

### D-24. `pack` emits `.ablx` only — the Arclight `.zip` layout is dropped (DESIGN §4.7 / SPEC B §4)

The Arclight `pack-extension.js` produces `releases/<slug>-v<version>[-os-arch].zip`
(a User-Library-shaped zip), while the official `extensions-cli package` produces
`<name-ws→dash>-<version>.ablx` (the distribution container). DESIGN §4.7 settles the
format on `.ablx`, so rackabel's `pack` produces **only** `.ablx`, never the Arclight
`.zip`:

- **pure-JS** (no native deps, default) ⇒ shell out to `extensions-cli package`
  (DESIGN §4.7: thin wrapper, no drift), surfacing the exact official
  `<name>-<version>.ablx` filename and passing `-o` to a chosen path.
- **native-dep** extensions, **and** `--no-official-cli` ⇒ rackabel's own packer,
  producing one `<slug>-v<version>-<os>-<arch>.ablx` per declared target. The Arclight
  *naming* (`<slug>-v<version>-<os>-<arch>`) is kept; the *extension* is `.ablx`, not
  `.zip`. The default output directory for the native set is `releases/` (the Arclight
  convention) since there are multiple files; the pure-JS single file defaults to the
  extension dir (the official convention). `-o` overrides: a verbatim path for the
  single pure-JS file, an output **directory** for the native set.

The native `.ablx` member layout is `manifest.json` + the manifest `entry` +
`dist/<extra_dist_files>` + the collected native `node_modules/` (prebuilds slimmed to
the target suffix) — the official packager bundles **no** node_modules (SPEC C §0), so
this is the only path that yields a working native bundle.

### D-25. The lidal `lidal.openEditor` sentinel is dropped (SPEC B §4)

`pack-extension.js`'s pre-flight has a lidal-specific guard: if `slug === "lidal"`, the
bundle text must contain `lidal.openEditor` or it errors (a wrong-bundle hack). This is
project-specific and does not generalize, so rackabel drops it. The portable pre-flight
correctness gates that *do* generalize (bundle exists, `node --check` parses it — both
already enforced by the shared `build` step that `pack` runs first) are kept; the 10KB
floor stays a build-time warning (see D-8).

### D-26. `pack` delegates to `commands::validate::run` for the full ship checklist (DESIGN §2 pack)

DESIGN §2 requires `pack` to auto-run `validate` and fail with exit 4 before producing
a distributable, so pack's gate must mean exactly what `rackabel validate` means.

The original 0.2 foundation had no shared, callable validation service (validate was a
stub), so pack ran an inline *subset* (manifest completeness + `minimumApiVersion`).
That subset skipped the CHANGELOG and version-bump rules, which let `pack` succeed and
print "validation passed" on a project that standalone `rackabel validate` rejected
(e.g. a fresh scaffold with no CHANGELOG) — i.e. it could ship an artifact validate
would fail. That divergence is now fixed: `commands::validate::run` is a real command,
so `pack` calls it directly (the same way `deploy --release` does, D-23) and runs the
*full* checklist (manifest completeness, `minimumApiVersion ≤ host`, version-bump,
CHANGELOG entry, native `.node` presence, identifier drift). The inline subset and its
`SDK_API_VERSIONS` constant were removed. To keep the "passes out of the box" promise
(D-37), `new` now also scaffolds a starter `CHANGELOG.md` with an entry for the
scaffolded version. No spec behavior is dropped — pack's gate is now a superset of the
old one and identical to `validate`.

### D-27. `zip` crate added for the own packer (SPEC C §2)

SPEC C §2 anticipates a Rust zip crate ("e.g. `zip`") for the own packer. The
foundation `Cargo.toml` did not yet include one, so the pack branch adds
`zip = { version = "2", default-features = false, features = ["deflate"] }` (the
flate2-backed deflate path; `flate2` comes in transitively via zip's `deflate`
feature). Byte identity with `archiver` is explicitly **not** a contract (SPEC A §1.4
closing note); the member layout is. The integrator should keep this dependency line.

Note: the foundation also carried explicit `flate2`/`tar` deps anticipating tarball
*extraction*, but toolkit vendoring copies `.tgz` files verbatim and never extracts, so
those two deps were unused in non-test code and have been removed; `flate2` remains
present only transitively under `zip`.

---

## 0.2 new (`rackabel new`)

### D-28. `--author` / `--license` are wizard prompts, not CLI flags (DESIGN §2 / SPEC C frozen CLI)

The task brief lists `--author` and `--license` among `new`'s flags, but the frozen
`cli.rs` (`NewArgs`) ships neither (only `--kind`, `--template`, `--minimal`, `--yes`,
`--no-input`, `--sdk-dir`, `--git`/`--no-git`, `--device-kind`, `--update`). Rather than
mutate the frozen CLI surface, `new` collects author and license **through the wizard**
(with Enter-to-accept defaults: author from `git config user.name`, license `MIT`).
Under `--no-input` neither is required: author follows UX rule 1 (infer-and-echo, never
hard-fail) and is left empty when unknown — `validate` surfaces a missing author later
(`RK4001`) — and license defaults to `MIT`. If first-class `--author`/`--license` flags
are wanted, the integrator should add them to the frozen `NewArgs`; this is noted so the
omission is a recorded decision, not a silent drop. (No spec behavior is lost — both
values are still captured.)

### D-29. The default template is rackabel's fork; the official `create-extension` reuse path is wired but dormant (DESIGN §4.7)

DESIGN §4.7 says `new` should *reuse* the official `create-extension` scaffolder when
present (shell out, then post-process) and use rackabel's fork only when absent. The
gated `create-extension` tarball is **not** vendored into rackabel and is **not**
present in tests (it is beta-gated, like the SDK/CLI). So in 0.2 the **forked** native
scaffold (`scaffold::render`) is the path actually exercised; the post-process side of
§4.7 is implemented (`scaffold::postprocess` derives `rackabel.toml` from the emitted
`manifest.json`/`package.json`, drops `build.ts`, rewrites scripts to call `rackabel`,
keeps the vendored tarballs, adds `.rackabel/` to `.gitignore`) and unit-tested, so the
reuse path is honored the moment the official scaffolder is available — it is not
painted into a corner — but it is dormant until then. The fork produces the **same
shape** the official scaffolder + post-process would, so callers are agnostic to which
ran. The default template is pure-JS only (one command + one AudioClip right-click
action that renames selected clips), so it never pulls a native or UI/vite dep.

### D-30. Generated `package.json` scripts drive `rackabel`, not `tsx build.ts` / `extensions-cli` (DESIGN §4.7 / SPEC A §3.5)

The official template's `package.json` scripts are `tsc --noEmit && tsx build.ts …`
plus `extensions-cli run/package`, and it emits a `build.ts`. Per §4.7 ("replace
`build.ts` with the rackabel pipeline") the rackabel-form project omits `build.ts` and
maps the scripts to `rackabel build` / `rackabel deploy` / `rackabel pack` /
`rackabel dev`. The SDK/CLI are still vendored and wired as `file:` deps (SPEC A §3.4),
and `esbuild` is still pinned at `0.28.0` so a plain `npm install` resolves the same
toolchain rackabel drives. `manifest.json` is **not** hand-written into the project —
`rackabel build` generates it from `rackabel.toml` (§4.5), and it is gitignored.

### D-31. Toolkit "vendoring into the project" = copy SDK+CLI into `<project>/vendor`; the gated tarballs are never committed to rackabel (SPEC A / DESIGN §4)

SPEC A says "vendor the tarballs into the project wired via `file:` deps." Because the
SDK/CLI tarballs are beta-gated (not redistributable, not on public npm), rackabel does
**not** commit them into its own repo. Instead `new` discovers the user's downloaded
toolkit (`toolkit::discover`) and copies the discovered SDK+CLI into the new project's
`vendor/` (`toolkit::vendor_into`), then writes `file:./vendor/<basename>` deps that
match the vendored files. Tests fabricate tiny stand-in `.tgz` fixtures
(`tests/fixtures/toolkit/`) — they never depend on the real gated tarballs.

### D-32. Wizard answers are remembered under `$RACKABEL_HOME`, keyed by project name (DESIGN §6.2)

The §6.2 SDK-not-found transcript promises "Your answers above are remembered." rackabel
persists the wizard answers to `$RACKABEL_HOME/new-answers/<sanitized-name>.toml` on the
SDK-not-found stop, seeds the wizard's Enter-to-accept defaults from that file on the
re-run for the same name, and clears it after a successful scaffold. This is the
mechanism behind the promise; it is best-effort (a write/read failure is swallowed so a
failure to remember never becomes a second error). The `--update` 3-way-merge answers
lockfile (`.rackabel-template`, §5.5) is a *separate*, 0.4 concern and is not
implemented here.

### D-33. Remote `--template` refs (`gh:`/`@scope`) are a framed "coming in 0.4" usage error (DESIGN §5.5)

Remote template fetch + render is the 0.4 templates milestone. In 0.2, `new` accepts the
`--template` flag but, for a remote ref, returns a three-part usage error (exit 2)
pointing at 0.4 rather than silently falling back to the default. A **local** `--template
<path>` is accepted without error but is not yet applied (the built-in default template
is used); applying a local template directory also lands with tier-1 templates in 0.4.
This honors the brief ("accept the flag but print a framed not-yet-supported error for
remote refs") without painting the 0.4 work into a corner.

### D-34. SDK-not-found / no-node trycmd vs. integration split (SPEC C §5)

The §6.2 SDK-not-found and `--no-input` cases are deterministic regardless of the host
machine (they fail before any Live/node detection), so they are **trycmd** transcript
acceptance tests (`tests/cli/new_no_toolkit.trycmd`, `new_no_input.trycmd`). The happy
scaffold and the no-node friendly-skip depend on Live/PATH-node *absence*, which the
shared trycmd harness cannot isolate on a developer's real machine (Live and a PATH node
may be present). Those two — plus answer-persistence, git-init/`--no-git`/`--minimal`,
and the device dispatch — are **assert_cmd integration tests** (`tests/integration/new.rs`)
that pin `ABLETON_APP` to a no-host fake `.app` and strip `PATH` to a node-free dir, so
Live/node detection is deterministic. Same coverage, deterministic across machines.

### D-35. The Extensions-beta URL is centralized in one module with an env override (DESIGN §6.2)

§6.2 requires the placeholder beta URL to come from "remote/updatable config — not a
hard-coded constant" scattered through the code. There is no remote-config fetch in 0.2
(that lands with the dev-host/registry milestone), so the URL lives in exactly **one**
module (`commands/new/config.rs`) and is read everywhere from there, with a
`RACKABEL_EXTENSIONS_BETA_URL` env override so a moved page can be corrected without a
release in the interim. When remote config arrives, only this module changes.

---

## Deferred (to be recorded by the owning command branch when it lands)

These are flagged in the specs as likely deviations but belong to a command body the
foundation only stubs; the owning branch records the final decision here.

- **`native_dep::fix` full pnpm automation** (DESIGN §3.7 / SPEC C §3.8). Resolved by
  the deploy branch — see the deploy section above.
- **pack dual-format `.ablx` vs `.zip`** (DESIGN §4.7 / SPEC B §4). Resolved by the
  pack branch — see the pack section above.
- **Developer Mode detection is behavioral/unverified** (DESIGN §9.2 / SPEC B §6).
  RESOLVED by the doctor branch — see the doctor section above.
- **`extra_dist_files` copied on deploy** (SPEC B §3). Resolved by the deploy branch —
  see the deploy section above.

---

## 0.2 integration (merge + smoke test)

These were resolved while merging the five command branches and smoke-testing the
combined 0.2 surface end to end against the real SDK tarballs.

### D-36. `--yes` accepts defaults non-interactively, like `--no-input` for present defaults (DESIGN §1 / D-3)

`new --yes` means "accept defaults." The frozen `ui::prompt::text` only auto-accepts a
default under `ctx.no_input`, so before this fix `new --yes` on a non-TTY still tried to
prompt for author/license and failed. `new`'s wizard now resolves defaults directly when
`--yes` (or `--no-input`) is set — author from `git config user.name` (else empty,
surfaced later by `validate` per UX rule 1), license `MIT` — and only reaches an
interactive prompt when neither flag is set. A missing project *name* under either flag
remains a deterministic usage error (no inferable default). This is the accept-defaults
half of D-3; no `ui::prompt` signature changed.

### D-37. The default `new` template uses verified SDK API only (DESIGN §2 / SPEC A §3.5)

The default template originally registered a "rename selected clips" command using
`song.view.selectedClips`, which is **not** part of the SDK's `Song` type — `rackabel
build` (no typecheck) succeeded but `pack`/`--typecheck` failed `tsc --noEmit`. The
template now ships a type-correct AudioClip right-click action that renames the clip the
action was triggered on, using the documented `getObjectFromHandle(args[0] as Handle,
AudioClip)` pattern (SPEC A §4 surface). The shape is unchanged (one command + one
AudioClip context-menu action, pure-JS only); only the body is now valid against the
real SDK. Combined with the starter `CHANGELOG.md` the scaffold now writes (see D-26),
a freshly-scaffolded project passes `validate`/`pack` out of the box (once its
dependencies are installed — see D-41 for the auto-build/install behavior).

### D-38. `doctor` resolves the User Library non-interactively (DESIGN §2 / SPEC C §3.4)

`user_library::resolve` prompts (numbered pick-list) when several
`~/Music/Ableton*/User Library` folders exist and `--no-input` is not set. `doctor` is a
diagnostic and must never prompt, so it forces the deterministic newest-wins resolution
(the same path `--no-input` takes) for its User-Library row. Without this, a machine with
both an "Ableton" and an "Ableton Alpha" library made `doctor` report a spurious
"couldn't find your User Library" on a non-TTY. No resolver signature changed; doctor
passes a `no_input`-forced clone of its quiet context.

### D-39. `doctor::preflight` is available but not wired into build/deploy/pack `run()` (SPEC C §3.7)

The doctor branch exposed `doctor::preflight(Preflight::{Build,Deploy,Pack}, …)` and
suggested each command call it at the top of `run()`. The integrator did **not** wire it
in: build/deploy/pack already perform their own, more granular environment resolution
(via `services::esbuild`/`user_library`/`live`) and already return correctly-framed
RK03xx environment errors with the right remedies and exit code 3 (covered by their
tests). Forcing a `preflight` that requires node would also break the intentionally
hermetic `build`/`pack` dry-run paths (which never touch node). `preflight` remains
available and unit-tested for the 0.3 dev host; wiring it into the command bodies is a
follow-up that should reconcile it with the dry-run paths first. No spec behavior is
lost — the same remedies are produced today by the commands' own checks.

### D-40. User Library source label is approximate for `--user-library` (known foundation limitation)

`--user-library` (flag) and `$ABLETON_USER_LIBRARY` (env) are merged into a single
`ctx.ableton_user_library` at `Ctx` construction (flag wins), so the resolver cannot
tell them apart and labels both "(from ABLETON_USER_LIBRARY)" in its echo. The
`ULSource::Flag` variant is therefore not reachable post-merge. This is a pre-existing,
documented approximation in the frozen foundation resolver (a cosmetic echo only — the
flag still wins); distinguishing the two would require a foundation `Ctx` change and is
left for a later coordinated bump.

### D-42. `validate --strict` flag (additive to the DESIGN §2 synopsis)

DESIGN §2's validate synopsis is `rackabel validate [--json]`. The implementation adds
a `--strict` flag (frozen in `cli.rs` `ValidateArgs`) that promotes every warning-tier
rule (e.g. stable-identifier drift) to a failure (exit 4). This is an **additive**
escape hatch for CI ("treat compatibility risks as blocking") that does not change the
default behavior — without `--strict`, warnings remain non-fatal exactly as the spec
describes. Recorded here so the extra flag is a documented decision, not a silent
surface addition; it is the same `--strict` `explain RK4005` already references.

### D-41. `new`'s auto-build friendly-skips when dependencies aren't installed (DESIGN §0 / §6.2)

DESIGN §0/§6.2's happy path shows `✓ built your extension (84ms)` immediately after
`new`. But `new` vendors the SDK/CLI tarballs and pins `esbuild` in `package.json`
*without* running an install (it has no managed/bundled package manager in 0.2; the
install is `npm install`, which the musician runs once). So on a brand-new project there
is no `node_modules`, and an immediate build would fail with the scary RK1301 "couldn't
find esbuild" frame on EVERY first run — exactly DESIGN §1's "first thing they see is
error:" trap. `maybe_auto_build` therefore checks for `node_modules` first: when a usable
node exists but deps are not installed, it **friendly-skips** with the exact next steps
(`cd <dir> && npm install`, then `rackabel build`/`dev`) instead of dead-ending on a red
error — the same shape as the existing no-node skip. The auto-build still runs (and shows
the `✓ built` line) once deps are installed (e.g. on a second `new` into a populated dir,
or after the user installs). A `new` that vendors and *also* runs the install is a later
improvement once rackabel drives a package manager end-to-end (cf. D-19); the contract
("a successful `new` never shows a red `error:`") is honored now. No spec behavior is
dropped — the build still happens, just after the one documented `npm install` step.

---

## 0.3 dev host (foundation)

These were decided while landing the 0.3 FOUNDATION (clap surface for the `dev` group,
shared types, module stubs, registry model + IO, FakeHost fixture, new RK codes). SPEC
D (daemon architecture) and SPEC H (verified host behavior) are the authoritative 0.3
inputs; entries note where the implementation refines them.

### D-43. `RK0312 NameCollision` is exit class 2 (usage), not 3 (SPEC D §3)

SPEC D §3's error table lists `RK0312 NameCollision` as "usage, 2". One sentence later
the §3 prose groups it loosely with the environment codes. The implementation honors
the table: `NameCollision.class()` is `ExitClass::Usage` (exit 2), because a `--name`
that collides irrecoverably under `--no-input` is an *invocation* the user can fix by
passing a different name (the same class as the existing `UsageError`). Recorded so the
2-vs-3 choice is explicit; the §3 table is the source of truth.

### D-44. NDJSON wire form uses an envelope struct, not a flattened `v` field (SPEC D §2/§4)

SPEC D §4 sketches `Request`/`Response` enums each carrying `#[serde(default)] v: u32`.
A `#[serde(tag="type")]` enum cannot also hold a sibling field, so the implementation
puts the protocol version on a thin `RequestEnvelope`/`ResponseEnvelope` wrapper
(`{ v, #[serde(flatten)] request|response }`). The on-the-wire JSON is byte-identical
to the spec's intent — `{"v":1,"type":"ping"}` — and `v` still defaults to
`DEV_PROTOCOL_VERSION` and is checked on receipt (`RK0308` on mismatch). The enums
themselves stay clean (`Request`/`Response`), matching the spec's variant names.

### D-45. A `Subscribe` request is added to the IPC table (SPEC D §2)

SPEC D §2 describes an unsolicited daemon→client `event` stream "on a subscribed conn"
but lists no request that performs the subscribe. The implementation adds an explicit
`Request::Subscribe` so the bare-`dev`/`watch` UI can opt a connection into the
`DevEvent` broadcast (the spec's `event (daemon→client …)` row). This is additive and
does not change any existing message; it just names the action the spec implied.

### D-46. `autotests = false` + explicit test targets for the FakeHost bin (SPEC D §6)

SPEC D §6 offers the FakeHost as either a `[[bin]]` or a committed shell script. The
bin form is chosen (a real Rust binary tests resolve via assert_cmd's cargo-bin lookup,
giving deterministic behavior + the `RK_FAKEHOST_CRASH`/`HANG`/`EXT` seams). Because the
bin lives at `tests/fakehost/main.rs`, cargo's test auto-discovery would ALSO claim it
as an integration-test target ("present in multiple build targets"). Fix: set
`autotests = false` and declare the two real test targets (`integration`, `trycmd`)
explicitly — matching the existing explicit `[[test]] integration` style. No test is
lost; discovery is just made explicit.

### D-47. Foundation freezes `Inspect` + extra path helpers beyond the SPEC D §4 sketch

SPEC D §4 references a `host:port` inspect value and per-Live `sock_path`/`pid_path`/
`log_dir`. The foundation adds the small supporting surface those imply so every agent
compiles against one definition: an `Inspect { host, port }` type with a `parse()` for
the `--inspect[=host:port]` forms + a `default_endpoint()` (127.0.0.1:9229, §7), and a
`host_out_path` helper (the daemon's captured-host-stdio file, SPEC D §1). Additive,
no spec behavior changed.

### D-48. Registry CRUD model is implemented in the foundation (not stubbed) (SPEC D §3/§4)

SPEC D assigns the registry *verbs* (`dev register`/`list`/…) to the REGISTRY agent and
the registry *model* (serde + file IO + `RACKABEL_HOME` pathing) to the FOUNDATION. The
two are entangled (the verbs are thin wrappers over `Registry::add`/`remove`/
`set_enabled`/`disambiguate`/`prefilter`), so the foundation implements the FULL
`Registry` model from the §4 signature — including working `add`/`add_recursive`/
`remove`/`set_enabled`/`disambiguate`/`prefilter` and the `O_EXCL` lockfile — and only
the command *verbs* are stubbed. This keeps the model correct and unit-tested from day
one (the registry agent wires the verbs + the interactive/`--no-input` `RK0312` policy
and the `[workspace].members` reconciliation on top of it). The command files remain
exclusively the registry agent's to fill.

## 0.3 daemon-core

### D-49. `RACKABEL_HOST_CMD` host-launch seam lands on `Ctx` (SPEC D §6)

SPEC D §6 recommends a `host_cmd_override` on `HostConfig` fed from a `RACKABEL_HOST_CMD`
env read in `Ctx`, as the single clean seam that keeps daemon-lifecycle tests off real
Live/node. Implemented exactly: `Ctx.rackabel_host_cmd: Option<Vec<String>>` parses
`RACKABEL_HOST_CMD` (split on `\x1f` if present, else whitespace) at `Ctx` construction;
`resolve::host_config` threads it into `HostConfig::host_cmd_override`; `Host` runs it
verbatim (with `--inspect` still appended) instead of the `EH_NODE -e require(EH_MOD)…`
recipe. This is the spec's own recommendation, recorded because it adds a public `Ctx`
field. Production code never sets it; only the FakeHost-driven tests do.

### D-50. `Ctx` derives `Clone` (DESIGN §3.1 daemon model)

The detached daemon supervises the host from a background thread that re-uses
`Ctx`-taking 0.2 services (`user_library::resolve_newest`, `resolve::host_config`) across
the request/supervisor boundary, where a borrow cannot outlive the call. `Ctx` is made
`Clone` so the daemon owns a snapshot. All fields were already `Clone`; purely additive,
no behavior change.

### D-51. `.rackabel/state.toml` gains `dev_live` for the persisted multi-Live choice (DESIGN §3.6, SPEC D §7)

SPEC D §7 / DESIGN §3.6 require the chosen Live `.app` to be persisted in
`.rackabel/state.toml` (project) so `rackabel dev` recalls it instead of re-prompting, and
keyed into the per-Live daemon socket/pidfile hash. The sidecar `State` (DESIGN §4.3)
gains an optional `dev_live: Option<String>` field, written best-effort by
`resolve::resolve`. Additive + backward-compatible (a missing field defaults to `None`).

### D-52. Daemon-core subdivides its owned surface into `preflight`/`resolve`/`launch_config` modules

SPEC D §3 assigns `dev/host.rs` + `dev/daemon.rs` + `commands/dev/{start,stop,status}.rs`
to daemon-core without prescribing internal helper modules. To keep those files focused,
daemon-core adds three small **daemon-core-owned** modules: `dev/preflight.rs` (the §3.6
Live-running / Dev-Mode block-and-wait gates + the behavioral detection seams),
`dev/resolve.rs` (the §3.6 Live + host-binary resolution, persisted choice, and the §3.6
storage/temp `ExtensionSpec` builder), and `dev/launch_config.rs` (the §7
`--emit-launch-config` writer). All are within daemon-core's assigned scope; no other
agent's file is touched. The watch/registry/logs/dev-test modules remain stubs owned by
their agents.

### D-53. The `LogSink` stub is implemented as a working fan-out (logs agent enriches later)

SPEC D §3 assigns `dev/logs.rs` to the LOGS agent. The daemon-core host lifecycle,
however, *requires* a non-panicking sink to capture the host child's stdout/stderr and to
broadcast lifecycle events to `dev logs --follow` subscribers — a `todo_err` stub would
make the daemon unrunnable and untestable. Daemon-core therefore lands a minimal-but-real
fan-out in `logs.rs` (a session log file + broadcast senders + best-effort `[<ext>]:`
attribution), keeping the frozen `LogSink`/`LogLine`/`LineKind` surface intact. The LOGS
agent layers the rich `ExtensionHost.txt` parser, per-extension keying, and sourcemap
mapping on top without changing that surface (`map_through_sourcemap`/`tail_exthost`
remain its stubs).

### D-54. Daemon runs its own accept loop instead of `ipc::serve` (SPEC D §2)

SPEC D §2's `ipc::serve(listener, handler)` scaffold blocks forever on
`listener.incoming()`, which cannot honor a `shutdown` request. The daemon instead runs an
equivalent nonblocking accept loop (`serve_until_shutdown`) that polls the shutdown flag,
spawning one thread per connection and dispatching to the same `ipc::Handler` trait object
the foundation froze. Same wire protocol, same handler contract; only the accept-loop
plumbing is daemon-local so the daemon can shut down cleanly (killpg host group, unlink
socket + pidfile, exit). The foundation `ipc::serve` is left in place for any caller that
wants the simple blocking form.

### D-55. Crash mode in the FakeHost prints the connected marker before crashing (SPEC D §6, SPEC H §6)

The foundation FakeHost's `RK_FAKEHOST_CRASH` exited before printing the connected marker.
The verified host abort (incompatible `minimumApiVersion` → uncaughtException during
*activation*, SPEC H §6 q9_6) happens AFTER the Live greeting — so the daemon's
crash-recovery supervisor only fires on a host that reached `Running` then died. The
FakeHost's crash mode now prints the connected marker + liveness first by default
(`RK_FAKEHOST_CRASH_PRECONNECT=1` restores the pre-connect crash for the launch-fails
case), so the crash-respawn/crash-loop tests exercise the real supervisor path. The
existing "crash mode exits non-zero" fixture assertion is unaffected.

### D-56. `dev reload --strict` skip is exit 1, not RK4006's validation class 4 (DESIGN §7 / SPEC D §3)

SPEC D §3 lists `RK4006 SkippedIncompatible` as a *validation* code (exit class 4), but
DESIGN §7 and the milestone task both require `dev reload --strict` to make any
pre-filtered skip **fatal exit 1** ("`--strict`, which makes any skip fatal exit 1
(RK4006/treated as a strict failure)"). The `reload` verb resolves this by keeping the
`RK4006` code (so `rackabel explain RK4006` still describes the skip) while constructing
the error with an explicit `ExitClass::BuildRuntime` via `RkError::new`, so `--strict`
exits 1 as documented. Without `--strict` a skip is reported on stderr / in `--json` and
the exit stays 0. The foundation's `ErrorCode::class()` default for `RK4006` (validation)
is unchanged; only this one `--strict`-failure site overrides the class.

### D-57. `dev reload` locates the daemon by scanning the daemon dir, not via Live resolution (DESIGN §3.2/§3.6 / SPEC D §2)

The reload verb must give a clean `exit 3 RK0309` when no daemon is up — even on a machine
with no Live install (the §6.2/§7 scriptable-CI contract, exercised hermetically by
trycmd + the fake-daemon integration tests). Routing through full Live resolution first
(`resolve::resolve` → `live::primary`) would instead surface an unrelated `RK0303
NoLiveInstall` on such a machine. So `connect_daemon` (a small shared helper in
`commands/dev/mod.rs`) finds the daemon by scanning `~/.rackabel/daemon/*.sock` and
connecting to the first socket that answers, falling back to `RK0309` when none do. With
one daemon per Live install this is unambiguous in the common case; multi-Live
disambiguation (keying off the resolved socket hash) is deferred to when a second daemon
can actually coexist.

### D-58. `register --name` collision is a usage error, not an interactive auto-rename (DESIGN §3.2)

DESIGN §3.2 describes auto-disambiguation on collisions. For an *explicit* `--name` that
collides with an existing entry, rackabel treats the invocation as unsatisfiable-as-written
and returns `RK0312 NameCollision` (usage, exit 2) naming a free alternative, rather than
silently renaming what the user explicitly asked for or prompting (which would leak an
RK4001 "no interactive terminal" in non-TTY/CI contexts and make the transcript
non-deterministic). The *auto* name path (no `--name`) still auto-disambiguates the dir
basename against existing names and reserved verbs (parent-prefixed, echoed). A `--name`
equal to a reserved dev verb is still *forced* with a warning (stored verbatim, reachable
only via `--only`/`--`), via the new `Registry::add_named` (extends, does not reshape, the
foundation model).

### D-59. `dev test` adds the `RK1308 TestFailed` error code (DESIGN §3.8, §7)

SPEC D §3 enumerates the new `RKxxxx` codes the foundation lands but does not include a
code for a `dev test` test *failure* — only `RK1306 ReloadActivateFailed` (an
`activate()` throw on a host reload) and the daemon/host environment codes exist in the
build/runtime class. The §3.8 CI entry point needs its own honest code: a failing
headless test run is a build/runtime failure (exit 1, §7 taxonomy) but is semantically
distinct from a reload `activate()` throw, and the engineering bar requires a three-part
frame + `explain` entry per expected failure. The dev-test agent therefore adds
`ErrorCode::TestFailed` (`RK1308`, BuildRuntime/exit 1) to `src/error.rs` (variant +
`as_str`/`class`/`ALL` arms) and its `short_title`/`long_form` prose to
`src/commands/explain.rs`. No other error code changes; the addition is purely additive
(every prior code keeps its string + class). The integrator should fold this into the
foundation error table.

### D-60. `dev test` gags the 0.2 build's stdout under `--json` instead of adding a `BuildOptions` field

DESIGN §3.8 requires `dev test --json` to emit a clean wrapper envelope as the SOLE
contents of stdout (a CI script reading `dev test --json` must always get one parseable
object). The reused 0.2 `esbuild::build_extension` prints its own banner/JSON line to
stdout via `report_success` keyed on `ctx.json`, which would pollute the envelope. Rather
than add a `quiet` field to `BuildOptions` — which would force edits to every existing
struct-literal constructor across build/pack/deploy (other agents' 0.2 files) — the
dev-test agent wraps the build call in a scoped `StdoutGag` that redirects the process's
stdout fd to `/dev/null` for the build's duration and restores it on drop (unix-only;
the whole `dev` module is `#[cfg(unix)]`). This is fully self-contained in
`test_cmd.rs`. In HUMAN mode no gag is applied, so the build banner prints inline as
§3.8 specifies ("build … (banner)").

### D-61. Foundation `dev test` stub tests updated to the real routing/behavior

The foundation landed two tests encoding the `dev test` STUB output (exit 1 / `RK1306`):
`tests/cli/dev_surface.trycmd`'s `dev test` block and
`tests/integration/dev.rs::dev_verb_wins_over_name`. With the real `dev test`, an empty
cwd/registry yields `RK0001` (nothing to test, exit 3) — still proving the verb routed
to the test runner (NOT the bare loop's `RK0307`). Both tests were updated to assert the
real contract (`RK0001` present, `RK0307` absent). The name-vs-verb routing guarantee is
preserved; only the stub's placeholder code/exit changed. `tests/cli/explain.trycmd`'s
valid-codes list gained `RK1308`.

### D-62. `dev logs` non-`--follow` reads persisted session files directly, not the daemon (DESIGN §3.4, SPEC D §2)

SPEC D §2 lists a `Logs{follow:false}` request whose response replays history then
`LogEnd`. The LOGS command instead serves a non-`--follow` read **entirely from the
persisted per-extension session files** (`~/.rackabel/logs/<name>/<session>.log`), only
using the daemon socket for `--follow` live streaming. Rationale: (a) DESIGN §3.4 mandates
that a read-only `dev logs` "must work with a dead daemon" — the file path is the single
source of truth that satisfies that unconditionally; (b) the files are complete and
already merged across session rotation, so there is no behavioral gap when the daemon is
up vs down; (c) it avoids depending on the daemon-core `stream_logs` non-follow handler
(another agent's file) replaying the in-memory ring. The sink still keeps an in-memory
history ring + exposes `LogSink::history()` so the daemon CAN replay over the socket if a
future client wants it; the command simply doesn't need it. `--follow` still prefers the
daemon's live broadcast and falls back to file-tailing the newest session log when no
daemon is up.

### D-63. Per-extension session sinks use a tab-delimited record format (DESIGN §3.4)

DESIGN §3.4 calls for "leveled timestamped per-extension sinks". The persisted line format
is a tab-delimited record `ts_ms\tlevel\tkind\text\ttext` (the foundation stub wrote raw
host text only). This structured form is the contract the dead-daemon file-tail fallback
reconstructs each `LogLine` from (ts/level/kind/ext attribution survives a daemon
restart); tabs/newlines in the message are flattened to spaces so a record stays one line.
Lines fan out to both the per-extension file (when attributed) and a shared
`_session/<session>.log` for host/unattributable lines. The frozen `LogSink`/`LogLine`
surface is unchanged. The `ExtensionHost.txt` tail + parser (`tail_exthost`) and
`map_through_sourcemap` (a small self-contained Base64-VLQ decoder over the dev build's
emitted `.map` — no new crate, per SPEC D §5) are now implemented; activate-failure stack
frames pointing at `dist/extension.js:L:C` are mapped back to `src` `file:line` and surfaced
as a framed `ActivateFailed` line.

### D-64. `build_deploy_reload` takes `&mut Client` (SPEC D §4 froze `&ipc::Client`)

SPEC D §4 lists `build_deploy_reload(changed, client: &ipc::Client, ctx)`, but the
foundation's real `ipc::Client::call`/`stream` take `&mut self` (they write a request then
read the response on the same stream), so a shared `&Client` cannot drive a round-trip.
The watch-loop chain therefore takes `&mut Client` — the same shape `dev status`'s
`apply_inspect` and every other client call site already use. Pure signature accommodation
of the frozen `Client` API; the build→deploy→reload ordering contract is unchanged.

### D-65. `[dev].debounce_ms` is read by parsing raw TOML, not via `ManifestRaw` (SPEC D §4.3, DESIGN §3.3)

DESIGN §3.3 specifies a `[dev].debounce_ms` override. The watch-loop agent originally read
it by parsing `rackabel.toml` as a generic `toml::Value`, because the foundation's
`ManifestRaw` (`src/manifest/mod.rs`, `#[serde(deny_unknown_fields)]`) had no `[dev]`
table. RESOLVED BY THE INTEGRATOR: a `[dev]` table with `debounce_ms` is now first-class on
`ManifestRaw` (an `Option<Dev>` field + a `Dev` struct), so a manifest carrying `[dev]` is
accepted by *every* command (previously `deny_unknown_fields` rejected `[dev]` as `RK0003`
in build/deploy/validate/pack while the watch loop read it separately — an inconsistency a
user would hit the moment they added the documented knob). `watch_cmd::read_debounce_from`
now reads the typed `project.raw.dev.debounce_ms` via `Project::discover`, defaulting to
200 ms when absent/non-positive. The `[dev]` table is documented in the README manifest
example.

### D-66. Watch chain quiets reused build/deploy via a `json`-set ctx clone (DESIGN §8)

DESIGN §8 says the watch chain "calls deploy; do NOT reimplement the copy set," but
`commands::deploy::run` (and `esbuild::build_extension`) always emit their own success
output, which would clutter the single `reloaded …` line the loop owns. Since
`commands::deploy`'s copy set (`CopySet`) is private to that 0.2 file, the chain reuses
`deploy::run` verbatim against a project-rooted clone of `Ctx`. UPDATE (fixer round 1):
the previous `json = true` overload was WRONG — both services' `report_success` branch on
`ctx.json` and, when true, `println!` a pretty-JSON envelope to stdout, so a bare
`rackabel dev` (no `--json`) printed ~20 lines of machine JSON per save above the intended
`rebuilt … → reloaded` line (the §6.2 clean-transcript promise was broken). The follow-up
this note anticipated is now done: a dedicated `Ctx.quiet` flag (never set from a CLI flag)
that both `report_success` sites honor by emitting NOTHING — neither the human frame nor
the JSON envelope. `echo_on()` also returns false under `quiet`. The watch chain's
`quiet_project_ctx` and the daemon's pre-launch `deploy_one` now set `quiet` (not `json`).
The chain also skips the esbuild exec when the bundle is already fresh (same build-if-stale
notion `deploy` uses), so a save outside the build inputs is a no-op rather than a redundant
rebuild.

### D-67. The bare-loop `--only`-routing transcript moved from trycmd to a hermetic integration test (SPEC D §6, DESIGN §6.2)

The foundation `tests/cli/dev_surface.trycmd` asserted the *stubbed* bare-loop output for
`rackabel dev --only test` (RK0307 "not implemented yet"). With the real loop implemented,
that case is environment-dependent (it gates on Live + the daemon) and so cannot be a
Live-agnostic transcript. The trycmd case was removed and the property it proved — `--only`
routes through the registry name matcher and into the bare loop, NEVER the `test` verb (so
the bare loop's environment exit 3, not the verb's exit 1) — is now covered hermetically in
`tests/integration/dev.rs::only_routes_through_name_matcher_not_verbs` (pinned with a
`FakeLive` + `RACKABEL_DOCTOR_LIVE_RUNNING=0`) and in the WATCH-LOOP suite
`tests/integration/dev_watch.rs`. No surface guarantee was dropped; it moved to a
deterministic harness.

---

## 0.3 integration

### D-68. trycmd hermeticity: per-transcript `RACKABEL_HOME`, no suite-level state root (INTEGRATOR)

Merging the four 0.3 agent branches put three *stateful* dev transcripts into the one
trycmd binary: `dev_registry.trycmd` (registry CRUD), `dev_register_names.trycmd` (name /
verb collisions), and the `dev test` case in `dev_surface.trycmd` (which asserts an empty
registry → `RK0001`). The foundation harness set a single suite-level
`RACKABEL_HOME=<scratch>` for all cases. Two problems surfaced only once the verbs were
real: (a) all three transcripts then shared ONE registry.toml and, because trycmd does not
guarantee case order, raced — `dev test` would observe entries another transcript had just
registered; (b) trycmd's env merge makes the SUITE env win over a per-command inline env
(`Env::update` does `step.add.extend(suite.add)`), so an inline `RACKABEL_HOME=…` could not
override the suite value. The integrator therefore (1) dropped the suite-level
`RACKABEL_HOME` (keeping only `HOME=<pid-keyed scratch>` as the real-`~/.rackabel` safety
net), so each command's inline `RACKABEL_HOME` now takes effect; (2) pinned a DISTINCT
inline `RACKABEL_HOME=rk-home-<file>` on every command of the three stateful transcripts so
they are isolated from each other regardless of case order; (3) wipes those (gitignored)
relative state dirs at harness start so a run never inherits residue. No transcript
*content* contract changed; only the state-root plumbing. The `RACKABEL_HOME=rk-home`
validate transcripts now also genuinely isolate (previously the suite value silently won).

---

## 0.3 live verification

### D-69. `dev start` / `dev` must build+deploy the enabled set BEFORE launching the host (DESIGN §3.3, SPEC H §1)

Live verification surfaced a launch-time gap: the daemon's `launch_initial` (and the
fresh-launch + reload paths in `do_reload`) built the host's `initialize()` array from
`resolve::host_config`, which points each extension at its DEPLOYED bundle
(`<UserLibrary>/Extensions/<slug>`), but nothing actually deployed there first. The watch
loop only deploys on a *subsequent* file change; the very first `dev start` / bare `dev`
sync pointed the host at a non-existent `manifest.json`, so the real Extension Host threw
`Failed to read manifest.json … not found` as an uncaughtException and exited code 1 —
crash-looping until the supervisor gave up. DESIGN §3.3's atomic build→deploy→reload is not
only a per-edit invariant; the initial host launch needs the deployed bundle on disk too.
Fix: `DaemonState::deploy_kept` builds-if-stale + deploys each kept `source = dist` entry
(reusing the 0.2 `deploy` service verbatim, quiet/`json` ctx) before the host config is
built, called from both `launch_initial` and `do_reload`. It is a cheap no-op when the
watch chain already deployed (build-if-stale skips a fresh bundle) and is the step that puts
the bundle on disk for `dev start`, `set_working_set`'s implicit reload, and manual
`dev reload`.

### D-70. macOS `accept(2)` inherits the listener's non-blocking flag — clear it per connection (SPEC D §2)

The daemon's socket server makes the LISTENER non-blocking so the accept loop can poll the
shutdown flag (SPEC D §2 design). On macOS/BSD, `accept(2)` inherits `O_NONBLOCK` from the
listening socket onto each accepted connection (Linux does not). The per-connection handler
does a *blocking* `read_line` loop to serve many requests on one persistent connection —
which is exactly what the bare-`dev`/watch UI relies on (one `Client` open for
`SetWorkingSet`, `Status`, and every `Reload`). With the inherited non-blocking flag, the
second `read_line` returned `WouldBlock`, the handler treated it as EOF and tore the
connection down, so the watch loop's reload-after-edit failed with `RK0309 lost the
connection` and the headline live-reload silently loaded the previous bundle. Fix:
`stream.set_nonblocking(false)` on every accepted connection in `serve_until_shutdown`
(no-op on Linux). Regression test:
`tests/integration/dev_daemon.rs::multiple_requests_on_one_connection` (three requests on
one stream; fails with BrokenPipe without the fix). The prior socket round-trip test masked
this by opening a fresh connection per request.

### D-71. Watch path matching must canonicalize symlinked prefixes (DESIGN §3.3 / §4.4)

The OS file-watcher (FSEvents on macOS) reports the CANONICAL real path of a changed file,
while the registry stores the path the user typed. When a project lives under a symlinked
prefix — the everyday macOS case `/tmp` → `/private/tmp`, and firmlinked `/Users` — the
watch classifier's `path.starts_with(&entry.root)` never matched the incoming event, so NO
reload ever fired on save. Fix: a `path_under(path, base)` helper that falls back to
comparing `canonicalize(path)` against `canonicalize(base)` when the lexical `starts_with`
misses; used for both the extension-root and workspace-lib matches in `ChangeSet::classify`.
Regression test:
`watch::tests::classify_matches_through_a_symlinked_path_prefix` (registers via a symlink
alias, feeds classify the canonical path).

### D-72. Registry-miss in `dev enable`/`disable`/`unregister`/`reload <name>` is a usage error (`RK0102`), not `RK0309` (DESIGN §3.2, §7)

A typo'd name (`dev disable nope`) used to surface `RK0309 NoDaemon` (exit 3), whose
`explain` text tells the user to "start the dev host" — wrong class AND wrong remedy, since
these registry verbs deliberately work WITHOUT a daemon. Added a dedicated
`ErrorCode::NoSuchExtension` (`RK0102`, usage class / exit 2) with a remedy that points at
`dev list` / `dev register`; `registry::not_found` now uses it. New `explain` prose +
`short_title`; the `explain RK9999` valid-codes list gains `RK0102`. trycmd
`dev_registry.trycmd` now expects exit 2 / `RK0102`. (Fixer round 1, finding #7.)

### D-73. Single-path `dev register` of an already-registered path is idempotent (DESIGN §3.2)

`dev register .` twice produced TWO registry entries for one path (the second under a
parent-prefixed derived name), so `dev status` showed the project loaded twice.
`add_recursive` already de-duped by canonical path; the single-path branch did not. Added
`Registry::find_by_path` (the same canonical-path comparison the other verbs use); the
`register` verb no-ops with `[✓] <name> is already registered` when the path is already
present, before computing a name. Tests: `registry::tests::find_by_path_*`,
`dev_registry.trycmd` idempotent-register block. (Fixer round 1, finding #6.)

### D-74. `dev reload --strict`/activate-failure suppresses the green success line on a non-zero exit (DESIGN §6.1, §7)

`dev reload --strict` printed `[✓] host reloaded (0 ms)` immediately ABOVE the `RK4006`
failure frame (and no reload had actually happened — the daemon short-circuits returning
`reloaded: []`). A stable `[✓]` success symbol on a failing (exit 1) command is misleading.
`reload.rs` now computes the will-fail condition (any activate failure, or a strict skip)
BEFORE printing, and suppresses the success line when the command will exit non-zero. The
`Skipped:`/`Failed:` stderr lines + the framed error remain. (Fixer round 1, findings #5/#8.)

### D-75. The transient working set resets when its owning watch session disconnects (DESIGN §3.3)

DESIGN §3.3 calls the working set transient — "for this session." But a scoped
`dev --only X` watch session set `SetWorkingSet` and never cleared it, so a later plain
`dev reload` from another terminal kept operating on the dead session's scope until daemon
restart. The daemon now tags the working set with the connection id that set it
(`ResponseSink::conn_id`, a monotonic per-connection id) and, when that connection closes
(`handle_conn` → `connection_closed`), resets the scope to the full enabled set and reloads.
A plain `dev reload` (a separate short-lived connection) is unaffected; only the OWNING
session's disconnect clears it. (Fixer round 1, finding #3.)

### D-76. Control-socket request lines are length-capped; mid-stream `StopStream` cancels a follow promptly (SPEC D §2)

Two control-channel robustness fixes share the rewritten `handle_conn`:
- (#12) `handle_conn` (and the foundation `ipc::handle_connection`) read each request line
  through a `take(MAX_REQUEST_LINE+1)` cap (64 KiB) instead of an unbounded
  `reader.lines()`, so a never-newline-terminated stream can no longer buffer unboundedly
  and OOM the daemon — an oversize line is rejected with `RK0308` and the connection closed.
- (#13) A dedicated per-connection READER thread owns the input half. A `Logs{follow}` /
  `Subscribe` stream blocks delivering log lines; the cancelling `StopStream` arrives as the
  NEXT request line, which the blocked dispatch loop could never read — so Ctrl-C hung the
  client and leaked a daemon thread. The reader thread intercepts `StopStream` and flips the
  in-flight stream's `stop` flag directly; `stream_logs` now also sends a closing `LogEnd` on
  a stop so the client's stream iterator terminates instead of hanging.

### D-77. Daemon process-safety: signal cleanup, identity-bound liveness, start serialization, orphan reaping (DESIGN §3.1, SPEC D §1)

Four daemon hazards fixed (fixer round 1, findings #9/#10/#11):
- (#9) `run_daemon` installs SIGTERM/SIGINT/SIGHUP handlers (a static `AtomicBool` the
  accept loop polls) so a catchable kill runs the same `stop_host()` + unlink cleanup
  instead of dying by default action and orphaning the host. (A `kill -9` is uncatchable by
  design; its orphan is reaped on the next start — see below.)
- (#9/#11) The pidfile records the host child's process-group id (`host_pgid`, written after
  every launch/reload/respawn). `reclaim_stale` killpg's that group when the recorded daemon
  pid is dead but the host group leader is still alive — so a `-9`'d daemon's orphaned host
  is reaped on the next `dev start`.
- (#10) Liveness is now IDENTITY-bound, not bare `kill(pid,0)`: `is_running` requires a
  socket `Ping` whose `Pong.pid` matches the pidfile pid (and a matching protocol version),
  defeating PID reuse where a recycled pid made `dev stop` killpg an innocent group.
  `dev stop`'s escalation killpg is gated on a fresh identity check.
- (#11) `dev start` holds an exclusive `O_CREAT|O_EXCL` start-lock (per-Live hash) across
  the is-running check + re-exec + wait-until-up, so concurrent starts observe the first
  daemon and reuse it instead of each spawning a daemon+host. The lock degrades to proceeding
  on timeout (never fails a legitimate start) and reclaims a stale lockfile (dead writer pid).
  The pidfile gains a `host_pgid: Option<i32>` field (`#[serde(default)]` for back-compat).

### D-78. Watch ignores the generated `manifest.json` as a build OUTPUT (DESIGN §3.3, finding #2)

The build step writes `manifest.json` at the project root from `rackabel.toml`; it matches
the `**/*.json` source glob and lives OUTSIDE `dist/`, so the watcher counted each chain
run's own manifest write as a new input — a self-write feedback loop that fired the whole
build→deploy→reload TWICE for one save (one save → two whole-host reloads). `is_source_input`
now excludes a file named `manifest.json` (it is generated, not a source — `rackabel.toml` is
the source of truth). A genuinely-edited source JSON under `src/` still triggers. Regression:
`watch::tests::classify_ignores_generated_manifest_write`.

### D-79. Daemon-spawning integration tests tear down via an RAII `DaemonGuard` (INTEGRATOR, finding #14)

Teardown was a trailing `force_stop()` as the LAST statement of each test body, so any
assertion panic before it leaked the sandboxed `__daemon` (a setsid session leader) and its
`rk-fakehost` child indefinitely. Replaced with `common::DaemonGuard` — bound by value at
test start (`let _guard = DaemonGuard::new(home, work, live)`), its `Drop` runs `dev stop`
(killpg's the group) + a liveness-gated `kill -9` of the pidfile pid. Drop runs on unwind,
so a panic still reaps the daemon. The liveness gate also removes the
`kill: <pid>: No such process` transcript noise (minor #6).

---

## 0.4 extensibility (foundation)

### D-80. New RK codes for the §5 extensibility surface (DESIGN §5.4/§5.5/§5.6/§7)

Seven new `ErrorCode`s land for the extensibility milestone, classed by the §7 taxonomy
(the `RKxxxx` thousands-grouping hint is preserved). Environment (exit 3): `RK0401
PluginNotFound`, `RK0402 TemplateNotFound`, `RK0403 TemplateFetchDeclined`, `RK0404
NoNetwork` — a new `RK04xx` block kept distinct from the `RK03xx` host/Live environment
codes so the cause is legible. Usage (exit 2): `RK0103 PluginShadowedByBuiltin` — the
shadow is informational (§5.6), its remedy is the `plugin run` escape hatch, so it sits in
the usage class beside `RK0101`. Validation (exit 4): `RK4007 PinMismatch` and `RK4008
UpdateConflicts` — a pin mismatch is a deterministic CI gate (§5.4) and an update conflict
needs a human decision (§5.5), both validation per §7. Every code has a `short_title` +
long-form `explain` entry; the `code_roundtrips`/`every_code_has_prose`/
`extensibility_codes_are_classed_per_design` tests pin them.

### D-81. `RACKABEL_PROJECT_DIR`/`RACKABEL_MANIFEST` are UNSET, not empty, outside a project (DESIGN §5.2)

The env-contract builder (`plugin::env_contract::build`) returns a map containing ONLY the
keys rackabel sets; the caller overlays it on the inherited environment. The two
project-only vars are simply ABSENT from the map when rackabel runs outside a project
(`Project::discover` returns `Err`, mapped to `None`), so a plugin presence-tests rather
than comparing an empty string — the §5.2 "commit unset, not empty" rule. The four
always-set vars (`RACKABEL`, `RACKABEL_VERSION`, `RACKABEL_PLUGIN_API`,
`RACKABEL_REGISTRY`) are always present; `RACKABEL` is recomputed from `current_exe` every
call so a stale inherited value is overwritten (cargo `CARGO`-points-at-wrong-binary bug).
Pinned by `env_contract::tests` and the `tests/integration/plugin.rs` exec tests.

### D-82. `RESERVED_NAMESPACE` is a single const that INCLUDES the unshipped `publish`/`login` (DESIGN §5.6/§8)

The reserved set lives in one place (`cli::RESERVED_NAMESPACE`) that both clap's
built-in-precedence (by construction — `allow_external_subcommands` only falls through for
tokens no built-in claims) and the plugin resolver (`is_reserved`) consult. It deliberately
reserves `publish` and `login` NOW, before the release that ships them (§8), so the §5.6
upgrade-time collision detector predates the collision: a `rackabel-publish` plugin is
already shadowed (and reachable via `plugin run`). `reserved_namespace_is_pinned` +
`every_builtin_is_reserved` pin the list and assert no built-in escapes it.

### D-83. PATH-subcommand resolution + env contract are FOUNDATION-owned and live (DESIGN §5.1/§5.2/§5.6)

Although §5 install/search/enable/disable bodies are agent-owned stubs, the load-bearing
RUNTIME surface — `rackabel <foo>` resolve (managed-bin first, then `$PATH`; built-ins
always win; the both-locations warning), the full §5.2 env contract, arg passthrough, and
tier-2 exit-code passthrough — is implemented and tested end to end in the foundation
(`plugin::resolve`, `commands::plugin::external`/`run`/`which`/`list`,
`tests/integration/plugin.rs`). This is intentional: the three parallel feature agents all
depend on this surface, so it is frozen + working rather than stubbed. A plugin's non-zero
exit is propagated via `std::process::exit(code)` (NOT wrapped in an `RkError`) because the
RkError taxonomy is for rackabel's OWN failures, not a plugin's.

### D-84. plugins.lock RECORDS inert 0.5 hook metadata in 0.4 (DESIGN §5.3/§5.4, milestone note)

Per the milestone note ("the install/list model should already RECORD what 0.5 needs"), the
`plugins.lock` entry carries `has_plugin_manifest: bool` and an inert `hooks: Vec<String>`,
plus `enabled: bool` (default false). No `rackabel-plugin.toml` is parsed for execution and
NO hook runs in 0.4 — the fields are pure metadata so the 0.5 hook surface (enable/disable,
`plugin migrate`) has a place to read from without a lockfile format change. `enabled`
defaults false because enabling is the consent gate for hooks (§5.7).

### D-85. Network seams are env vars; no HTTP client lands in the foundation (DESIGN §5.4)

Two test/override seams are env vars resolved in one place: `RACKABEL_TEMPLATE_GIT_BASE`
(rewrites a `gh:`/`@scope` clone URL to a local `file://` base so template tests use
fixture repos, never the network) and `RACKABEL_GITHUB_API` (the `plugin search` API base).
The git wrapper (`plugin::git`) shells the system `git` binary with fixed arg arrays
(injection-safe for third-party refs) and is tested against LOCAL repos in tempdirs. The
foundation deliberately adds NO HTTP client dependency: `plugin search`/`install`'s real
network fetch is the agent's job and is exercised manually later; the foundation freezes the
seams + the clean `RK0404 NoNetwork` frame. The only new crate is `sha2` (pure-Rust, no
openssl) for the §5.4 sha256 pin.

### D-86. `--template`/`--update` route to a frozen boundary, never a silent default (DESIGN §5.5)

`new --template <ref>` is classified through the frozen `TemplateSource` and routed to a
clear "not implemented yet" (`RK0402`) for a remote/local template, or a usage error
(`RK0101`) for a malformed ref — it never silently falls back to the built-in default
(replaces the old 0.2 "remote templates arrive in 0.4" usage-error stub, which is now
inaccurate). `new --update` short-circuits before kind resolution, reads the frozen
`.rackabel-template` lockfile, and returns "nothing to update" (`RK0402`) when absent or the
not-implemented merge frame when present. The TEMPLATES agent fills the fetch/render/3-way
merge; the foundation freezes the source classification + the lockfile models.

## 0.4 dispatch (PATH subcommands + one-time warning + did-you-mean)

### D-87. The both-locations warning is one-time per name, persisted under RACKABEL_HOME (DESIGN §5.1)

DESIGN §5.1 says rackabel emits a **one-time** warning when an external `rackabel-<foo>` is
resolvable from both `~/.rackabel/plugins/bin` and `$PATH`. The foundation surfaced the
warning but fired it on every invocation. This milestone makes it genuinely one-time: the
first both-locations hit for a given name prints the warning and records the name in
`~/.rackabel/plugins/warned-both-locations` (one name per line); later runs of that name stay
quiet. The state file is **advisory** — a missing/unreadable/unwritable file just means the
warning fires again, so it never turns a plugin invocation into an error. Under `--json`/the
`quiet` dev-watch seam the warning is suppressed AND not recorded, so a later interactive run
still gets the nudge. `plugin which <name>` remains the always-shown authoritative surface.

### D-88. The both-locations warning prints to STDERR, not stdout (DESIGN §5.1)

The §5.1 warning (and the shared `ui::frame::ewarn` helper added for it) writes to **stderr**,
not stdout. The bare `rackabel <foo>` dispatch and the `plugin run` escape hatch both forward
the plugin's own stdout to the user (a plugin may emit machine output a caller pipes); a
warning on stdout would corrupt that stream. stderr keeps the plugin's stdout pristine while
still surfacing the collision. (`ui::frame::emit`, the existing status helper, is stdout-only;
`ewarn` is a small additive sibling — flagged for the integrator as a shared `ui` change.)

### D-89. Unknown-token frame carries a did-you-mean help LINE, never an auto-correct (DESIGN §5.1)

An unknown top-level token with no matching `rackabel-<foo>` returns `RK0401` as before, but
its `help:` block now leads with a did-you-mean line listing the closest candidates (built-ins
+ installed plugins within Levenshtein distance ≤ min(len/2, 2)), e.g. `did you mean:
\`build\`?`. It is purely a help line LISTING candidates — rackabel never silently runs a
different command than the user typed (no auto-correct), honoring the §5.7 never-silent posture.
A genuinely novel token (e.g. `frobnicate`) gets no suggestion and the original frame verbatim.

## 0.4 plugin-management (install / list / enable / disable / search + plugins.lock)

### D-90. A declined PLUGIN install reuses RK0403 (environment, exit 3), not a usage RK01xx (DESIGN §5.4/§5.7, §7)

§7 says a prompt refused under `--no-input` exits `2` for a usage/missing-answer prompt and
`3` for an environment prompt. The remote-install consent prompt ("fetch + run unreviewed
third-party code?") is treated as an **environment** decision and reuses `RK0403
TemplateFetchDeclined` (exit 3) — the SAME code the foundation froze for a declined remote
TEMPLATE fetch, whose `explain` prose already names `plugin install` explicitly. Rationale:
(a) a declined install is the same *class* of event as a declined template fetch (consent to
run remote code) and should `explain` identically; (b) it is not a malformed invocation (the
command is well-formed — the user simply did not consent), so the usage class (2) is the
wrong remedy. `--yes` is the scripted consent; `--no-input` (or a non-TTY with no `--yes`,
or `--json` with no `--yes`) refuses with `RK0403` and fetches NOTHING. A sideload
(`<path>`/`<tarball>`) is local code the user already has and is NOT gated behind this prompt
— it still announces what it will do, and the bytes are still sha256-pinned.

### D-91. A plain tier-2 plugin installs ENABLED; a hook plugin installs DISABLED (DESIGN §5.4 vs §5.7)

The foundation's `plugins.lock` `enabled` flag defaults `false` (documented as the 0.5 hook
consent gate). The 0.4 milestone task additionally makes the flag **gate dispatch of the
managed copy** (a disabled managed plugin is skipped in the bin search with a one-line note).
These two intents are reconciled at install time: a **plain** tier-2 plugin (NO
`rackabel-plugin.toml`) installs `enabled = true` so it is immediately usable (the musician
happy path — installing it then having `rackabel <foo>` say "disabled" would be baffling); a
**hook** plugin (carries a manifest) installs `enabled = false` so hooks never run under a
default-on flag — enabling is the explicit 0.5 consent (§5.7). A reinstall preserves the
prior flag unless the code changed; if it changed, the §5.7 rule applies (a hook plugin is
forced back to disabled — new code never runs under old consent; a plain plugin stays usable).

### D-92. Dispatch gating is a one-line hook in the foundation-owned bare-external dispatch (DESIGN §5.4)

"A disabled managed plugin is skipped in the bin search" requires a check at the dispatch
site. The pure resolver (`plugin::resolve`, foundation-owned) intentionally does NOT read the
lockfile, so the enabled-state gate lives in `plugin::store::is_managed_disabled` and is
consulted by `commands::plugin::external::run` (the bare `rackabel <foo>` path): a disabled
managed copy is skipped with a note, falling back to a `$PATH` `rackabel-<foo>` if one exists,
else `RK0401` ("installed but disabled"). `plugin run` (the §5.6 escape hatch) deliberately
IGNORES the **enabled** flag and always reaches the plugin — but it does NOT bypass the pin:
it calls `store::verify_managed` for the SAME §5.7 tamper guarantee the bare dispatch
enforces (see below), so a tampered managed store file fails `RK4007` (exit 4) on the escape
hatch too, before any code runs. (The escape hatch is for running a *disabled* or
*built-in-shadowed* plugin, never for running *modified* code past its pin.) `plugin which`
is left to the foundation as-is (it reports resolution, not enabled-state). The bare dispatch
site ALSO calls `store::verify_managed` (the §5.7 tamper check) before running a managed
copy: a modified store file fails its lockfile sha256 pin with `RK4007` (exit 4) before any
code runs; an unmanaged `$PATH` plugin has no pin and is run as-is. **Integrator note:**
`external.rs` is the only foundation-owned command file this agent edits (two added calls
into this agent's `store.rs`: `is_managed_disabled` and `verify_managed`); both gate
functions live in `store.rs`. `plugin run` (PLUGIN-MGMT-owned) calls `verify_managed` too.

### D-93. New seam `RACKABEL_GITHUB_DL` + HTTP/tar/gzip deps for the install/search bodies (DESIGN §5.4, D-85)

D-85 deferred the real network fetch and added no HTTP client in the foundation. This
milestone implements it: a small blocking HTTP client (`ureq`, rustls — pure-Rust, no
openssl) issues the GitHub `search/repositories` query (`plugin search`) and the
`releases/latest` + release-asset download (`plugin install OWNER/REPO`), and pure-Rust
`flate2`+`tar` unpack a sideloaded `.tgz`. A SECOND seam joins the foundation's
`RACKABEL_GITHUB_API` (which lists asset URLs): `RACKABEL_GITHUB_DL` rewrites the
release-asset *download* host so the asset fetch hits a local stub server in tests (the API
response advertises a github.com URL; the seam swaps only the host). Tests stub both seams
with a throwaway in-process `TcpListener`, so the suite NEVER touches the network; the real
fetch is exercised manually. Every network failure (transport, 403/429 rate limit, 5xx) maps
to the clean `RK0404 NoNetwork` frame; a 404 on `OWNER/REPO` is `RK0401 PluginNotFound`.

All three HTTP calls (the search query, `releases/latest`, and the asset download) go through
a shared `ureq::Agent` (`store::http_agent`) configured with explicit connect (10s) and
read/write (60s) timeouts. ureq 2.x applies NO timeout unless one is set, so a stalled or
half-open GitHub connection would otherwise hang `plugin install`/`search` forever; the
timeouts surface a stall as a transport error → the existing `RK0404` frame. (Proxy env vars
are still not honored — ureq does not read `HTTP(S)_PROXY` automatically; explicit proxy
support is a later improvement.)

### D-94. `OWNER/REPO` clone-with-no-build is a clear frame, not a silent success (DESIGN §5.4)

For `plugin install OWNER/REPO`, 0.4 implements the **release-asset** path (preferred:
`rackabel-<name>-<os>-<arch>`, sha256-pinned) and a **clone** path that pins the commit and
installs a `rackabel-<name>` already committed at the repo root. 0.4 does NOT invent a build
toolchain: a clone with no prebuilt executable and no build rackabel recognizes returns a
clear `RK0401` frame (naming the cloned commit) pointing the user at publishing a release
asset or sideloading a locally-built executable — never a silent "installed nothing". The
asset path is the intended production route; the full auto-build of an arbitrary repo is left
for a later milestone.

A clone-resolved entry records BOTH a `commit` (provenance + what `enforce_pin` compares on
reinstall, since its SourceKind is `Gh`) AND a `sha256` of the resolved executable. The
sha256 is the byte pin the §5.7 runtime tamper check (`verify_entry`) needs: without it, a
commit-only entry made `verify_entry` a no-op, so a clone-built plugin's store bytes could be
swapped post-install and run undetected on BOTH dispatch paths. Recording the exe's sha256
makes EVERY managed plugin byte-verifiable at dispatch regardless of source kind. (A legacy
entry written before this change carries no sha256 and cannot be byte-verified until it is
reinstalled; `verify_entry` passes such an entry rather than failing closed on absent data.)

## 0.4 templates (`new --template` render + `new --update` 3-way merge)

### D-95. Tier-1 templates land (`new --template` render + `new --update` 3-way merge) — supersedes D-86's boundary (DESIGN §5.5/§5.7)

The TEMPLATES tier-1 surface is now implemented (`src/templates/`: `render`, `update`,
`placeholder`, `exclude`), replacing the foundation's "not implemented yet" boundary
(D-86): `new --template gh:owner/repo[@ref]` and `new --template <local-path>` resolve a
`rackabel-template.toml`, run its `[prompts]` as the wizard, copy the tree with `{{ key }}`
substitution, and persist `.rackabel-template` (repo + ref + commit + answers). `new
--update` re-renders old@oldcommit + new@newcommit and 3-way-merges against the user tree.
D-86's invariants survive verbatim: a malformed ref is `RK0101`; a remote ref under
`--no-input` without `--yes` REFUSES (`RK0403`) — never a silent default. The frozen
classification (`TemplateSource`), the lockfile models (`plugin::template`), and the network
seams (`RACKABEL_TEMPLATE_GIT_BASE`) the foundation froze are reused unchanged.

The explicitly-typed positional project name SEEDS the template's `name` prompt: `new myproj
--template …` renders `myproj` (folder AND content) rather than the template's default
`name`, matching the §6.2 model where `new clip-renamer` yields a clip-renamer-named project.
Mechanically, `new_from_template` passes a `{ name: <typed> }` seed into `render_into`, which
threads it to `run_prompts`; per the §5.5 "re-prompt only for NEW prompts" rule a seeded
prompt is used verbatim and not re-asked (so the typed name governs even under
`--no-input`/`--yes`). A template that declares no `name` prompt simply ignores the seed;
other prompts (e.g. `author`) still resolve from their own defaults/answers. The persisted
`.rackabel-template` records the seeded name so `new --update` re-uses it.

### D-96. The placeholder syntax is a deliberately minimal `{{ key }}` (DESIGN §5.5 "declarative data, never dependent on rackabel internals")

The substitution language is ONE construct — `{{ key }}`, with optional inner ASCII
whitespace — and nothing else: no conditionals, loops, partials, or filters. Rationale: §5.5
demands templates be "declarative data" that "don't bit-rot when rackabel changes" (the
Yeoman-decline lesson), so the syntax is intentionally too small to depend on internals. An
UNKNOWN key is left VERBATIM (not substituted-to-empty) so a typo is visible in the output;
substitution is a single left-to-right pass (a replacement value is never re-scanned, so an
answer that contains `{{…}}` can't trigger a second substitution — no answer-driven
injection). The syntax is documented in `docs/TEMPLATES.md` (the template-author reference)
and in the `placeholder` module docs.

### D-97. `@scope/name` templates are accepted + classified but not resolved in 0.4 (out of scope, not half-done) (DESIGN §5.5)

`TemplateSource` parses `@scope/name`, but npm-registry resolution is OUT OF SCOPE for 0.4.
Rather than half-implement it, `new --template @scope/name` emits a clear not-yet-supported
frame (`RK0402`) pointing the user at `gh:owner/repo` or a local checkout. gh: and local
paths are the two fully-supported resolution kinds this milestone.

### D-98. `[merge].exclude` is UNIONED with an always-excluded binary/tarball set (DESIGN §5.5)

§5.5 says vendored SDK/CLI tarballs and other binary/generated files are excluded from the
3-way text merge "per a declared `[merge].exclude` glob". To be safe-by-default, the
author's declared globs are UNIONED with a built-in `ALWAYS_EXCLUDED` set (`**/*.tgz`,
`**/*.tar.gz`, `**/*.zip`, `**/*.node`, `**/*.wasm`, common images, `**/vendor/**`,
`**/node_modules/**`, and the npm/pnpm/yarn lockfiles), so a template that forgets to list
its vendored toolkit still never gets its tarballs mangled or marker-corrupted. Excluded
files are copied VERBATIM on the initial render (no substitution) and, on update, are
OVERWRITTEN from the new render when changed (never text-merged). A no-slash declared
pattern (e.g. `*.tgz`) is matched at any depth, matching author intuition. Additionally, a
non-UTF-8 file the author did NOT list is copied verbatim rather than corrupted.

### D-99. `new --dry-run` (with `--update`) + `tempfile` promoted to a normal dependency; `merge_file`/`clone_full`/`checkout` added to the git wrapper (DESIGN §5.5)

Three shared-surface additions the tier-1 work required (flagged for the integrator):
(1) `NewArgs` gains `--dry-run` (only meaningful with `--update`: prints the merge plan —
which files apply/conflict/overwrite/skip — and mutates nothing). (2) `tempfile` moves from
a dev-dependency to a normal dependency, because the remote-clone holder and the old/new
render scratch trees `--update` merges are runtime temp dirs, not test-only. (3) The
FOUNDATION git wrapper (`plugin::git`) gains `clone_full` (full history, so two commits can
be checked out from one clone), `checkout`, and `merge_file` (an arg-array-safe wrapper over
`git merge-file` that maps the conflict exit to `false`, not an error). The 3-way merge uses
`git merge-file` per text file (per the task's "git merge-file per text file is fine");
`--update` clones a LOCAL template repo into a tempdir rather than checking out commits in
the user's own template work tree, so the source repo's HEAD is never mutated.

### D-100. `plugin migrate` ships the SURFACE only; the codemod machinery is deferred (DESIGN §2/§5.3, milestone 0.5)

The 0.5 foundation lands the hook-contract types (`hooks::*`), the `hook_api` parse in
`rackabel-plugin.toml` (`PluginManifest::declared_hook_api`, defaulting to the v1 floor when
unset), and the `HOOK_API = 1` const — but NOT a `plugin migrate` codemod. There are no
hook-contract migrations yet (the supported `hook_api` is 1), so there is nothing to codemod.
The DESIGN `plugin migrate` synopsis is honored as a SURFACE contract recorded here: detect a
plugin's declared `hook_api` vs the supported `HOOK_API`; `== 1` => "nothing to migrate"
(success); `> HOOK_API` => a clear UNSUPPORTED frame (`RK0104 MigrateUnsupported`, usage/exit 2)
rather than a faked codemod. At hook-RUN time the same higher-`hook_api` plugin is refused with
`RK0405 HookApiUnsupported` (environment/exit 3 — "your rackabel is too old"). The codemod
engine is built one migration at a time when the FIRST `hook_api` bump ships (the ESLint-v9
lesson). We deliberately do not fake a codemod.

### D-101. Hook engine `run_hook` signature takes a `ResolvedHook` (source+command+timeout), not separate `(kind, source)` args (DESIGN §5.3, milestone 0.5 foundation)

The milestone brief names the engine signature `run_hook(kind, payload, source: Project|Plugin,
ctx) -> HookOutcome`. The frozen foundation signature is
`run_hook(hook: &ResolvedHook, payload: &HookPayload, ctx: &Ctx) -> CmdResult<HookOutcome>`,
which CARRIES the same information in a tighter contract: `ResolvedHook` bundles the source
(`HookSource::Project { project_root } | Plugin { name, store_dir }` — exactly the
Project|Plugin split), the hook KIND, the resolved command path, AND the per-hook timeout that
discovery already computed (§5.3) — so the engine never re-derives the timeout or re-resolves
the command, and `kind`/`payload.kind()` are debug-asserted to agree. `HookPayload` is the
typed envelope over the five §5.3 stdin structs. The return is wrapped in `CmdResult` so an
engine-level failure (a command path that does not exist) is a framed `RkError`, distinct from
an in-contract `HookOutcome` (informational-skip / veto / doctor-row / template-choice). The
BODY is a stub returning `RK1309` until the 0.5 feature agent lands the subprocess/timeout
machinery; the signature is frozen now so callers compile against it.

### D-102. The hook engine BODY lands; subprocess + timeout/SIGKILL machinery is Unix-only this milestone (DESIGN §5.3/§5.7, milestone 0.5)

D-101 froze the signature with an `RK1309` stub. This milestone fills the body. The §5.3
execution contract is implemented EXACTLY: the §5.2 env map (`env_contract::build`) PLUS
`RACKABEL_HOOK_API=HOOK_API` on the child; exactly one JSON payload written to stdin then
stdin CLOSED (EOF framing — a read-to-EOF hook terminates naturally, a blocking one hits the
timeout); a wall-clock timeout (`ResolvedHook::timeout_ms`, default 30s, `[hooks.timeouts]`
override in ms) enforced by polling `try_wait` against a deadline, then SIGTERM to the child's
PROCESS GROUP and SIGKILL after a 5s grace, then reap — a timeout is treated EXACTLY like a
nonzero exit per kind. The child is placed in its own process group via a `pre_exec`
`setpgid(0,0)` (mirroring the dev host) so `killpg` reaps a hung hook's whole tree with NO
orphan (asserted by a `ps`-based test). **Deviation:** that signalling is `#[cfg(unix)]`. On a
non-Unix build `exec` returns a framed `RK1309` ("hooks not supported on this platform yet"),
matching the dev host's Unix-only posture (§9.3) rather than running a hook WITHOUT the
bounded-DoS timeout guarantee. Cross-platform hook execution is deferred, not faked.

### D-103. A `pre_deploy` hook whose command cannot be SPAWNED aborts the deploy (it is a refusal, not a skip) (DESIGN §5.3/§5.7)

§5.3 says a `pre_deploy` nonzero/timeout aborts the deploy. It is silent on a `pre_deploy`
whose command path does not exist / is not executable (a spawn failure, distinct from a hook
that ran and exited nonzero). **Decision:** a spawn failure of a `pre_deploy` hook ALSO aborts
the deploy (framed `RK1310`), because a deploy GATE the user enabled that cannot even run is a
refusal — silently skipping it would deploy past a notarize/lint check the user explicitly
asked for, the exact "enabling is the consent" violation §5.7 guards against. The informational
hooks (`post_build`/`on_reload`) take the opposite branch by design: a spawn failure there is
logged + skipped (never fatal), because those phases can never abort. Both branches live in
`hooks::lifecycle`; the engine itself returns the spawn failure as a framed `RkError` and lets
each phase apply its policy.

### D-104. on_reload fires per-reloaded-extension with the reload's OVERALL ok/reload_ms (DESIGN §5.3)

§5.3's `on_reload` payload is `{project_dir, manifest_toml, name, reload_ms, ok}`. The dev watch
chain reloads a SET of affected extensions in one `reload` IPC whose `ReloadResult` carries an
overall `ok` + `reload_ms` (the host kill+respawn is whole-host, SPEC H §2 — there is no
per-extension reload timing). **Decision:** `on_reload` fires once per reloaded extension, each
with its own `name`/`project_dir`/`manifest_toml` but sharing that one reload's `ok`/`reload_ms`.
This is faithful to the host model (a single respawn served them all) and keeps the per-extension
`name` a hook can key on. A reload that errored at the IPC layer (never completed) fires NO
`on_reload`. `post_build` is NOT fired inside the watch chain when build-if-stale SKIPS esbuild
(no build ran ⇒ no post_build) — it fires only on an actual build, with `bundle_path` present.
