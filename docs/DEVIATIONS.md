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
  RESOLVED by the doctor branch — see D-11 above.
- **`extra_dist_files` copied on deploy** (SPEC B §3). The shared `deploy-extension.js`
  helper does not copy extra dist files; rackabel unifies this with pack. The
  deploy-owner records the final behavior.
