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

## 0.2 deploy

### D-11. `native_dep::fix` IS implemented (no slip) — but depends on pnpm on PATH (DESIGN §3.7 / SPEC C §3.8)

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

### D-12. `extra_dist_files` ARE copied on deploy (unifying deploy with pack) (SPEC B §3)

Resolved the deferred item: the Arclight `deploy-extension.js` helper copies only
`manifest.json` + `dist/extension.js` (lidal overrides this for `editor-client.js`);
rackabel's deploy copies `[extension.build].extra_dist_files` into `<dest>/dist/` too,
matching pack and lidal's intent so the deployed and packed dist trees are identical. A
declared extra dist file that is missing on disk is a warn-and-skip (parity with
`pack-extension.js`), not a hard failure.

### D-13. build-if-stale uses an mtime check, not the recorded build hash (DESIGN §2 deploy)

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

### D-14. `--undo` safety contract: refuse a folder without a `manifest.json` (DESIGN §2 deploy)

`--undo` removes `<UserLibrary>/Extensions/<slug>`. To avoid ever `rm -rf`-ing an
unrelated user folder that happens to share the slug, deploy refuses (framed error,
exit 1) when the target exists but does not contain a `manifest.json` — the always-
written member of a rackabel deploy. A not-deployed target is a clean no-op success
(the desired end state already holds), not an error. The spec describes undo as "the
discoverable cleanup path"; this adds the safety gate the spec implies but does not
spell out.

### D-15. `deploy --release` calls `validate::run` directly (DESIGN §2 deploy)

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

## Deferred (to be recorded by the owning command branch when it lands)

These are flagged in the specs as likely deviations but belong to a command body the
foundation only stubs; the owning branch records the final decision here.

- **`native_dep::fix` full pnpm automation** (DESIGN §3.7 / SPEC C §3.8). Resolved by
  the deploy branch — see D-11 above.
- **pack dual-format `.ablx` vs `.zip`** (DESIGN §4.7 / SPEC B §4). The pack-owner
  records the reconciliation between the official `<name>-<version>.ablx` and the
  Arclight `<slug>-v<version>[-os-arch].zip` layouts, and the dropped/ generalized
  lidal `lidal.openEditor` sentinel.
- **Developer Mode detection is behavioral/unverified** (DESIGN §9.2 / SPEC B §6).
  Dev Mode is not statically readable; the doctor-owner records the inferred
  (running-Live + host-child presence) approach when `doctor` lands.
- **`extra_dist_files` copied on deploy** (SPEC B §3). Resolved by the deploy branch —
  see D-12 above.
