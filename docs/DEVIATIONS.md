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

## 0.2 new (`rackabel new`)

### D-11. `--author` / `--license` are wizard prompts, not CLI flags (DESIGN §2 / SPEC C frozen CLI)

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

### D-12. The default template is rackabel's fork; the official `create-extension` reuse path is wired but dormant (DESIGN §4.7)

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

### D-13. Generated `package.json` scripts drive `rackabel`, not `tsx build.ts` / `extensions-cli` (DESIGN §4.7 / SPEC A §3.5)

The official template's `package.json` scripts are `tsc --noEmit && tsx build.ts …`
plus `extensions-cli run/package`, and it emits a `build.ts`. Per §4.7 ("replace
`build.ts` with the rackabel pipeline") the rackabel-form project omits `build.ts` and
maps the scripts to `rackabel build` / `rackabel deploy` / `rackabel pack` /
`rackabel dev`. The SDK/CLI are still vendored and wired as `file:` deps (SPEC A §3.4),
and `esbuild` is still pinned at `0.28.0` so a plain `npm install` resolves the same
toolchain rackabel drives. `manifest.json` is **not** hand-written into the project —
`rackabel build` generates it from `rackabel.toml` (§4.5), and it is gitignored.

### D-14. Toolkit "vendoring into the project" = copy SDK+CLI into `<project>/vendor`; the gated tarballs are never committed to rackabel (SPEC A / DESIGN §4)

SPEC A says "vendor the tarballs into the project wired via `file:` deps." Because the
SDK/CLI tarballs are beta-gated (not redistributable, not on public npm), rackabel does
**not** commit them into its own repo. Instead `new` discovers the user's downloaded
toolkit (`toolkit::discover`) and copies the discovered SDK+CLI into the new project's
`vendor/` (`toolkit::vendor_into`), then writes `file:./vendor/<basename>` deps that
match the vendored files. Tests fabricate tiny stand-in `.tgz` fixtures
(`tests/fixtures/toolkit/`) — they never depend on the real gated tarballs.

### D-15. Wizard answers are remembered under `$RACKABEL_HOME`, keyed by project name (DESIGN §6.2)

The §6.2 SDK-not-found transcript promises "Your answers above are remembered." rackabel
persists the wizard answers to `$RACKABEL_HOME/new-answers/<sanitized-name>.toml` on the
SDK-not-found stop, seeds the wizard's Enter-to-accept defaults from that file on the
re-run for the same name, and clears it after a successful scaffold. This is the
mechanism behind the promise; it is best-effort (a write/read failure is swallowed so a
failure to remember never becomes a second error). The `--update` 3-way-merge answers
lockfile (`.rackabel-template`, §5.5) is a *separate*, 0.4 concern and is not
implemented here.

### D-16. Remote `--template` refs (`gh:`/`@scope`) are a framed "coming in 0.4" usage error (DESIGN §5.5)

Remote template fetch + render is the 0.4 templates milestone. In 0.2, `new` accepts the
`--template` flag but, for a remote ref, returns a three-part usage error (exit 2)
pointing at 0.4 rather than silently falling back to the default. A **local** `--template
<path>` is accepted without error but is not yet applied (the built-in default template
is used); applying a local template directory also lands with tier-1 templates in 0.4.
This honors the brief ("accept the flag but print a framed not-yet-supported error for
remote refs") without painting the 0.4 work into a corner.

### D-17. SDK-not-found / no-node trycmd vs. integration split (SPEC C §5)

The §6.2 SDK-not-found and `--no-input` cases are deterministic regardless of the host
machine (they fail before any Live/node detection), so they are **trycmd** transcript
acceptance tests (`tests/cli/new_no_toolkit.trycmd`, `new_no_input.trycmd`). The happy
scaffold and the no-node friendly-skip depend on Live/PATH-node *absence*, which the
shared trycmd harness cannot isolate on a developer's real machine (Live and a PATH node
may be present). Those two — plus answer-persistence, git-init/`--no-git`/`--minimal`,
and the device dispatch — are **assert_cmd integration tests** (`tests/integration/new.rs`)
that pin `ABLETON_APP` to a no-host fake `.app` and strip `PATH` to a node-free dir, so
Live/node detection is deterministic. Same coverage, deterministic across machines.

### D-18. The Extensions-beta URL is centralized in one module with an env override (DESIGN §6.2)

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

- **`native_dep::fix` full pnpm automation** (DESIGN §3.7 / SPEC C §3.8). The
  foundation ships `fix` as an `RK0304` stub with a plain-English help line. If full
  `pnpm approve-builds`+`rebuild` automation slips past 0.2, the deploy-owner records
  it here.
- **pack dual-format `.ablx` vs `.zip`** (DESIGN §4.7 / SPEC B §4). The pack-owner
  records the reconciliation between the official `<name>-<version>.ablx` and the
  Arclight `<slug>-v<version>[-os-arch].zip` layouts, and the dropped/ generalized
  lidal `lidal.openEditor` sentinel.
- **Developer Mode detection is behavioral/unverified** (DESIGN §9.2 / SPEC B §6).
  Dev Mode is not statically readable; the doctor-owner records the inferred
  (running-Live + host-child presence) approach when `doctor` lands.
- **`extra_dist_files` copied on deploy** (SPEC B §3). The shared `deploy-extension.js`
  helper does not copy extra dist files; rackabel unifies this with pack. The
  deploy-owner records the final behavior.
