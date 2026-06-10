# rackabel v2 — Design

> Status: design proposal for the v2 scope. Supersedes the M4L-only scaffold in
> `src/` (today: `[device]`-only `rackabel.toml`, M4L `new/build/install/watch/doctor`).
> v2 adds Ableton Live Extensions (official SDK v1.0), a managed dev Extension Host
> with baked-in logging, a persistent watched-extension registry, and third-party
> extensibility of rackabel itself.

This document is decisive by design. Where the SDK or the ecosystem research left a
genuine fork, the choice is stated and justified; the residual unknowns are collected
in §9. Every non-obvious decision cites either an SDK constraint (from the
investigation of the local SDK repos and Ableton's public docs) or a named ecosystem
lesson.

**Versioning note.** rackabel ships on **0.x milestones** (§8); "v2" in the filename means
the *second design generation* (Extensions added to the M4L scaffold), not a 2.x product
version. Every version-contract check (`RACKABEL_VERSION` §5.2, hook `requires` §5.3) uses
0.x; the plugin/hook contracts carry their **own** small integers, independent of the
product version (§5.2/§5.3).

**Ground-truth sources** (referenced throughout): (a) **the launcher** = Arclight's private
`dev-launch.sh` + `deploy-extension.js`/`pack-extension.js` (a multi-extension, User-Library,
SIGHUP-reload launcher — what rackabel *replaces*; a project-local convention, **not** an
SDK mandate); (b) **the official CLI** `@ableton-extensions/cli` (beta tarball) shipping
`run` (single-shot, source-dir) and `package` (produces `.ablx`); (c) the official
`@ableton-extensions/create-extension` scaffolder (beta tarball) that vendors the SDK+CLI
and emits a starter project.

---

## 0. Installation & prerequisites

Persona A "can use a terminal" but "has never used npm" and has never installed a Rust
binary. The §1 promise ("before you have read any documentation") only holds if getting
rackabel onto the machine is itself documentation-free. So:

**Getting rackabel (macOS, the Persona-A path).** rackabel ships as a **signed, notarized
`.pkg`** (and `brew install rackabel` once the tap is up); double-click or `brew install`
and it lands on `PATH`. We **never** tell a musician to `cargo install` (assumes Rust) or
`npm i -g` (assumes npm). The `.pkg` is the only thing a first-timer installs by hand.

**Node/pnpm: the user installs nothing.** rackabel uses **Live's bundled node** for
everything user-facing — the dev host *must* run on it for native-ABI match (§3.1), and
rackabel **prefers** it for `build`. Unlike the dev host, though, `build` does **not require**
Live's node: if Live is absent, `build`/`new` fall back to a **PATH node** (the same
`command -v node` fallback the dev daemon uses, §3.1) so a musician evaluating rackabel before
installing Live can still scaffold. If **neither** Live nor a PATH node exists, `new` **skips
the auto-build** rather than dead-ending (see §6.2: it creates the project, prints a friendly
"build/run once Live is present" note, and points at `rackabel doctor`). So `build` has a
PATH-node fallback, and the `new` auto-build is **gated on a usable node being present at
all** — never a raw "node not found". The **runtime** floor doctor enforces against Live's
bundled node is the SDK/CLI `engines` value (**>=22.11.0**, §4.2 `node_runtime`) — not "install
Node"; below it, doctor says upgrade Live. The stricter **>=24.14.1** that `create-extension` bakes
into a *generated* project (§4.2 `node_build`) is **build-time-only** and **not** gated on the
dev loop, so Persona A is never penalized for the scaffolder's preference when Live's node
already meets the real runtime need. pnpm is **never required of Persona A** — the default
template is pure-JS and `--fix` owns any native-dep build internally (§3.7). Persona B sets
`[toolchain]` (§7).

**Prerequisites a musician satisfies themselves, each guided by doctor:** (1) **Ableton Live
Suite 12.4.5+ with the Extensions beta enabled** (Suite-only, beta-gated; doctor's no-Live
`help:` points at the download/upgrade page + beta enrollment, §2 doctor); (2) **the gated
SDK download** (`rackabel new` walks them through obtaining/placing it, §4/§6.2); (3)
**Developer Mode on inside Live** for the dev loop (doctor and `rackabel dev` detect it and
block with a navigational fix line, §3.6).

Intel Macs (`darwin-x64`) are supported: the dev host runs on Live's bundled node for
whichever arch Live is (native on Apple Silicon, Rosetta on Intel); doctor reports the
resolved arch and `pack` can target `darwin-x64`.

---

## 1. Vision & personas

rackabel builds two kinds of artifact for Ableton Live:

- **Max for Live devices** (`.amxd`) — the existing scope. Real-time/persistent, Max-based.
- **Live Extensions** (`.ablx`) — the new official SDK v1.0 scope. Node/TypeScript,
  right-click-invoked, run-once-to-completion. Suite-only, Live 12.4.5+ beta.

These are different runtimes with different lifecycles, but they share an audience, an
install target (the Live User Library), and a desire for a single tool with one mental
model. rackabel is that tool.

### Persona A — the musician (technical-but-not-developer)

Can use a terminal. Owns Live Suite. Wants to write a small extension ("rename my
selected clips", "a utility gain device") and *see it in Live* with the fewest possible
decisions. Cannot supply identifiers they have no way to know (install paths, host
versions, FQBN-equivalents). Will quit in the first five minutes if the first thing they
see is a Node stack trace or `error: missing required field` (onboarding research:
Arduino `board list` auto-detect; CRA day-one peer-dep death; rustc/Elm "compiler as
assistant").

**Promise to Persona A:** once rackabel is installed (§0) and Live's Extensions beta +
Developer Mode are on, `rackabel new` → `rackabel dev` and your extension is running in
Live, with logs in your terminal, before you have read any documentation. rackabel installs a
**working copy of just this extension** into Live's User Library (a `<slug>` folder it owns);
your **saved Live Sets are never touched**, and undoing it is two symmetric commands:
`rackabel deploy --undo` **removes the copy rackabel placed** in your User Library, and
`rackabel dev unregister` **stops watching it** (§2 — neither alone does both jobs). The
reversibility is what makes this safe: you cannot break your real Live setup doing this.

### Persona B — the power developer

Knows Node, esbuild, pnpm, git. Runs monorepos of extensions. Wants env-var overrides
exposed as flags, `--json` output for scripts, deterministic exit codes, `--inspect`
passthrough, multi-target packing, raw logs, and escape hatches from *every* managed
behavior. Will be annoyed by hand-holding that cannot be turned off.

**Promise to Persona B:** every default is overridable, every managed step has a bypass,
nothing rackabel does is a black box (`--dry-run`/`--print-config` reveal it), and
rackabel never strips you of the ability to drop down to raw `npm`/esbuild/the official
`extensions-cli`.

### The connecting principle: progressive disclosure, not two tools

A single command set serves both. The no-flag path is the musician path; the same
commands gain flags, env overrides, and a manifest for the developer. Graduation is
**continuous, never a cliff** — there is no irreversible `eject` (CRA postmortem:
`eject` was so destructive nobody used it, so it did not count as an escape hatch).
Defaults are friendly; flags are sharp (web-clis: Expo Go default vs `--dev-client`;
Wrangler local vs `--remote`).

Three rules enforce this everywhere:

1. **Never hard-fail on a missing config field.** Infer it and echo what you inferred:
   `using inferred name = my-ext (set [extension].name to override)` (Wrangler v2
   zero-config).
2. **Never require a hand-typed identifier on the happy path.** Auto-detect and echo
   (Arduino `board list`). Numbered pick-list, never free-text, when multiple are found.
3. **One declaration, many behaviors.** The user declares an extension once; rackabel
   derives watch globs, build inputs, install targets, and log channels from it. Never
   make the user maintain two parallel lists that drift (VS Code's `contributes` +
   `activationEvents` footgun, fixed by implicit activation in 1.74).

---

## 2. Command surface

### Naming decision: `rackabel dev <verb>`, not top-level dev-host verbs

The maintainer's sketch used a separate `extensions-dev` binary
(`start`/`stop`/`register`/`register-parent`/…). **Decision: fold the dev host into
`rackabel` under a `dev` command group** — not a second binary, not top-level verbs. One
binary, one mental model (web-clis: Shopify's Ruby-CLI-for-a-JS-audience split caused a
costly rewrite); a noun-grouped `rackabel dev start` keeps the top-level namespace clean for
the blessed verbs and PATH-discovered subcommands, the way cargo/git reserve short verbs and
group the rest; and `register-parent`/`unregister-parent` collapse into `register
--recursive`. The one exception: the flagship loop gets a **bare top-level `rackabel dev`**
(= start-if-needed + watch the registered set + tail logs), because the dev loop must be one
short verb (Raycast/Vite). `rackabel dev <verb>` is the explicit lifecycle controls below.

**Intra-`dev` precedence (name-vs-verb).** Bare `rackabel dev [NAME… | PATH]` takes registry
names, but the `dev` group reserves the verbs `start`/`stop`/`status`/`register`/
`unregister`/`enable`/`disable`/`list`/`watch`/`reload`/`logs`/`test`. **A `dev` verb always
wins over a same-named registry entry** (mirroring the top-level reserved-namespace rule):
`rackabel dev test` is the test subcommand, even if an extension is named `test`. So that an
extension named after a verb stays addressable by the bare loop, **`--only` always routes
through the name matcher** (§3.3), never the verb table: `rackabel dev --only test` watches the
extension named `test`. A `rackabel dev -- <NAME…>` separator does the same (everything after
`--` is treated as names). And to prevent the trap up front, the `dev` verb names are a
**reserved set that `register` checks**: registering an extension whose unique name would be a
`dev` verb triggers the same auto-disambiguation as a name collision (§3.2) — rackabel picks a
parent-prefixed name and prints it — so a user can't end up with a bare-loop-unaddressable
entry. `--name` can still force the bare name, with a one-line warning that only `--only`/`--`
will target it.

### Top-level tree

```
rackabel
  new            scaffold a project (Extension or M4L device)
  build          compile/bundle the artifact (no install)
  deploy         build + copy into the Live User Library (alias: install)
  pack           production build → distributable .ablx / .amxd
  validate       lint manifest + artifact against ship rules
  doctor         diagnose the environment
  dev            (bare) the managed dev loop over the registry
    start        launch the managed Extension Host daemon
    stop         stop the daemon
    status       show daemon + per-extension state
    register     add a path to the watched registry
    unregister   remove a path from the registry
    enable       re-activate a registered-but-dormant entry
    disable      keep an entry registered but dormant
    list         show the registry (alias: ls)
    watch        watch + auto-reload the registered set (or a one-off path)
    reload       force a host reload now (scriptable; CI manual-reload path)
    logs         tail/filter host logs
    test         run headless vitest/TestHarness tests — no Live (CI entry point, §3.8)
  plugin         manage rackabel's own third-party plugins (§5)
    install      install OWNER/REPO (release asset) or a path/tarball (sideload)
    list         installed plugins + enabled state + pinned ref (alias: ls)
    which        which file a name would run (or "shadowed by built-in")
    run          run a plugin even if a built-in shadows the name (§5.6)
    enable       enable a plugin / its hooks
    disable      disable a plugin / its hooks
    search       query the `rackabel-plugin` GitHub topic
    migrate      codemod a plugin's hooks across a `hook_api` bump (§5.3)
  explain        long-form help for an error code (cargo --explain)
  <foo>          PATH-discovered third-party subcommand `rackabel-foo` (§5)
```

`new --update` (copier-style 3-way template update, §5.5) and `plugin migrate` (hook-contract
codemod, §5.3) are the two non-leaf verbs surfaced here for the scan; their full synopses are
in those sections.

**Built-in precedence (reserved namespace).** A token that names a built-in subcommand —
including the `dev` group, `new`, `build`, `deploy`, `pack`, `validate`, `doctor`,
`plugin`, `explain` — **always resolves to the built-in and can never be shadowed by a
PATH executable** (git/cargo behavior). PATH lookup for `rackabel-<foo>` (§5.1) happens
*only* for tokens that match no built-in. `rackabel plugin which <name>` prints
`shadowed by built-in` if a `rackabel-<name>` exists but a built-in claims the name, and
points at `rackabel plugin run <name>` to invoke the plugin anyway (§5.6). The reserved
set and the policy for future built-ins colliding with installed plugins are in §5.6.

`install` is kept as a hidden alias of `deploy` because the existing M4L code and README
already use it; `deploy` is the canonical name because for Extensions "deploy" (copy the
*bundle* into the User Library) is the load-bearing distinction behind the
deploy-before-reload trap (§3).

### Per-command spec

Each command works on both artifact kinds. rackabel infers the kind from
`rackabel.toml` (`[extension]` vs `[device]`) and dispatches to the right provider
(§4, §5). A flag-free invocation in a project directory Just Works.

---

#### `rackabel new [NAME]`

**Synopsis:** `rackabel new my-ext [--kind extension|device] [--template …] [--minimal] [--yes] [--no-input] [--update]`

`--update` re-runs the template's 3-way merge against the current project (copier-style,
§5.5) — a developer action, never on the Persona-A happy path.

**Behavior:** interactive wizard by default (no NAME ⇒ prompt for it). Asks, with
bracketed Enter-to-accept defaults: kind (Extension / M4L device), name
[`<dir>`], author [`git config user.name`], license [MIT], template
[default working example]. Scaffolds a project that **builds, deploys, and shows up in
Live with zero edits** — the default Extension template adds one working context-menu
action (e.g. "Rename selected clips") plus one command; the default device template is an
audible gain/utility device. (The wizard's template label deliberately says only "a working
right-click action" for Persona-A clarity even though the template also ships one command —
the two descriptions are intentionally aligned, not divergent.)

Runs a build immediately after scaffolding **when a usable node is present** (Live's bundled
node, else a PATH node — §0) and prints the exact next command. **If neither is present** (a
musician evaluating rackabel before installing Live — a real, common first state), `new`
**skips the auto-build instead of dead-ending**: it still creates the project, prints a
friendly note, and points at `doctor` (see §6.2). The auto-build is therefore never the
command that produces a raw "node not found". On the with-node happy path:

```
✓ created my-ext/ (extension, default template)
✓ built (84ms) — dist/extension.js
  next:  cd my-ext && rackabel dev
```

For Extensions, scaffolding **reuses/extends the official `create-extension` scaffolder**
rather than reinventing it (full decision in §4.7): shell out when present and post-process
its output into rackabel form, or use rackabel's forked equivalents when absent. Either way
the gated SDK+CLI tarballs end up vendored and wired via `file:`.

**The gated-SDK problem** (§4): the SDK is a separate, beta-gated download (not on public npm
during beta, §9.9). `rackabel new` finds the tarballs by a **recursive** search of the download
dir (or `--sdk-dir`) and **tolerates every shape a non-developer ends up with** — the raw
`.tgz`, an already-expanded toolkit *folder* (Safari/macOS auto-expand some archives), or a
vendor folder dropped anywhere in the search root — locating both SDK and CLI at any depth; if
both a `.tgz` and an expanded form exist it picks the **expanded/newer** one and echoes which.
The user never re-flattens or re-tars anything. If still not found, it prints the full §6.2
guidance (where to request access, where to put it, the one command to re-run) **plus** the
interactive `rackabel new` → pick-file fallback — so this branch never dead-ends. This is the
single biggest "get started" lever for musicians.

**Key flags:** `--kind`, `--template gh:user/repo|@scope/name|<path>`, `--minimal`
(power-user bare skeleton), `--yes`/`--no-input` (CI, accept defaults), `--sdk-dir`,
`--git`/`--no-git`.

**Rationale:** interactive-by-default + fully-flag-drivable serves both audiences from
one command (yo code `-q`; `npm create vite`). A working, commented "blink" as the
default template makes the first signal *success*, not a wall of yellow (Raycast: zero
warnings on first build; Arduino File→Examples; norns First Light).

---

#### `rackabel build`

**Synopsis:** `rackabel build [--release] [--clean] [--typecheck] [--print-config] [--dry-run] [--json]`

**Behavior (Extension):** esbuild-equivalent bundle of `src/extension.ts` →
`dist/extension.js`: `format=cjs`, `platform=node`, `bundle=true`, `define global=globalThis`,
**with rackabel's polyfill banner baked in** (the value-add the official `build.ts` omits;
full rationale §4.6). Externalizes declared native deps. `--release` ⇒ minify on, no
sourcemap; otherwise sourcemap on. Optional `tsc --noEmit` (`--typecheck`, default on for
`--release`). Then **generates the SDK's `manifest.json` from `rackabel.toml`** (§4) and
validates: bundle `>10KB`, `node --check` passes. Build is incremental; prints `rebuilt in
84ms` and a build hash so "did it actually rebuild?" is never a mystery (JUCE/CMake
stale-cache postmortem).

**Behavior (device):** the existing `.amxd` assembly path, unchanged in shape.

**Key flags:** `--release`, `--clean` (blow away the build dir — first-class, not
folklore), `--typecheck`/`--no-typecheck`, `--print-config` (dump the resolved esbuild
config and exit — escape hatch for Persona B), `--dry-run` (print the planned steps and
exit, mutating nothing — generic across `build`/`deploy`/`pack`), `--json`.

**Rationale:** default to esbuild with sub-second incremental rebuilds and bundle to a
single entry (VS Code: bundling is first-class; never ship a node_modules tree). Print
build time so the user feels the speed (Raycast status line).

---

#### `rackabel deploy` (alias `install`)

**Synopsis:** `rackabel deploy [--user-library PATH] [--live PATH] [--release] [--undo] [--fix] [--dry-run] [--json]`

**Behavior:** `build` (if stale) then copy `manifest.json` + `dist/extension.js` +
declared extra dist files into `<UserLibrary>/Extensions/<slug>`. **`slug` = the project
root directory basename**, independent of the manifest display name — it drives the
install folder, storage/temp dir names, and host registration. This is rackabel's deploy
convention, inherited from `dev-launch.sh`/`deploy-extension.js` (which use the deployed
dir basename as the extension identity); the SDK itself keys off the extension *path* and
manifest, not the folder name, so this is a rackabel/launcher convention, not an SDK rule.
For native-dep extensions, walks the runtime graph (deps + optionalDeps, not peer), follows
pnpm symlinks, asserts the compiled `.node` exists, and recopies `node_modules`. A missing
`.node` never dumps a bare `pnpm` command at the user — `--fix` owns that build under the
hood with a plain-English help line (§3.7).

`--release` runs `validate` first (§5) and fails the deploy on any validation error.
`--undo` removes the deployed `<UserLibrary>/Extensions/<slug>` folder rackabel created,
restoring the User Library to its pre-deploy state (the discoverable cleanup path Persona
A can use to fully reverse a deploy).

**User Library resolution order** (echo the resolved value, Arduino style):
`--user-library` flag → `rackabel.toml [host].user_library` → `$ABLETON_USER_LIBRARY`
→ newest-mtime `~/Music/Ableton*/User Library` that contains `Extensions` → platform
default. If multiple Live installs/libraries are found, numbered pick-list, never
free-text.

**Rationale:** the host (whatever launches it) and deploy must agree on the target
directory, so deploy resolves the *same* User Library rackabel's host launch uses — this
is internal consistency between rackabel's launch and its deploy target, not an SDK
mandate. `ABLETON_USER_LIBRARY` is a convention inherited from the launcher
(`dev-launch.sh`/`deploy-extension.js`), not an official SDK env var; the official CLI's
own env var is `EXTENSION_HOST_PATH` (which points at the host module, §3.1, §7).
Auto-resolve + echo, not hand-typed paths (onboarding research).

---

#### `rackabel pack`

**Synopsis:** `rackabel pack [--target os-arch …] [--include GLOB …] [--output PATH] [--no-official-cli] [--dry-run] [--json]`

**Behavior:** production build + `validate` (never produce a distributable that fails
validation — Stream Deck/Raycast pre-publish gate), then assemble the official `.ablx`
container. The official `extensions-cli package` **exists today** (beta tarball) and
produces `.ablx` (an `archiver` zip of `manifest.json` + the manifest `entry` file + any
explicit `-i` includes), invoked `npx extensions-cli package -o … -i …`. Two paths,
because the official packager is **not a superset**:

- **Pure-JS extensions ⇒ shell out to `extensions-cli package` (the default path, now).**
  It is the canonical packer; be a thin, legible wrapper over the SDK's own tool, not a
  reimplementation that drifts (audio-music research). The official CLI writes
  `<name-with-spaces-as-dashes>-<version>.ablx` to the **extension dir** (or wherever `-o`
  points) — `releases/` is the Arclight `pack-extension.js` convention, **not** the official
  packager's default. rackabel surfaces that exact `<name>-<version>.ablx` filename (rather than
  inventing a `<slug>-v…` name) and may pass `-o releases/…` if it chooses to collect outputs
  there.
- **Native-dep extensions ⇒ rackabel's own packer.** The official packager archives only
  `manifest.json` + entry + includes — **it does not bundle `node_modules` at all**, so it
  cannot produce a working native-dep bundle. For these, rackabel uses its own packer (the
  `pack-extension.js` model: `collectDepTree` over deps+optionalDeps, `slimPrebuildsDir`
  to the matching suffix, same-OS-different-arch cross-compile via node-gyp, cross-OS
  errors with a clear message — SDK constraint), producing one archive per declared
  `[extension.pack].targets`: `<slug>-v<version>-<os>-<arch>.ablx`.

`--no-official-cli` forces rackabel's own packer even for the pure-JS case (Persona B
escape hatch). Prints copy-pasteable install instructions ("drop `<file>.ablx` into Live →
Settings → Extensions"). The `.ablx` format itself is available now; an integrity
hash / signing scheme is not yet announced and is deferred (§9.5).

**Include guards (pre-validated for a friendly error).** The official `packageExtension`
requires every `-i`/`--include` to be a **relative** path that stays **inside** the extension
dir (an `isInsideDir` check — absolute or escaping paths are rejected) and to **exist** (a
missing include errors). rackabel **pre-validates** these and emits its own three-part error
(§6.1) — "`--include ../x` must be relative and inside the extension dir", "`--include foo`
not found" — rather than passing through the raw official-CLI message.

**Key flags:** `--target` (repeatable), `--include`/`-i`, `--output`/`-o` (mirror the
official CLI flags), `--no-official-cli`, `--dry-run`.

---

#### `rackabel validate`

**Synopsis:** `rackabel validate [--json]` (auto-run inside `pack` and `deploy --release`)

**Behavior:** a pass/fail checklist — manifest completeness (name/author/entry/version/
minimumApiVersion), `minimumApiVersion ≤ detected host apiVersion`, version bumped vs the
last packed version, changelog entry present, native `.node` files present and matching
the target, **stable-identifier drift** (a command id present in the last packed manifest
that has disappeared or been renamed ⇒ "this breaks existing users' saved state — keep
the old id or provide a migration"). Emits:

```
✓ manifest complete
✓ minimumApiVersion 1.0.0 ≤ host 1.0.0
✗ CHANGELOG.md has no entry for 1.2.0
warning: command id `rename` was removed (present in 1.1.0) — existing setups may break
1 failed, 1 warning
```

**Rationale:** catch ship problems locally with the exact rule named, before the
demoralizing remote-rejection loop (creative-plugins: Raycast/Stream Deck validate).
Treat stable identifiers as a compatibility contract (M4L Long-Name rename postmortem;
Ableton Python 2→3 ecosystem wipeout).

---

#### `rackabel dev` (bare) and the `dev` group

The headline. Detailed architecture in §3. Command-surface summary:

| Command | Synopsis | Behavior | Rationale |
|---|---|---|---|
| `rackabel dev` | `rackabel dev [NAME… \| PATH] [--only GLOB] [--no-auto-reload] [--raw] [--emit-launch-config] [--inspect[=host:port]]` | Ensure the daemon is up; watch the registered set — or, if `NAME…`/`--only` given, a transient **working set** of just those (§3.3 scoping); auto-reload on change; tail pretty per-extension logs inline; print a live hotkey legend. `--inspect` is passed through to the host's node; `--emit-launch-config` writes a VS Code `launch.json`. | Raycast `ray develop`: one verb = build+launch+reload+logs. |
| `dev start` | `rackabel dev start [--live PATH] [--foreground] [--inspect[=host:port]] [--emit-launch-config]` | Launch the managed Extension Host (replaces `dev-launch.sh`); daemonized by default. | maintainer sketch `start`; flutter/expo "managed runtime". |
| `dev stop` | `rackabel dev stop` | Stop the daemon cleanly (forward the *correct* reload/term signal). | maintainer `stop`; avoids the wrong-PID-SIGHUP trap. |
| `dev status` | `rackabel dev status [--json]` | Daemon state + per-extension state (registered/deployed/loaded/failed/skipped), Live + host paths, Developer Mode state, inspector state + port (when active, §7), **and the measured last-reload ms + rolling p50** so the whole-host reload cost (§3.3) is observable. | flutter doctor summary; Wrangler resolved-target echo. |
| `dev register` | `rackabel dev register [PATH] [--type extension\|device] [--recursive] [--name NAME] [--disabled]` | Add a path to the persistent registry; the path may be a `rackabel.toml` project **or** a manifestless `package.json` dir (§4.1). `--type` supplies the kind at registration time (stored on the entry); it **defaults to `extension`** and is how a manifestless project opts into device. `--recursive` registers each `manifest`-bearing subdir (the monorepo case). **`--name` is mutually exclusive with `--recursive`** (one name cannot label N members — rejected at parse time, exit `2`); per-member renames after a recursive register are a follow-up `dev register <member-path> --name X` or a hand-edit of `registry.toml` (§3.2). | maintainer `register`/`register-parent`→ one flag; VS Code persistent registry. |
| `dev unregister` | `rackabel dev unregister <NAME\|PATH> [--recursive]` | Remove from the registry. | maintainer `unregister`. |
| `dev enable` | `rackabel dev enable <NAME\|PATH>` | Flip a dormant entry back to `enabled = true`. | VS Code enable/disable; one verb to flip state. |
| `dev disable` | `rackabel dev disable <NAME\|PATH>` | Flip an entry to `enabled = false` (registered but not loaded). | VS Code enable/disable. |
| `dev list` (`ls`) | `rackabel dev list [--json]` | Show registry with status columns. | persistent registry > re-listing. |
| `dev watch` | `rackabel dev watch [NAME… \| PATH] [--only GLOB] [--no-auto-reload]` | Explicit form of bare `dev` (no implicit daemon-start). | — |
| `dev reload` | `rackabel dev reload [NAME…] [--strict] [--json]` | Force a whole-host reload now (the manual-reload path under `--no-auto-reload`, and the scriptable CI trigger). Exit `0` once the host re-initializes and every targeted extension reports loaded; exit `1` if any targeted extension fails `activate()`; exit `3` if the host can't reload (Dev Mode off / no daemon). **Host-incompatible extensions that were pre-filtered (§3.2) are reported on stderr and in `--json` as `Skipped:` rather than silently dropped**; exit stays `0` unless `--strict`, which makes any skip fatal (`exit 1`) so a CI gate can't mistake "silently dropped one as incompatible" for "all good". | the `--no-auto-reload` escape hatch needs a command surface scripts can gate on. |
| `dev logs` | `rackabel dev logs [NAME] [--follow] [--since 5m] [--level LEVEL] [--json] [--raw]` | Tail/filter the host's per-extension log sink; addressable by registered unique name from anywhere. | Wrangler `tail`; VS Code LogOutputChannel. |
| `dev test` | `rackabel dev test [NAME… \| PATH] [--bail] [--json] [-- <runner args>]` | Run the project's vitest/TestHarness tests and any `*:headless` script when present; best-effort generic smoke `activate()` otherwise — no Live, no Developer Mode, no GUI (§3.8). **Non-interactive by default** (never prompts, so CI can't hang); `--bail` fails fast; `--` forwards args verbatim to the underlying runner (vitest). Inherits the §7 exit-code taxonomy. The CI entry point for exercising `activate()`/commands. | CI cannot run the GUI loop; the SDK ships a (proof-of-concept) TestHarness, used via vitest (§3.8). |

`dev enable`/`dev disable` flip the `enabled` flag on an existing entry;
`register --disabled` is simply the one-shot way to *add* an entry already dormant. So
there is exactly one documented state (`enabled` in `registry.toml`), one way to add an
entry in either state (`register [--disabled]`), and one pair of verbs to flip it
(`enable`/`disable`).

---

#### `rackabel doctor`

**Synopsis:** `rackabel doctor [--verbose] [--json] [--fix]`

**Behavior:** a flutter/expo-style checklist with a stable symbol vocabulary
(`[✓]`/`[!]`/`[✗]`), a per-failure `help:` fix line, and a tail summary count.
Quiet-on-success: passes collapse to a count unless `--verbose`. **Internals
(`NODE_MODULE_VERSION`, exact `apiVersion`, resolved host module path) are hidden behind
`--verbose`/`--json`** — the default checklist a musician sees reads as "you're fine,"
e.g. `[✓] Live's components are compatible`. Checks (both kinds):

- Ableton Live install(s), version, **arch** (12.4.5+ Suite beta; Apple Silicon native or
  Intel/`darwin-x64` under Rosetta) — echo detected. **No-Live case:** `[✗] No Ableton Live
  install found — help: install Live Suite 12.4.5+ and enable the Extensions beta (Live →
  Settings → … → Beta), then rerun rackabel doctor.`
- Extension Host layout: probe `Contents/Helpers/ExtensionHost` then
  `…/App-Resources/Extensions/ExtensionHost`; report which exists (never hardcode).
- Live **bundled** node vs the `[toolchain].node_runtime` floor (>=22.11.0, the SDK/CLI
  `engines`; ABI match; `NODE_MODULE_VERSION` only under `--verbose`). Below the floor ⇒ `help:`
  says upgrade **Live**, never "install Node" (§0). The stricter scaffolder `node_build` floor is
  not checked against Live's runtime.
- Developer Mode state. OFF ⇒ a **navigational** line naming it as the gate for the whole
  dev loop (the §3.6/§6.2 text). Also warns if a *bare* node host is running (SIGHUP-unsafe).
- **Live running** — informational in a static `doctor` run; in the fast subset that `dev`
  runs it is a gate: `dev` **blocks-and-waits** ("open Ableton Live… I'll continue
  automatically", §6.2) exactly like the Dev-Mode-off wait, instead of hanging on
  "connected to Live".
- User Library resolution (show the resolved path and how it was chosen).
- SDK present + version (`@ableton-extensions/sdk`/`cli`), vendored tarballs. **When doctor
  runs OUTSIDE a project** (the sensible "check first, then create" order), there is no
  vendored toolkit to find — so this row reads as a non-blocking note, **not** a red X:
  `[!] Extensions toolkit — not needed until you run `rackabel new` (it vendors the toolkit
  into your project)`. The toolkit-ready `[✓]` appears only once a project has vendored it.
- Manifest validity; **`minimumApiVersion` compatibility** (treated as a hard
  pre-deploy/pre-launch gate, not a warning: a host-incompatible `minimumApiVersion` may abort
  host init for *all* registered extensions — an open question, §9.6 — and the daemon pre-filters
  for it regardless, §3.2); native deps compiled.
- **Deployed-vs-source drift** (stale-deploy detection — the deploy-before-reload trap,
  §3): warn if `dist/extension.js` is newer than the deployed copy.
- Max + Max for Live (device path).

`build`/`deploy`/`dev` run a fast subset of doctor first and fail with the doctor-style
remedy, never a raw SDK trace. `--fix` performs safe auto-fixes — vendor the SDK, **build
native deps under the hood (locate/use pnpm, run `approve-builds`+`rebuild` so the user
never types a pnpm command, §3.7)**, redeploy a stale bundle — and points at the rest.
`doctor --watch` (implied by `rackabel dev` at launch) polls Developer Mode and proceeds
the moment it flips on, so the user isn't guessing when to rerun.

**Rationale:** flutter doctor is the gold standard because the remedy travels with the
diagnosis; doctor is the on-ramp that *fixes* prereqs, not just reports red X's
(onboarding research).

---

## 3. The dev loop architecture

This is the headline feature and the part the SDK makes hardest. The design's job is to
make the two documented traps **impossible to hit**.

### 3.1 Daemon vs foreground

**Decision: a long-lived background daemon, owned by rackabel, that *is* the host
wrapper** — with a foreground escape hatch (`dev start --foreground`, and the bare
`rackabel dev` runs the watch UI in the foreground while the host runs as a child it
fully owns).

**Two launch models exist in ground truth; rackabel re-implements the right one.** (1) The
launcher scans the User Library `Extensions/` dir, builds `initialize({extensions:[MANY]})`
from the **deployed** bundles, and traps SIGHUP to reload all — the only model that supports
a registry + watch + reload loop. (2) The official `extensions-cli run` takes a **single**
extension dir, points the host at that **extension dir** (whose `manifest.entry` is the
freshly built bundle `dist/extension.js`, not raw `.ts` — there is still a build, just no
separate User-Library deploy), calls `initialize({extensions:[ONE]})`, and **exits** — no
SIGHUP, no reload, no watch, no deploy. **Decision: the daemon
re-implements model (1) in Rust**, because model (2) structurally cannot own a watch loop.
`extensions-cli run` is therefore *not* a candidate to own the dev loop; at most it is the
one-off behind `rackabel dev PATH` (§9.11 corrected).

The launcher is a foreground bash wrapper whose **wrapper PID** is the only correct SIGHUP
target; SIGHUP-ing the bare node host triggers each extension's own cleanup with no
re-init and leaves the host half-dead (recovery needs a Developer-Mode toggle or a Live
restart). rackabel must **own that wrapper process itself**, so:

- The user never learns a PID and never sends a signal. `dev stop`/reload go through
  rackabel, which sends the right signal to the right PID, every time. The wrong-PID
  SIGHUP trap becomes structurally unreachable.
- The daemon survives terminal close, so `rackabel dev logs` from a second terminal works
  (Wrangler `tail` from anywhere). One daemon serves the whole registered set.
- A foreground mode exists for those who want the process tied to their shell / for CI.

The daemon is a thin rackabel-managed supervisor around
`EH_NODE -e 'require(EH_MOD).initialize({extensions:[…]})'`, as `dev-launch.sh` does.
**Per-extension object shape (for the Rust re-implementation):** each element the daemon emits
into `extensions:[…]` is `{ path, storageDirectory, tempDirectory }`. Ground truth on who
passes what: `dev-launch.sh` (both repos) always passes the full triple; the official
`extensions-cli run` passes `{ path }` **alone**, adding `storageDirectory`/`tempDirectory`
only when its `--storage-directory`/`--temp-directory` flags are given (run.mjs
`buildExtensionObject`) — so the two extra fields are **optional to the host, not required**.
rackabel **deliberately always supplies all three** (matching `dev-launch.sh`) so dev storage
is stable and predictable rather than falling back to whatever the host would choose — a
rackabel choice, not an SDK mandate (§9.7 tracks what Live's own host assigns). All paths are
**forward-slash-normalized** (the official `run` runs them through `toForwardSlash` before
`JSON.stringify`). `path` is the deployed extension dir; `storageDirectory`/`tempDirectory`
are drawn from the §3.6 layout
(`~/Library/Application Support/Ableton/Extension Data/<slug>` and `/tmp/<slug>`), and the
daemon **mkdir -p's both before launch** (as `dev-launch.sh` does) so init never fails on a
missing dir.
**Node (`EH_NODE`):** Live's **bundled** node next to `ExtensionHostNodeModule.node` (for
ABI match), else **PATH node** (the `dev-launch.sh` `command -v node` fallback, *not* the
official `run`'s `process.execPath` — a Rust daemon has no embedded node). The bundled-node
**basename is platform-specific** — `node` on macOS, `node.exe` on Windows (per the official
`findExtensionHost`) — so the daemon's `EH_NODE` probe must be platform-aware even though
Windows daemon/signal mechanics are deferred (§9.3). **Host module
(`EH_MOD`):** the **modern** layout comes from the official CLI's
`findExtensionHost`/`resolveExtensionHostDir` (verified: for a `.app` it returns **only**
`Contents/Helpers/ExtensionHost`; on Windows `<root>\Program\ExtensionHost`) — rackabel
reuses that for the modern path. **The legacy alpha layout
`Contents/App-Resources/Extensions/ExtensionHost` is NOT in the official resolver** (it is
probed only by the launcher's `dev-launch.sh` `EH_LAYOUTS` array); so rackabel keeps its
**own two-layout probe in Rust** — modern path first (matching the official resolver), legacy
`App-Resources` path as a fallback — and does **not** lean on the official resolver for the
legacy case. This keeps §3.1 consistent with the §2 doctor item, which probes both layouts.
(If Live 12.4.5+ becomes a hard floor and the alpha layout is confirmed dead, the legacy
probe can be dropped; until then rackabel owns it, §9.3.) The official CLI surfaces the host
module as `EXTENSION_HOST_PATH` (read from `.env`, overridable with `--live`); rackabel's
`--eh-mod`/`ABLETON_EH_MOD` (§7) is the same concept and is reconciled with it, so a
create-extension `.env` is honored.

### 3.2 The registry: location, format, persistence

`register`/`unregister` write a persistent, human-readable, hand-editable registry so the
host launches a **curated set** instead of re-scanning (maintainer's core ask; VS Code
`~/.vscode/extensions`; Wrangler `.wrangler/state`; "delete the file to reset").

- **Location:** `~/.rackabel/registry.toml` (global). A per-project `.rackabel/` holds
  build/log/daemon state and is added to the generated `.gitignore`.
- **Format (TOML, hand-editable):**

```toml
# ~/.rackabel/registry.toml  —  managed by rackabel, but safe to hand-edit
[[extension]]
name = "harmonic-lens"          # unique name; how you address it in `dev logs harmonic-lens`
path = "/Users/x/code/harmonic-lens"
source = "dist"                  # "dist" (watch repo dist) | "deployed" (watch User Lib)
enabled = true

[[extension]]
name = "groove-transplant"
path = "/Users/x/mono/packages/groove-transplant"
enabled = false                  # registered but dormant (--disabled / `dev disable`)
```

`register --recursive PATH` walks `PATH`'s subdirs and adds each manifest-bearing subdir
(the monorepo / `register-parent` case) as a separate entry. **Name-collision handling:**
the entry `name` defaults to the dir basename, but `--recursive` over a monorepo can yield
two `foo` dirs in different members. On collision rackabel **auto-disambiguates** with a
parent-prefixed unique name (`packages-a-foo` vs `vendor-foo`) and prints what it chose.
`--name` overrides for a **single-path** register only — combining it with `--recursive` is a
parse-time error (exit `2`), since one name cannot label N members; per-member renames after a
recursive register are a follow-up `dev register <member-path> --name X` or a hand-edit of
`registry.toml` (hand-editability exists precisely for this). The unique `name` is what `dev logs <name>` addresses **and** what
keys the per-extension log sink for framed lifecycle events (§3.4; free-form `console.*` is
best-effort, since the host does not tag lines by extension).

The bare `rackabel dev` watches every `enabled` entry with no re-listing.
`dev enable`/`dev disable` flip an entry's `enabled` flag (the dormant-toggle, central to
managing a multi-extension registry); see the §2 `dev` table for the single canonical
state model.

**The daemon filters the registry to host-compatible extensions before launch — a NEW
rackabel behavior the launcher lacks.** `dev-launch.sh` registers **every** manifest-bearing
subdir unconditionally (`[[ -f manifest.json ]] || continue`, whole set into
`initialize`, **no** `apiVersion` check anywhere) — exactly the failure mode rackabel fixes, so
the pre-filter is something rackabel **adds**, not an inherited pattern. The daemon **drops**
any extension whose `minimumApiVersion` exceeds the detected host `apiVersion`, marking it
`Skipped: <name> (minimumApiVersion=… > host …)` in `dev status` — so one bad manifest can
never take down the dev session. Whether an incompatible `minimumApiVersion` *actually*
aborts-all (vs drops-one) is unestablished and an open question (§9.6) — but the pre-filter is
safe **either way**: it prevents abort-all and makes drop-one explicit.

Registry/doctor logic is **operable even when the host is broken** (SuperCollider Quarks
postmortem: never let the package/registry manager depend on the runtime it manages;
`dev list`, `doctor`, `dev register` must work with a dead daemon).

### 3.3 Reload triggers — the deploy-before-reload trap, solved

The defining trap of the **deployed-bundle (launcher) model rackabel uses**: SIGHUP
reloads the DEPLOYED bundle in the User Library, not the repo `dist/` or source. A bare
`build` is not enough; only `deploy` copies the bundle. The naive loop (edit → SIGHUP)
reloads stale code silently. (Note this trap is specific to the deployed-bundle model; the
official `extensions-cli run` points the host at the extension *dir*, whose `manifest.entry`
is the freshly built bundle (`dist/extension.js`, set by the scaffolder) — so there is still a
build step and the host never loads raw `.ts`, but there is no separate User-Library deploy
step — and it also has no reload primitive at all, §3.1, so it cannot drive this loop.
rackabel uses the deployed-bundle model and owns the ordering in code, below.)

**The maintainer's `restart.txt` sentinel idea, honestly:** a sentinel (rebuild touches
`restart.txt`; the watcher reloads on its change) decouples "build done" from "reload now" and
works as a *manual* trigger — but on its own it **does not close the trap**: touch it after
`build` but before `deploy` and you reload stale code. It reintroduces the VS Code failure of
coupling "build done" to a side-channel the user can desync (tasks.json problemMatcher race).
**The right fix is not a user-maintained sentinel; rackabel owns the ordering in code.**

**Decision: rackabel chains build → deploy → reload as one atomic step, ordered in code,
gated on a real compiler success signal.** The watch loop:

1. File change in a watched extension's *source/build inputs* (watch globs **derived**
   from the build config — one declaration, never a separate watch list). **The watch set
   for an extension extends to the SOURCE tree of every internal `workspace:*` library it
   depends on (§4.4).** Because esbuild bundles a `workspace:*` library from its prebuilt
   `dist/`, the bundle graph terminates at the library's compiled output and a naive
   "derive globs from the bundle graph" would watch `dist/`, not `src/` — so editing
   `arclight-core/src/*.ts` would trigger no rebuild and dependent extensions would reload
   stale code. rackabel closes this by walking each watched extension's `workspace:*` deps
   and adding **their** `src` globs to that extension's watch set.
2. Debounce; **if the change was in an internal `workspace:*` library, rebuild that library
   first** (e.g. `tsc --build` / `pnpm --filter <lib> build`) so its `dist/` is fresh, then
   run the dependent extension's incremental esbuild — i.e. rackabel **owns the
   `build-library → build-extension` chain** the reference `predeploy`/`pack` scripts run by
   hand. A single library edit fans out to a rebuild of every dependent extension. If the
   change was only in the extension's own source, skip straight to its esbuild.
3. **Only on a clean build** (the compiler's success signal, not a regex/sentinel),
   `deploy` the fresh bundle to the User Library.
4. **Only after deploy completes**, trigger the host reload.
5. On a *failed* build: keep the last good deployed artifact loaded, print a framed error
   (§6), and do **not** reload. Never reload against a half-built bundle (VS Code
   anti-pattern).

Because rackabel owns steps 3 and 4, "reload stale code" is unreachable. The sentinel idea is
adopted *internally and inverted*: rackabel may write a `.rackabel/reload` marker into the
deployed folder as the very last step and reload on *that* (so the signal can never precede the
deploy) — an implementation detail the user never touches, not a contract they can desync. The
user sees only `reloaded harmonic-lens (build 84ms → deploy 11ms)`.

**Reload granularity, blast radius, and scoping.** The host's only documented reload
primitive is whole-host SIGHUP (re-scan + respawn all). rackabel **aspires to per-extension
reload** but the v1.0 host does not expose it (§9.1). Consequence for a monorepo: with N
extensions loaded, every save in *any* one re-runs `activate()` for **all N** (cost ≈ one
respawn + N `activate()` per reload — a respawn+re-init is roughly a few hundred ms plus the
sum of each `activate()`). The daemon **debounces a burst to one reload with a default 200 ms
window** (overridable via `[dev].debounce_ms`), prints which extension triggered it, and reports
the time. **The respawn figure is a budget to measure, not an asserted constant:** `dev status`
surfaces the **measured last-reload ms** (and a rolling p50) so the whole-host cost is
*observable* rather than asserted, and when the `enabled` set exceeds a threshold (default 4) or
a measured reload p50 exceeds a budget (default ~750 ms), **bare `rackabel dev` prints a one-time
hint** to scope: `7 extensions loaded — reloads re-run all of them; scope with
`rackabel dev --only <name…>` for faster saves`. To bound the cost the bare/`watch` loop takes a
**working set**:
`rackabel dev <NAME…>`/`--only <glob>` loads *only* those into the host for this session
(transient, not a registry edit), the recommended path for large repos; with no argument the
whole `enabled` set loads. **`--only GLOB` matches the registry unique `name`** (the same token
`dev logs`/`dev reload` use) — **not** dir basenames or workspace globs — so after
collision-disambiguation (§3.2) it matches the **disambiguated** name (`packages-a-foo`, not
`foo`); `dev list` shows the matchable names. Example: `rackabel dev --only 'harmonic-*'`.
**`--only` (and the `rackabel dev -- <NAME…>` separator) always route through this name
matcher, never the `dev` verb table** (§2), so an extension whose name collides with a `dev`
verb — `test`, `reload`, `status`, … — stays reachable: `rackabel dev --only test` watches the
extension `test`, while bare `rackabel dev test` is the subcommand. (`register` also
auto-disambiguates names that collide with `dev` verbs up front, §2/§3.2.)

**Sibling isolation.** A failing/slow sibling `activate()` is isolated and logged but does
**not** abort the extension you are editing: the daemon pre-filters host-incompatible
manifests (§3.2) before init, and a sibling throwing in `activate()` is surfaced as a framed
per-extension error (§3.4) while the rest stays up. (The most dangerous case — an incompatible
`minimumApiVersion` possibly aborting init for *all* of them, §9.6 — is removed up front by the
§3.2 pre-filter regardless of which way the host actually behaves.)

`--no-auto-reload` opts out for the rare debugging case; manual reload is then `[r]` in the
UI or `rackabel dev reload [NAME…]` from a script (auto-reload is the default you are
explicitly fixing).

**Developer-Mode-off case.** When Live runs the host itself (Developer Mode off), there is
no wrapper to own and no SIGHUP channel — rackabel cannot reload, and the entire `dev`
loop is unavailable. So `rackabel dev` does **not** start a host that can't reload: at
launch it detects Dev-Mode-off and **blocks** with the same navigational line doctor uses —
`Developer Mode is OFF — the dev loop cannot run without it. Open Live →
Settings/Preferences → Extensions → enable Developer Mode (it appears once you've joined
the Extensions beta), then this command continues automatically.` — and then **polls until
the toggle flips**, proceeding the moment it does (so the user isn't left guessing when to
rerun). `doctor`/`dev status` report the same state. (§9.2: what IPC, if any, exists in
Dev-Mode-off is unverified; the stance is that `rackabel dev` requires Developer Mode.)

### 3.4 Log capture & streaming

The host's only native log sink is a single `ExtensionHost.txt` in Live's **per-version**
Preferences dir (no rotation, no per-extension separation) — the most-cited debugging pain in
adjacent ecosystems (Figma's "impenetrable errors"). It is per-Live-version, so the daemon
resolves the version-specific dir, not a fixed path: macOS
`~/Library/Preferences/Ableton/Live <x.x.x>/ExtensionHost.txt`; Windows
`%APPDATA%\Ableton\Live <x.x.x>\Preferences\ExtensionHost.txt`. rackabel owns logging on top:

- The daemon captures host stdout/stderr (stdio is inherited from the node child, so
  `console.log` from `activate()` does reach it — verified) and tails the version-resolved
  `ExtensionHost.txt`, fanning it into **leveled, timestamped sinks**. **Per-extension keying is
  reliable for lifecycle/framed events** — `activate()` start/finish, load failures, the
  liveness banner — because rackabel frames those itself around the extension it is initializing,
  keyed by the **unique registry name** (§3.2): `~/.rackabel/logs/<name>/<session>.log` (VS Code
  LogOutputChannel). For **free-form `console.*` output it is best-effort**: the host emits a
  single shared stream and does **not** tag lines by emitting extension, so an arbitrary log line
  cannot always be attributed to one extension (true per-extension separation is a
  SDK-IMPROVEMENTS wishlist item, not a current host capability). `doctor` prints these paths.
- `rackabel dev logs [name]` tails them with `--follow`, `--since 5m`, `--level`, pretty
  (colorized, per-extension prefix) by default and `--json` for scripts (Wrangler
  `tail`). `--raw` shows the unfiltered host output (Node internals included).
- **Surface silent activate failures.** Module-load/`activate()` errors are otherwise
  invisible (only `ExtensionHost.txt`). The daemon detects them and prints a framed error
  with the extension name and, where a sourcemap exists, the mapped `file:line` —
  **never a raw opaque host error without first attempting to map it back to the user's
  TypeScript** (creative-plugins anti-pattern).
- On reload, print a liveness banner: `harmonic-lens v0.3 active — 2 commands, 1 menu
  action` (AbletonOSC "listening on port…" pattern — confirm it actually loaded).

### 3.5 Crash recovery

If the host child dies, the daemon does **not** kill the watch loop. It prints the crash
to `dev logs` and keeps the last good state. **Behavior splits by context:**
- **Foreground UI (TTY):** prompt `host crashed — reload? [Y/n]`.
- **Detached daemon / non-TTY / `--no-input` (the default, since the daemon is
  daemonized and survives terminal close):** **no prompt** — auto-respawn with exponential
  backoff. There is frequently no TTY to answer, so prompting would hang. After a bounded
  number of failed respawns within a window, the daemon stops retrying, marks the host
  `crash-looping` in `dev status` (exit-code/log signal `host crash-looping` so CI can
  detect it), and waits for an explicit `dev reload`/`dev start`.

(VS Code isolated-EDH lesson: a crash should be consequence-free and recoverable, not
fatal.) Because the dev host is a separate, rackabel-owned process operating against the
User Library **copy**, a crash never touches the user's production Live set — an explicit
documented promise (VS Code isolated dev host). Note this isolation boundary covers the
host process only; it does **not** sandbox third-party rackabel hooks (§5.7).

### 3.6 Multi-Live-install handling

rackabel detects all installs, echoes each with version, and **persists the choice** rather
than re-prompting or requiring a repeated flag (web-clis `--persist-to` anti-pattern).
Resolution: `--live` → `[host].live` → `$ABLETON_APP` → newest detected (numbered pick-list
if ambiguous and interactive); the chosen Live + host path are recorded in `.rackabel/` and
echoed by `dev status`. The six `ABLETON_*` vars (`ABLETON_APP`, `ABLETON_USER_LIBRARY`,
`ABLETON_EH_MOD`, `ABLETON_EH_NODE`, `ABLETON_EXTENSIONS_DIR`, `ABLETON_STORAGE_BASE`) are
**conventions inherited from `dev-launch.sh`/`deploy-extension.js`** — the launcher rackabel
replaces — *not* official SDK env vars; rackabel honors all six and exposes each as a flag (§7)
for drop-in compat. The **official** CLI env var is `EXTENSION_HOST_PATH` (the host module),
also read and reconciled with `--eh-mod`. Storage dir =
`~/Library/Application Support/Ableton/Extension Data/<slug>`, temp = `/tmp/<slug>` —
matching the launcher so dev state carries over (§9.7).

### 3.7 Native dependencies without package-manager jargon

The default template is pure-JS, so a first-time musician never meets a native dep. But the
schema advertises native examples (`abletonlink`, `easymidi`), and the moment a user copies
one, the failure surface must **not** leak pnpm internals. **Decision: `rackabel deploy
--fix` / `doctor --fix` fully own the native build** — under the hood rackabel locates/uses
pnpm, runs `approve-builds`+`rebuild`, walks the dep graph, asserts the `.node`; the user
never types or sees a `pnpm` command. The only Persona-A-facing string is plain English:
*"this extension uses a compiled component that needs to be built — run `rackabel deploy
--fix`."* A bare `pnpm approve-builds` is **never** the primary instruction. Persona B reads
the raw command under `--verbose`/`--print-config` and can run it directly.

### 3.8 Headless / CI dev + test (no Live, no GUI)

The §3 reload loop needs Developer Mode, a GUI Live, and (today) macOS — none in CI. Persona
B's CI must still exercise `activate()`/commands without Live. `rackabel dev test` is that path
— scoped to **what the SDK actually ships, not an idealized turnkey harness** (this misled an
earlier draft). Verified facts:

- The SDK **source tree** has a `TestHarness`, used via **vitest** in the SDK's examples
  (`examples/{minimal,warpMode}/extension.test.ts`, `harness.activateExtension(activate)`) and
  documented in `docs/essentials/basics/3-testing.html`.
- **It is NOT importable from the published package:** `@ableton-extensions/sdk` 1.0.0-beta.0
  exports only `"."` — no `/testing` subpath, no testing module in `dist/`; the examples import it
  via `../../src/testing/index.js`, which resolves only inside the SDK repo. The testing doc even
  uses the **stale scope** `@ableton/extensions-sdk/testing` and is banner-marked
  *proof-of-concept, "will likely significantly change."*
- `build:headless`/`start:headless`/`runner.mjs` are **lidal's own scripts, not SDK-shipped**;
  lidal's `runner.mjs` hand-patches an older SDK (`apiVersion === "0.0.4"` hardcode;
  `MockActivationContext` env with only `userId`, no `storageDirectory`). Only **lidal** ships a
  headless **runner harness** (`build:headless`/`start:headless`/`runner.mjs`); the other
  members at most have plain vitest `test` scripts (groove-transplant and arclight-core define
  one; 3 of the 7 workspace *members* have a `test` script at all — and one of those is the
  shared library, not an extension) that need Live or are unit-only — i.e. no turnkey
  no-Live runner exists across the repo.

**Decision (scoped honestly): `rackabel dev test` runs the project's own vitest/TestHarness
tests where provided and shells out to any project-defined `*:headless` script when present** —
no Live, Developer Mode, daemon, or reload; builds each target (§2 pipeline, banner) and returns
deterministic exit codes (§7). With **no** harness and **no** tests it attempts only a
**best-effort generic smoke** `activate()`, explicitly *not* guaranteed (needs project-supplied
stubs, may hit the `apiVersion`/`MockActivationContext` gaps above).

**`dev test --json` envelope.** `--json` emits a **rackabel wrapper** object, never raw vitest
output: `{ targets: [{ name, harness_present, passed, failed, skipped_no_harness, exit_code }], passed, failed }`
(one entry per registered/selected target; `skipped_no_harness=true` for a target with no
tests/harness that only got the best-effort smoke). **Precedence:** rackabel's `--json` always
controls rackabel's own stdout; if the user *also* passes a vitest reporter via `-- --reporter=json`,
that reporter's output is forwarded to vitest and surfaced under `--raw`/per-target logs but
does **not** replace or merge into the wrapper envelope — so a CI script reading `dev test --json`
gets a stable rackabel shape regardless of runner flags.

So `dev test` (and `doctor`)
**report which registered extensions lack a headless harness** rather than implying all are
CI-ready, and `rackabel new` **scaffolds a vitest+TestHarness harness by default** so the
template is CI-testable out of the box. `rackabel dev` stays the Live-gated GUI loop; the testing
surface is officially unstable and may change at GA (§9.13).

---

## 4. Manifest & project model

### 4.1 `rackabel.toml` is the override surface (optional)

A project is **anchored** by `rackabel.toml` when present, or — when it is absent — by the
nearest `package.json` (the synthesized-project fallback). So `rackabel.toml` is **optional**:
the zero-config single-plugin watch path needs no manifest at all (`rackabel dev` in a
`package.json` dir just works). When present, the manifest is the single, hand-edited
**override** surface — it always wins; when absent, *everything* infers (Cargo
convention-over-config). The SDK's `manifest.json` is **generated** from the resolved project
on every build with a `do not edit` header; the user never hand-edits it (avoids the
Figma/Adobe "I edited the manifest and nothing changed" footgun). Fields are **all optional
with documented inference** — a fresh project builds with no manifest at all, or with an
essentially empty one (Wrangler v2; Cargo).

**Kind defaults to Extension for a manifestless project.** A synthesized project (anchored by
`package.json`, no `[extension]`/`[device]`/`[workspace]` table) is treated as an **Extension**
unless it opts into device: either `rackabel dev register --type device` (the kind is stored on
the registry entry) or a `package.json` `"rackabel": { "kind": "device" }` key. A *real*
`rackabel.toml` that declares **no** recognized table is still an error (`RK0002`) — that is a
genuine authoring mistake, distinct from a deliberately manifestless project. When neither a
`rackabel.toml` ancestor nor a `package.json` anchor is found, discovery still fails with
`RK0001` (its hint now mentions `package.json` as the alternative anchor). **Backward
compatibility is total:** every existing `rackabel.toml` project and every existing
`registry.toml` loads and behaves identically — the synthesized-project + default-kind paths
are purely additive, and `rackabel.toml` always wins when it is present.

### 4.2 Schema — both kinds, one file

```toml
# ---- An Extension (Live Extensions SDK v1.0) ----
[extension]
name = "Harmonic Lens"        # DISPLAY name in Live's menu. Optional; inferred from dir.
author = "Jane Doe"           # inferred from `git config user.name`
version = "0.3.0"             # semver; inferred 0.1.0
entry = "src/extension.ts"   # source entry; inferred from conventional layout
minimum_api_version = "1.0.0" # inferred from the installed SDK
# NOTE: the install slug is the project DIRECTORY basename, not `name` (launcher convention, §2 deploy).

[extension.build]
extra_dist_files = ["editor-client.js"]        # dist-relative basenames (was arclightExtraDistFiles)
native_deps = ["abletonlink", "easymidi"]      # externalized from bundle, copied to node_modules
# watch globs are DERIVED from entry + bundle graph; you do not list them separately.

[extension.pack]
targets = ["darwin-arm64", "darwin-x64"]       # was arclightPackTargets

# ---- A Max for Live device (existing scope) ----
[device]
name = "my-device"
kind = "audio-effect"          # audio-effect | midi-effect | instrument
entry = "src/my-device.maxpat"

# ---- Shared, optional ----
[host]
live = "/Applications/Ableton Live 12 Suite.app"   # overrides ABLETON_APP
user_library = "/Users/x/Music/Ableton/User Library"

[toolchain]
# TWO floors, not one (§0):
#  - RUNTIME floor doctor enforces against Live's BUNDLED node = SDK/CLI engines (>=22.11.0).
#    This is the only floor the dev loop actually needs; don't fail Persona A on more.
#  - BUILD-TIME floor for tooling that emits a project = the node create-extension BAKES INTO
#    the generated project (package.json.ejs: engines.node >=24.14.1, @types/node ^24.1.0).
#    create-extension's OWN engines is also >=22.11.0 — the 24.14.1 is what it EMITS, not requires.
node_runtime = ">=22.11.0"   # FLOOR doctor checks Live's bundled node against (NOT "install Node" — §0)
node_build   = ">=24.14.1"   # floor for build-time tooling / generated project; not gated on the dev loop
sdk = "1.0"                   # SDK contract version; doctor flags mismatch

[meta]                         # future SDK-IMPROVEMENTS metadata, carried ahead of the SDK
description = "..."
homepage = "..."
license = "MIT"
categories = ["utility"]
```

A project that *declares* a kind has **exactly one** of `[extension]` or `[device]` (a
workspace can hold many single-kind projects — §4.4); a manifestless (synthesized) project
declares neither and **defaults to Extension** (§4.1: device opts in via `register --type
device` or a `package.json` `"rackabel": { "kind": "device" }` key). Every table shown above is
optional — a project with no `rackabel.toml` at all is valid, with all fields inferred; the
manifest exists only to **override** an inferred value or carry the `[extension.build]`/`pack`/
`[meta]` knobs that have no inference. This formalizes the conventions that today live in
`package.json` under `arclight*` keys (`arclightNativeDeps`, `arclightExtraDistFiles`,
`arclightPackTargets`) and carries forward the SDK-IMPROVEMENTS wishlist fields under
`[meta]` so rackabel can emit them into `manifest.json` the moment the SDK honors them
(maximumApiVersion, icon, repository, keywords, etc.) without a schema break.

**Migration note for `extra_dist_files`.** The old `arclightExtraDistFiles` entries are
**dist-relative basenames** (e.g. `"editor-client.js"`, meaning `dist/editor-client.js`);
rackabel's `extra_dist_files` keeps that exact shape, so porting an existing manifest is a
mechanical copy of the basenames (no path rewriting).

**Migration note for `pack.targets`.** `arclightPackTargets` was an array of
`{platform, arch}` **objects** (e.g. `[{platform:"darwin",arch:"arm64"}, …]`, consumed by
`pack-extension.js` as `target.platform`/`target.arch`); rackabel's `pack.targets` uses
hyphenated **os-arch strings** (`"darwin"`+`"arm64"` → `"darwin-arm64"`). Porting requires
rewriting each object to its hyphenated string — copying the object form verbatim is a parse
error. **Output-filename change, too:** the grounding packer emitted
`<slug>-v<version>-<platform>-<arch>.zip`; rackabel changes **both** the extension
(`.zip` → `.ablx`) **and** the segment vocabulary, producing
`<slug>-v<version>-<os>-<arch>.ablx`. A CI script keyed on the old `.zip` filename must be
updated for the new extension and naming.

### 4.3 Managed vs user-owned keys

Hard line between author-authored and tool-computed fields (Max `package-info.json`
`c74install`/`installdate` discipline). Computed values (resolved host path, build hash,
install timestamp, last-packed version for drift detection, registry id) live in a
**sidecar `.rackabel/state.toml` / `rackabel.lock`**, never injected back into
`rackabel.toml`, so hand-editing the manifest never fights the tool.

### 4.4 Monorepo / workspace

A `[workspace]` root manifest lists member globs; `register --recursive` over the
workspace root registers each member. This is the `register-parent` case, expressed as
convention + one flag rather than parallel verbs. Both nested and **flat** layouts are
supported (the grounding monorepo is flat — each extension is a top-level dir, listed
individually in `pnpm-workspace.yaml`):

```toml
# nested layout
[workspace]
members = ["packages/*"]

# flat layout (each extension a top-level dir)
[workspace]
members = ["harmonic-lens", "groove-transplant", "clip-renamer"]
```

**One resolution path, not two.** `register --recursive` and `[workspace].members` are
reconciled: if a `[workspace]` manifest exists at the root, `register --recursive` uses its
`members` globs as the authoritative member list; if there is **no** `[workspace]`, it
falls back to scanning subdirs for manifests. So a workspace's `members` is the single
source of which dirs are members; the manifest-scan is only the no-workspace fallback.

**Two kinds of workspace member: extensions and shared libraries.** Not every workspace
member is a deployable extension. The grounding monorepo
(`~/Projects/ableton-extensions-public`) contains `@arclight/core` — a member with **no
`manifest.json`** that 4 of the 6 extensions depend on via `workspace:*` (7 workspace members
total: 6 extensions plus this one shared library). rackabel
distinguishes them by the manifest:

- **Extension members** bear a `rackabel.toml [extension]` (or a `manifest.json`): they
  register, deploy, and load into the host.
- **Library members** are workspace members that are a `workspace:*` dependency of an
  extension and have **no** manifest: they are **never** registered/deployed/loaded on their
  own. They exist only as build inputs to the extensions that consume them. `register
  --recursive` **skips** them (no manifest ⇒ not an entry), and `dev list` never shows them.

Each extension's esbuild bundles a `workspace:*` library from the library's **prebuilt
`dist/`** (it resolves e.g. `@arclight/core/dist/index.js` through the pnpm symlink), **not**
from the library's TypeScript source — which is why a library must be built **before** any
dependent extension. This is the dependency relationship the watch model (§3.3) and build
order (below) must honor; ignoring it is exactly how "edit shared code, nothing reloads"
happens.

`build`/`deploy`/`pack` at the workspace root operate over all members in **dependency
order: rebuild each changed internal `workspace:*` library first (e.g. `tsc --build` /
`pnpm --filter <lib> build`), then rebuild the dependents.** (This replaces the earlier
vague "core-first, mirroring the SDK's `build:all`": `build:all` is the **monorepo root**
script, not an SDK script, and the real invariant is "deps before dependents," not a single
named core.) `dev` watches all registered extension members; `dev <NAME…>`/`--only` scopes
to a working subset (§3.3).

### 4.5 Relationship to the SDK's `manifest.json`

rackabel **generates and validates** it; the user never maintains it. On `build` it writes
`manifest.json` (name/author/entry=`dist/extension.js`/version/minimumApiVersion) from
`[extension]`; `validate`/`doctor` check it. Because deploy alone makes an extension usable in
a production Live (Live spawns its own host), the generated `manifest.json` + bundle is a
self-contained artifact that works without rackabel running — rackabel is required only for
the *dev* loop (§9.4: deploy-alone usability strongly indicated, gated there).

### 4.6 The polyfill banner (a rackabel value-add, not an SDK rule)

rackabel's `build` bakes a polyfill banner (URL, URLSearchParams, TextEncoder/Decoder,
atob/btoa, Request/Response/Headers, the stream classes, setImmediate, performance). The
**official** `build.ts` ships **no** banner and the SDK docs never mention one — so it is not
an SDK requirement. But the 1.0 host VM still lacks these globals (verified in the running
host), so code touching them throws runtime `ReferenceError`s invisible at build time.
rackabel adds the banner the template omits — which is exactly why `doctor` reports the
"forgotten polyfill banner" footgun as *impossible* (§6.3): baked into rackabel's build,
never the user's config.

### 4.7 Relationship to `create-extension` and the official CLI

The official scaffolder `create-extension` (`npm create @ableton-extensions/extension`,
beta tarball) already does much of what `rackabel new` needs: it **vendors** the SDK+CLI
tarballs and emits `manifest.json`, `package.json` (`start`/`package` scripts), `build.ts`,
`.env` (`EXTENSION_HOST_PATH`), `src/extension.ts`, `ui/interface.html`, and
`.vscode/launch.json`+`tasks.json`.

**Decision: `rackabel new` reuses/extends create-extension, not reimplements it.** When
available, rackabel shells out and **post-processes**: derive `rackabel.toml` from the
emitted `manifest.json`/`package.json`; replace `build.ts` with rackabel's pipeline (banner,
§4.6); swap in rackabel's default template; keep the vendored tarballs. When **not**
available, rackabel uses its own forked equivalents — a conscious fork, because the SDK is
beta-gated and rackabel must work the moment the tarballs are present even if the scaffolder
package isn't. So §4's "vendor the tarballs" and §7's `--emit-launch-config` are
*reuse/extend the official tooling*. Likewise `pack` shells out to `extensions-cli package`
for pure-JS (§2), and `extensions-cli run` is the one-off behind `rackabel dev PATH` but
cannot own the reload loop (§3.1).

---

## 5. Extensibility — rackabel's own plugin model

The maintainer requires rackabel to be **extensible by third parties without changes to
core** — the explicit failure to avoid is norns' original "recompile C and send a PR to
core" model.

### 5.1 Primary mechanism: PATH-convention subcommands

**Decision: the primary plugin mechanism is git/cargo-style external subcommands** —
`rackabel <foo>` resolves to an executable `rackabel-foo`, searching
`~/.rackabel/plugins/bin` **first**, then `$PATH`. No SDK, no ABI, no in-process API.

**Built-ins always win (reserved namespace).** External lookup happens **only** for tokens
that do not match a built-in subcommand (§2). A built-in can never be shadowed by a PATH
executable (git/cargo behavior), so a plugin can never hijack the flagship flag-free
`rackabel dev`. The two-location search order above (`plugins/bin` first, then `$PATH`) is
the rule **among external candidates only**. To avoid silent surprises across that
boundary, when an invoked external name is resolvable from **more than one** location
rackabel emits a **one-time warning** ("`rackabel-foo` found in both ~/.rackabel/plugins/bin
and $PATH; using the managed one — see `rackabel plugin which foo`"), applying the
cargo-#6507 proactive-surfacing lesson rather than leaving it to an on-demand command. The
full reserved set and the future-built-in-collision policy are in §5.6.

Rationale (cli-extensibility research): git and cargo sustained huge plugin ecosystems for a
decade on argv + exit codes with essentially **zero API churn**, because the "API" is just
the process boundary. rackabel's plugin authors are TS developers; a `#!/usr/bin/env node`
script named `rackabel-foo` made executable *just works*. This is the lowest-coupling option
most likely to survive rackabel's evolution; a bespoke in-process hook API on day one is the
thing most likely to break the ecosystem (ESLint v9: a versioned plugin API is a liability
you must then never break).

### 5.2 The contract plugins get

Before exec, rackabel sets a language-agnostic, versioned env contract and forwards all
trailing args verbatim:

```
RACKABEL              abs path to the running rackabel binary (call back without re-resolving)
RACKABEL_PLUGIN_API   the SUBCOMMAND/env-contract version integer (currently 1) — tier 2
RACKABEL_MANIFEST     abs path to the project rackabel.toml (if in a project)
RACKABEL_PROJECT_DIR  abs project root
RACKABEL_REGISTRY     abs path to ~/.rackabel/registry.toml
RACKABEL_VERSION      rackabel's version (0.x scheme — see versioning note in the header)
```

**Presence rule (commit unset, not empty).** `RACKABEL`, `RACKABEL_VERSION`,
`RACKABEL_PLUGIN_API`, and `RACKABEL_REGISTRY` are **always set**. `RACKABEL_MANIFEST` and
`RACKABEL_PROJECT_DIR` are **UNSET (not empty-string)** when rackabel is invoked outside a
project (e.g. `rackabel foo` from `~`). A plugin tests presence, never an empty string —
removing the empty-vs-unset ambiguity that commonly breaks plugins.

Exit codes are the success/failure signal. `rackabel plugin which <name>` shows exactly
which file would run (pre-empts cargo's ambiguous-shadowing pain, issue #6507).
`RACKABEL` is always set to the absolute path of the *currently running* binary,
overwriting any inherited value (cargo `CARGO`-points-at-wrong-binary bug, #15099).

**Two version contracts, versioned independently — do not conflate.** `RACKABEL_PLUGIN_API`
(this env integer) governs **tier-2 PATH subcommands** (the env/subcommand surface); the
**tier-3 hook** surface (§5.3) has its own integer `RACKABEL_HOOK_API` (env) with a matching
`hook_api` key in `rackabel-plugin.toml`. They share no number and evolve separately —
different surfaces (a runtime-only env read vs a manifest-declared, codemoddable contract).

**Tier-2 needs no migration: the env contract is additive-only.** A PATH subcommand has no
manifest and declares no version, so rackabel can't gate or codemod it the way it can a tier-3
hook. **Tier-2 avoids migrations entirely** because the env contract is committed in writing to
be additive-only — bumping `RACKABEL_PLUGIN_API` only ever *adds* vars; a v1 var is never
removed or repurposed. So a v1 plugin works unchanged forever; "what happens at v2?" → nothing
breaks.

**What `RACKABEL_PLUGIN_API` is for (a hint, never a gate).** Because the contract is
additive-only, a plugin **never needs** the integer to keep working — every var it already
reads is guaranteed present forever, so presence-testing (the unset rule above) is the only
mechanism it *requires*. The integer therefore has exactly **one documented job: it is the
signal that newer optional vars exist.** Bumping it is the *only* way rackabel announces "a new
var was added"; a plugin **MAY** read it to decide whether a newer optional var is worth probing
(e.g. "if `RACKABEL_PLUGIN_API >= 2`, look for `RACKABEL_FOO`"), but **MUST still presence-test
that var** rather than assuming it from the integer. It is a hint, never a gate — a plugin that
ignores it entirely and just presence-tests is fully correct. This resolves the apparent
contradiction: the integer is informational *and* useful, because "informational" here means
"tells you new vars may exist," not "carries no signal." (Tier-3 hooks *can* declare `hook_api`
and be migrated, §5.3; tier 2 trades migrations for this immutable-additive contract.)

### 5.3 Hooks for the built-in lifecycle (secondary, opt-in)

PATH subcommands add *new* commands but cannot hook *existing* phases (build/deploy/
doctor). For that, a plugin's `rackabel-plugin.toml` may declare named lifecycle hooks
that rackabel invokes as subprocesses at documented points (norns mods model — a stable
named-hook contract, disabled-by-default, can never brick core `build`):

```toml
# rackabel-plugin.toml
name = "rackabel-plugin-notarize"
hook_api = 1                                  # the TIER-3 hook contract version (own number)
requires = { rackabel = ">=0.5", node = ">=20" }   # 0.x scheme; hooks land in 0.5

[hooks]
post_build  = "bin/post-build"      # invoked after a successful build
pre_deploy  = "bin/pre-deploy"
doctor_check = "bin/check"          # contributes a doctor line
new_template = "bin/template"       # contributes a `new`-wizard template CHOICE
on_reload   = "bin/on-reload"       # invoked after a dev reload
```

Hooks receive the same env contract (plus `RACKABEL_HOOK_API`) and a per-hook JSON payload
on stdin; their stdout/exit-code contract is defined per hook below. They are **disabled by
default**, enabled via `rackabel plugin enable <name>`, and a crashing hook is logged and
skipped — it never aborts `build` (norns "disabled by default + explicit enable" so a broken
third-party plugin never bricks core). The hook contract carries its own `hook_api` integer
(§5.2), **separate from rackabel's product version and from the tier-2 env contract**,
shipped one change at a time with a `rackabel plugin migrate` codemod and the migration URL
embedded in the runtime error (ESLint v9 lesson: never batch breaking plugin-contract
changes; tooling, not docs, drives adoption).

**Per-hook I/O contract** (mirrors the §5.2 env-table rigor). Every hook gets the env
contract on its environment and a JSON object on **stdin**; interpretation of **stdout** and
**exit code** is per hook:

| Hook | stdin JSON | stdout contract | exit code |
|---|---|---|---|
| `post_build` | `{project_dir, manifest_toml, bundle_path?, build_hash, kind, release}` | ignored (informational logging only) | nonzero = logged + skipped; never aborts build |
| `pre_deploy` | `{project_dir, manifest_toml, bundle_path, user_library, slug}` | ignored | **nonzero ABORTS the deploy** (this is the one hook allowed to veto, e.g. a notarize gate); message surfaced via §6.1 framing |
| `on_reload` | `{project_dir, manifest_toml, name, reload_ms, ok}` | ignored | nonzero = logged + skipped |
| `doctor_check` | `{project_dir?, manifest_toml?}` — **both null/absent when doctor runs outside a project** | **one JSON line** `{symbol:"ok"\|"warn"\|"fail", message, help}` (or empty stdout) — **authoritative when present**, see precedence rule below | consulted **only when stdout has no contract line**: `0` ⇒ pass, nonzero ⇒ generic fail row (see precedence rule below) |
| `new_template` (enumerate) | `{kind}` only — **no `wizard_answers`, no `project_dir`** (neither exists yet) | **one line**: an absolute path to a template dir **or** a `gh:owner/repo[@ref]` ref, used to add a CHOICE to the wizard's template list | nonzero = the choice is omitted (logged) |

**Payload field types** (held to the same rigor as the §5.2 env table). `project_dir`,
`bundle_path`, and `user_library` are **absolute path strings**; `slug`, `name`, `kind`, and
`build_hash` are **strings**; `release` and `ok` are **booleans**; `reload_ms` is a **number**.
`manifest_toml` is the project's `rackabel.toml` **parsed and rendered as a JSON object** (not
a path), so a hook author parses nothing — its keys mirror §4.2 (`extension`, `host`,
`toolchain`, `meta`, …). (A hook that also needs the *generated* SDK `manifest.json` can read
`RACKABEL_PROJECT_DIR` + `dist/manifest.json`; the abs path to `rackabel.toml` is still in the
env as `RACKABEL_MANIFEST`, §5.2 — `manifest_toml` is the parsed object, deliberately a
*different thing* from that path, hence the rename from the old ambiguous `manifest`.)

**Unset/optional rule** (mirrors §5.2's commit-unset-not-empty). A field that has no value in
a given context is **omitted** from the stdin object (a JSON-`null` is treated identically),
never sent as an empty string: `bundle_path` is **absent** when a build was skipped;
`project_dir`/`manifest_toml` are **absent** when `doctor_check` runs outside any project (so a
`doctor_check` hook **must tolerate a no-project payload** — `rackabel doctor` is an
environment command that runs outside a project, §5.2/§6.2). A hook tests presence, never an
empty string.

stdout that does not match the contract for a hook is treated as informational log output,
not data; only the documented shapes above are parsed.

**Hangs are handled, not just crashes.** Every hook subprocess runs under a **wall-clock
timeout** — default **30s**, overridable per hook (`[hooks.timeouts] post_build = 120000`
(ms) in `rackabel-plugin.toml`, and the same table next to a project-local `[hooks]`). On
timeout rackabel sends SIGTERM, then SIGKILL after a 5s grace, and treats the hook **exactly
like a nonzero exit**: logged + skipped for `post_build`/`on_reload`/`doctor_check`/
`new_template`, and an **abort** for `pre_deploy` — a hanging veto hook can never block a
deploy indefinitely; the deploy fails fast with the §6.1 framing naming the hook and its
timeout. **stdin framing guarantees EOF:** rackabel writes exactly one JSON object and
**closes stdin**, so a hook that reads to EOF terminates naturally; a hook that blocks
waiting for more input hits the timeout. This matters most in the dev loop, where an enabled
`post_build`/`on_reload` hook runs on **every save** (§5.7).

**`new_template` is a single-phase enumerate hook, run before any project exists.** rackabel
invokes it **before** the wizard's template prompt (so its returned template can appear as a
choice), passing only `{kind}` — no `wizard_answers`, no `project_dir`, because the user hasn't
answered and nothing is scaffolded yet. It runs **once per `new`** to contribute its choice; if
picked, rackabel renders it with ordinary tier-1 machinery (§5.5) — **no second call**.
**Enumerate hooks honor the same enable gate as every other hook:** a freshly-installed but
**not-yet-`plugin enable`d** plugin's `new_template` does **not** run, so it contributes **no**
wizard choice — keeping "enabling is the consent" (§5.7) uniform across all hook kinds (an
unenabled plugin influences nothing, not even the `new` wizard). (An
earlier draft passed `{wizard_answers, project_dir}`, a chicken-and-egg contradiction: a hook
that adds one of the wizard's *own* options can't receive answers/a dir that exist only *after*
the wizard runs.)

**`doctor_check` precedence: one authoritative channel.** The **stdout JSON line wins whenever
present and well-formed** — its `symbol`/`message`/`help` drive the row regardless of exit code.
The exit code is consulted **only when stdout has no parseable line** (`0` ⇒ pass; nonzero ⇒ a
generic `doctor_check <name> failed` row). The four combinations are thus deterministic: (a)
exit 0 + line ⇒ line wins; (b) nonzero + line ⇒ line wins (a script that emits a valid row then
crashes still shows it; authors can deliberately show a `fail`/`warn` row while exiting nonzero);
(c) exit 0 + no line ⇒ pass; (d) nonzero + no line ⇒ generic fail. A **timeout** (§5.3) has
produced no contract line by definition, so it is combination (d): the generic fail row.
Output stays consistent across
plugins even when this casually-written read-only hook bug-exits.

### 5.4 Discovery, install, list

```
rackabel plugin install OWNER/REPO [--yes] [--json]   gh-style: prefer release asset
                                      rackabel-<name>-<os>-<arch>, else clone + run
rackabel plugin install <path|tarball> [--yes] [--json]   sideload (always works, no gatekeeper)
rackabel plugin list [--json]         installed plugins + enabled state + pinned ref
rackabel plugin which <name> [--json] which file would run (or "shadowed by built-in")
rackabel plugin run <name> [args…]    run a plugin EVEN IF a built-in shadows the name (§5.6)
rackabel plugin enable|disable <name>
rackabel plugin search <term> [--json]   queries the `rackabel-plugin` GitHub topic
```

`plugin install` places the resolved executable — the downloaded release asset, or the build
output of a clone — **and a symlink to it under `~/.rackabel/plugins/bin`**, which is exactly
the "managed" location §5.1 searches **first** (before `$PATH`) and what the §5.1/§5.6
both-locations warning ("using the managed one") distinguishes. `plugins.lock` pins **that
managed entry** by commit/sha256. This closes the loop: a `plugin install`ed plugin is the
managed copy, a `rackabel-foo` the user dropped on `$PATH` themselves is the unmanaged one, and
the search order + warning are what disambiguate them.

Install prints what it will run and where, requires confirmation (`--yes` to script),
pins by commit/sha256 in `~/.rackabel/plugins.lock`, and **never auto-updates silently**
(supply-chain lessons: the lockfile is authoritative; `--force` past a pin announces it,
gh pin-bug #13551). A pin mismatch at install/verify time is a **validation failure (exit
`4`)** so CI can gate on it deterministically; `plugin list --json`/`plugin which --json` are
the machine-readable state surface (§7), and `--yes`/`--no-input` make `plugin install`
scriptable like every other command. Discovery is **never gated** on maintainer review — a GitHub topic +
sideloading work from day one; an optional curated/"verified" index is additive, not a
publish gate (krew bottleneck anti-pattern). We deliberately **do not** become an npm:
plugins bundle/vendor their own JS; rackabel verifies and symlinks, it does not resolve
dependency trees (krew "we are not a package manager" scope discipline).

### 5.5 Templates: the low-bar extensibility tier

The lowest-effort extensibility tier is **templates**, resolved on-demand with nothing
pre-installed (`npm create` model, which displaced global-install Yeoman):

```
rackabel new --template gh:user/repo
rackabel new --template @scope/name
rackabel new --template ./local/dir
```

A template is a git repo with a `rackabel-template.toml` declaring `[prompts]`
(name/type/default/choices) plus files containing placeholders; rackabel renders the
prompts as the `new` wizard. A **remote** template ref (`gh:`/`@scope`) is third-party code:
before fetching, rackabel prints the resolved repo/ref and warns that `new`'s auto-build will
execute the template's build configuration, requiring confirmation (`--yes` in scripts);
local paths and the built-in default skip the prompt (threat model: §5.7). Templates are **declarative data, never dependent on rackabel
internals**, so they don't bit-rot when rackabel changes (Yeoman generator decline).

**`new --update` (copier-style 3-way merge) needs the original answers, so it persists
them.** Re-rendering the old baseline is impossible from `repo+commit` alone — you also need
the prompt **answers** (copier persists an answers file for exactly this reason).
**Decision: the `.rackabel-template` lockfile persists `repo + commit + rendered answers`.**
`new --update` then 3-way-merges: **old-render** (template@oldcommit + saved answers) is the
base, **new-render** (template@newcommit + the same answers, re-prompting only for *new*
prompts) is theirs, **user-tree** is ours. **Conflict UX:** conflicting files get conflict
markers and a summary `help:` line; clean files apply silently. **Vendored SDK/CLI tarballs and
other binary/generated files are EXCLUDED from the 3-way text merge** (they differ by SDK
version, not template commit, and a marker-based merge can't reconcile binary bytes): they are
overwritten from the new render or left to the user, per a declared `[merge].exclude` glob in
`rackabel-template.toml`, so the merge only ever operates on author-editable text. `--update` is **never run on
the Persona-A happy path** and never unattended — an explicit developer action, so it can't
silently clobber a musician's setup ("you cannot break your setup", §1). This is the
SDK-churn bit-rot mitigation (copier `update`; CRA frozen-snapshot anti-pattern).

**Tier-1 templates vs the tier-3 `new_template` hook** are different tools: a tier-1 template
is a standalone scaffold the user names explicitly (`new --template gh:…`), nothing
pre-installed; a tier-3 `new_template` hook belongs to an *already installed* plugin and
**injects an extra CHOICE into the `new` wizard's template list**. It is an **enumerate** hook
(§5.3): rackabel runs it **before** the template prompt with only `{kind}`, the hook prints the
path/ref of the template it wants to offer, and that becomes a wizard choice; if picked, it
renders through ordinary tier-1 machinery (no second call, and it runs before `project_dir`
exists). E.g. a company's notarize plugin also offering its house starter. Tier 1 is the
musician's path; a plugin that just wants to ship a scaffold publishes a tier-1 template repo
instead.

**Project-local hooks without plugin ceremony.** A one-off `post_build` in your *own* repo
shouldn't need a `rackabel-plugin.toml` + `plugin enable` (that ceremony is for *installed
third-party* plugins). **Decision: a project's own `rackabel.toml` may declare a `[hooks]`
table directly** pointing at repo scripts (e.g. `.rackabel/hooks/post_build`); these run with
**no manifest and no enable step** because it's the user's own code (trust is implicit).

```toml
# in the project's own rackabel.toml — local hooks, no plugin, no enable
[hooks]
post_build = ".rackabel/hooks/post-build"
```

**Tiering summary:** templates (no code, on-demand) → PATH subcommands (any executable,
zero API) → lifecycle hooks (manifest + named hooks, versioned; or local `[hooks]` with no
manifest). A musician consumes tier 1; a developer ships tier 2 in an afternoon; only deep
*third-party* integrations need the full tier-3 manifest.

### 5.6 Reserved namespace & future built-ins

Because built-ins always win (§5.1), rackabel must manage the namespace honestly:

- **Reserved set.** The current top-level verbs (`new`, `build`, `deploy`, `pack`,
  `validate`, `doctor`, `dev`, `plugin`, `explain`, `install`) are reserved for core; a
  `rackabel-<reserved>` on PATH never runs as a bare subcommand. The list is published and
  grows only deliberately.
- **Future-built-in collisions.** When a new release adds a built-in whose name an installed
  plugin already provides (the textbook case is the planned `publish`/`login`, §8 — exactly
  the names a third party would ship as `rackabel-publish`), rackabel **detects it on
  upgrade and warns loudly**: `built-in 'publish' now shadows your plugin rackabel-publish;
  invoke it as 'rackabel plugin run publish' or rename the plugin`. The plugin is **never
  silently dropped**.
- **Escape hatch.** `rackabel plugin run <name>` always invokes the plugin executable even
  when a built-in claims the name, so a shadowed plugin stays reachable. It uses the **same
  external-candidate order as §5.1** — `~/.rackabel/plugins/bin` first, then `$PATH` — and emits
  the same one-time both-locations warning, so the escape hatch is itself deterministic.
  `plugin which` reports `shadowed by built-in` and points here.

This applies the cargo-#6507 shadowing lesson the doc already cites: surface the collision
proactively, keep both reachable, never silently change behavior under the user.

### 5.7 Security posture & threat model (stated plainly)

The install-time posture (§5.4) is strong, but the **runtime** model must be stated without
euphemism:

- **Tier-2 subcommands and tier-3 hooks are unsandboxed native processes with the user's
  full privileges** — a `rackabel-foo` or a `post_build` hook can do anything the user can.
- **Enabling a hook authorizes execution on every relevant lifecycle event** — a
  `post_build`/`on_reload` hook enabled for `rackabel dev` runs **on every save**. Enabling
  is the consent; there is no per-invocation prompt.
- **A hung hook is a denial-of-service on the loop it runs in** — an enabled
  `post_build`/`on_reload` hook that simply never exits would freeze every save, and a
  hanging `pre_deploy` would block deploys indefinitely. The per-hook timeout (§5.3) is the
  mitigation: timeout ⇒ treated as hook failure, the loop continues (or the deploy aborts
  fast) — the trivial "just don't exit" attack is bounded, not ignored.
- **A remote template is unreviewed third-party code, and `new` builds what it scaffolds.**
  `new --template gh:…`/`@scope/…` fetches an arbitrary repo whose build configuration the
  auto-build then executes with the user's full privileges — scaffold-time code execution
  aimed at the least-defended audience. Mitigation: when the template source is **remote**
  (not the built-in default and not a local path), rackabel prints the resolved repo/ref and
  what will be fetched **and built**, and requires confirmation before rendering (`--yes` to
  script), mirroring `plugin install`. The Persona-A no-flag happy path uses only the
  **built-in** template and never sees this prompt.
- **`plugin install OWNER/REPO` ("clone + run") runs unreviewed code on first install.** The
  confirmation prompt and sha256/commit pin protect against *tampering and silent updates*,
  **not** against malicious-but-pinned code — pinning keeps you on the *same* code, not safe
  code.
- **Changing a hook plugin's pinned code does NOT silently retain enablement.** When a hook
  plugin's pinned code changes (`--force` past a pin, or a reinstall at the same enabled name),
  rackabel **disables the hook and requires an explicit `plugin enable` re-confirmation** — so
  new code never runs on-save under consent given for the old code. Enabling is standing consent
  *for the code you reviewed*, not for whatever later replaces it.
- **rackabel's one isolation boundary does not extend to hooks.** The dev host runs against
  the User Library *copy* (§3.5), but that boundary is around the host *process*, not around
  hooks, which run in rackabel's context with full privileges.

Real sandboxing is deferred to the §8 WASM capability-tier; until then the trust model is
"you are running code you chose to install," stated up front. Local project `[hooks]` (§5.5)
are the user's own repo and carry the same implicit trust as any script they wrote.

---

## 6. Error/log UX & onboarding

### 6.1 Error-message style guide

Every rackabel error has a **three-part shape** (rustc/Elm "compiler as assistant";
RFC 1644): plain-English problem, the offending value shown, and a literal `help:` next
action. Never a question ("did you mean?"), never a raw traceback as primary output, never
warning-fatigue (quiet on success).

```
error: no manifest found
  --> looked for rackabel.toml in /Users/x/proj and its parents
  help: run `rackabel new` to scaffold one, or cd into a project directory
```

```
error: the Extension failed to load
  --> harmonic-lens — TypeError: cannot read 'tracks' of undefined
       at src/extension.ts:14:22        (mapped from dist/extension.js via sourcemap)
  help: open src/extension.ts:14 — `application.song` may be undefined before activate.
        full host output: rackabel dev logs harmonic-lens --raw
```

Rules: short inline message; longer prose lives behind `rackabel explain <code>`
(cargo `--explain`). Wrap unknown host failures rather than dumping them
(`the Extension Host crashed — see `rackabel dev logs --raw`; run `rackabel doctor``).
Raw Node/V8 stack traces are gated behind `--raw`/`--verbose`. In dev, where `$EDITOR`/
`code --goto file:line` is available, the framed error offers an open-in-editor affordance
(Raycast). For installed (non-dev) extensions, author console output is **quiet by
default** unless `--verbose` (Raycast: store extensions log quietly; Figma iframe-noise
complaint).

### 6.2 First-five-minutes — a musician transcript

**The most likely first run: the SDK download isn't there yet.** The SDK is a separate,
beta-gated file from Ableton (not on public npm, §9.9), so a brand-new user has almost
certainly not downloaded it. That path must not dead-end — here it is verbatim. (The beta URL
shown is a **placeholder to confirm at ship**, and rackabel sources it from
remote/updatable config — **not** a hard-coded constant — so a moved page can be corrected
without a rackabel release; the transcript also gives a search fallback so a 404 never
re-creates the dead-end this branch exists to prevent.)

```
$ rackabel new
? What are you building?  › Live Extension   (↑/↓, Enter)
? Name › clip-renamer
? Author › Jane Doe                          [from git config — Enter to accept]
? Template › Default (a working right-click action) [Enter]
  Looking for the Ableton Extensions toolkit…
✗ Couldn't find the Ableton Extensions toolkit download.
  It's a separate file from Ableton, only available if you've joined the
  Extensions beta. Access is granted by Ableton and may not be instant —
  once you have the toolkit file, come back and run the command below.
  Here's how to get it:
    1. Join / open the beta at:  https://www.ableton.com/extensions-beta
       (if that page has moved, search Ableton's site for "Extensions beta".
        if you've just requested access, you may need to wait for approval.)
    2. Download the toolkit file it gives you (it ends in .tgz).
    3. Put it (or its folder) anywhere easy, e.g. your Downloads folder.
       The .tgz, or an unzipped folder, either works — I'll find it.
  Then run this once, pointing at where you saved it:
    rackabel new clip-renamer --sdk-dir ~/Downloads
  (or just run `rackabel new` again and pick the file when asked.)
  Already downloaded it but still seeing this? Run `rackabel new` again
  and choose "find the toolkit file myself" to point me straight at it.
  Your answers above are remembered — nothing was lost.
```

Once the toolkit is in place, the happy path:

```
$ rackabel new clip-renamer --sdk-dir ~/Downloads
  Looking for the Ableton Extensions toolkit…
  ✓ found the Ableton Extensions toolkit in ~/Downloads
  ✓ added it to clip-renamer/ (no internet or npm needed)
✓ created clip-renamer/   ✓ built your extension (84ms)
  next:  cd clip-renamer && rackabel dev
```

**Aside — if Live isn't installed yet** (so there's no bundled node and, say, no PATH node
either), `new` does **not** dead-end on the auto-build — it creates the project, skips the
build, and says exactly how to resume:

```
$ rackabel new clip-renamer --sdk-dir ~/Downloads
  ✓ found the Ableton Extensions toolkit in ~/Downloads
  ✓ added it to clip-renamer/ (no internet or npm needed)
✓ created clip-renamer/
  (skipped the build — I couldn't find Ableton Live or a Node runtime yet.)
  next:  install Live Suite 12.4.5+ and enable the Extensions beta, then run
         `rackabel doctor` from inside clip-renamer/ — build and run happen
         once Live is present. Get Live: https://www.ableton.com/live (Suite).
```

*(End of aside.)* **Back on the main path** — Live is installed and the project was created
above; check the environment:

```
$ cd clip-renamer
$ rackabel doctor
[✓] Ableton Live 12.4.5 Suite (beta) — Extensions supported (Apple Silicon)
[✓] Live's Extension components found
[✓] Live's components are compatible
[!] Developer Mode is OFF — the dev loop (live reload) can't run without it
    help: open Live → Settings → Extensions → turn on Developer Mode
          (it appears once you've joined the Extensions beta), then rerun
          `rackabel doctor` — or just run `rackabel dev`, which waits for it.
[✓] User Library: ~/Music/Ableton/User Library
[✓] Extensions toolkit ready
5/6 checks passed, 1 warning
   (run `rackabel doctor --verbose` for version/ABI details)

  …user turns on Developer Mode in Live…
```

The same screen stays friendly on the most demoralizing first runs. **No Live installed:**

```
$ rackabel doctor
[✗] No Ableton Live install found
    help: install Live Suite 12.4.5+ and turn on the Extensions beta
          (Live → Settings → … → Beta), then rerun `rackabel doctor`.
0/6 checks passed — 1 thing to fix (the remaining checks can't run until Live is found)
```

**On an Intel Mac**, the arch line still reads as success — Rosetta is not an error:

```
[✓] Ableton Live 12.4.5 Suite (beta) — Extensions supported (Intel, via Rosetta)
```

```
  …Live is open (the dev loop talks to a running Live) and Developer Mode is on…

$ rackabel dev
  Installs a working copy of just clip-renamer into Live's User Library —
  your saved Live Sets are untouched, and `rackabel deploy --undo` removes it.
  ✓ connected to Live   ✓ installed clip-renamer into Live's User Library
  ● live  clip-renamer  build OK  watching your source files
  clip-renamer v0.1.0 active — 1 command, 1 right-click action
  find it in Live: right-click any clip → "Rename selected clips"
  keys: [l] logs   [q] quit
        [r] force a reload now   (reloads happen automatically when you save)

  …in Live: right-click a clip → "Rename selected clips" → it works…

  [edit your extension, save]
  ✓ rebuilt (61ms) → updated in Live (9ms) → reloaded clip-renamer
```

To fully undo everything later: `rackabel dev unregister clip-renamer` stops watching it
and `rackabel deploy --undo` removes the `clip-renamer` copy from Live's User Library.

**`rackabel dev` when something the loop needs is missing.** `dev` (like `build`/`deploy`)
runs a fast doctor subset first and fails with the same doctor-style `help:` line a musician
sees in `doctor` — it never dumps a raw SDK trace or a half-started host. The Dev-Mode-off
case is the block-and-wait above (§3.6). **Live-not-running gets the same block-and-wait** —
a musician who did everything from the terminal has no reason to know the Live *app* must be
open, so `dev` detects it and says so rather than hanging on "connected to Live":

```
$ rackabel dev
[!] Ableton Live doesn't appear to be running
    help: open Ableton Live (the app) and leave it running — the dev loop
          connects to it. I'll continue automatically once it's up.
  waiting for Live…   (ctrl-c to stop)
```

The other Persona-A-reachable misses fail like this
(User-Library-not-found shown; toolkit-missing and install-copy-failed read the same way):

```
$ rackabel dev
[✗] Couldn't find your Live User Library yet
    help: open Ableton Live once so it creates ~/Music/Ableton…/User Library
          (with an Extensions folder), then rerun `rackabel dev`. Or point me at
          it: `rackabel dev --user-library "/path/to/User Library"`. Nothing was
          installed or changed.
```

The same shape covers "toolkit was found by `new` but Live got moved/uninstalled" (`help:`
points at reinstalling Live + `rackabel doctor`) and "couldn't install the working copy into
your User Library" (`help:` names the folder and a permissions remedy). In every case the
loop stops *before* touching anything, so the failure is recoverable, never a dead-end.

Nothing in the happy path required a hand-typed path, a PID, knowledge of esbuild, the
polyfill banner, the install-folder-naming subtlety, or where logs live; the SDK-not-found
branch tells a non-developer exactly where to get the toolkit, where to put it, and the one
command to re-run. The deploy-before-reload trap is invisible because rackabel owns the
ordering. (`--verbose` shows the developer-facing internals — node/`NODE_MODULE_VERSION`,
`apiVersion`, the resolved host path, the `dist/extension.js` filename — that the default
view hides.)

### 6.3 How doctor diagnoses the known footguns

| Footgun | doctor signal |
|---|---|
| Deploy-before-reload (stale deployed bundle) | "dist newer than deployed copy — `rackabel deploy`" (and `dev` never hits it). |
| Wrong-PID SIGHUP / bare node host running | "a non-rackabel host is running — reload unsafe; stop it or use `rackabel dev`". |
| Native dep not compiled (pnpm blocks build scripts) | plain English: "this extension uses a compiled component that needs to be built — run `rackabel deploy --fix`" (`--fix` runs pnpm `approve-builds`+`rebuild` under the hood; the user never types pnpm — §3.7). |
| Forgotten polyfill banner | impossible: the banner is baked into rackabel's build, not the user's config. |
| Host bundle path moved between Live versions | probes both layouts; reports which exists. |
| Wrong node for native ABI | reports Live bundled node `NODE_MODULE_VERSION` vs the compiled `.node`. |
| Developer Mode off | warns + blocks `dev` with a navigational fix line; `dev` waits for the toggle (§3.6). |
| Live (the app) not running | `dev` detects it, prints the open-Live help line, and waits — never hangs on "connected to Live" (§6.2). |
| minimumApiVersion incompatible | hard pre-deploy/pre-launch gate; the daemon pre-filters incompatible extensions (a behavior rackabel *adds* — the launcher does none) so one bad manifest can't take down the session, whether or not the host aborts-all (§3.2/§9.6). |

---

## 7. Sharp edges for power users

- **Env-var overrides exposed as flags.** The six launcher `ABLETON_*` vars (§3.6 — launcher
  conventions, **not** SDK vars) are each honored with a flag: `--live`/`ABLETON_APP`,
  `--user-library`/`ABLETON_USER_LIBRARY`, `--eh-mod`/`ABLETON_EH_MOD`,
  `--eh-node`/`ABLETON_EH_NODE`, `--extensions-dir`/`ABLETON_EXTENSIONS_DIR`,
  `--storage-base`/`ABLETON_STORAGE_BASE`. The official CLI's `EXTENSION_HOST_PATH` (and a
  create-extension `.env`) is also read and reconciled with `--eh-mod`. Persisted choices live
  in `rackabel.toml`/state, never a flag you repeat (web-clis `--persist-to` anti-pattern);
  the resolved target is always echoed.
- **JSON output.** `--json` on `build`, `deploy`, `pack`, `validate`, `doctor`,
  `dev status`, `dev list`, `dev logs`, `dev reload`, `dev test`, `plugin install`,
  `plugin list`, `plugin which`, `plugin search` for scripting (Wrangler structured tail).
- **`--no-input` is a GLOBAL flag** honored by **every** command, not just `new`. It forces
  non-interactive mode even on a TTY (a tmux/CI runner that *has* a terminal but must stay
  deterministic) and **fails instead of falling back to a default**: any branch that would
  prompt becomes a deterministic error (exit `2` for a usage/missing-answer prompt, `3` for an
  environment prompt such as Dev-Mode-off block-and-wait), **never a silent default-accept**.
  This is stricter than the implicit non-TTY detection (§3.5, §5.4 `--yes`): non-TTY detection
  *infers* non-interactive; `--no-input` *forces* it and removes the fallback, so a CI run of
  `deploy`/`dev reload`/`dev test`/`plugin install` either succeeds with what was given or
  exits non-zero — it can never hang or quietly pick a default. (`--yes` still means "accept
  the defaults"; `--no-input` means "do not prompt, and do not invent an answer.")
- **Exit codes.** `0` success; `1` build/runtime failure; `2` usage error; `3` environment
  not ready (doctor-class failure, so CI can distinguish "my code is wrong" from "this
  machine isn't set up"); `4` validation failure. **Precedence:** when a command auto-runs
  multiple gates (e.g. `pack` on an unprepared machine runs both the environment subset and
  `validate`), the **environment check runs first and short-circuits** — so `3` is returned
  before `4` is ever reached — and the command returns the **single highest-severity code**
  rather than mixing causes, so CI can attribute a failure unambiguously (environment `3` >
  validation `4` > build/runtime `1` for cause attribution — the numbers are identifiers,
  not a severity scale; severity is this listed order. `2` usage errors are caught at
  parse time). PATH-subcommand exit codes pass through (tier 2).
- **Escape hatches from every managed behavior** (granular and reversible — never a
  one-way `eject`):
  - `build --print-config` / `--dry-run` (generic across `build`/`deploy`/`pack`) — see the
    resolved esbuild config / planned steps without mutating anything.
  - `dev --no-auto-reload` — manual reload via `[r]` in the UI or `rackabel dev reload`
    (scriptable, exit-coded — §2 dev table).
  - `dev --raw` / `dev logs --raw` — unfiltered host output, Node internals included.
  - `dev start --foreground` — host tied to your shell.
  - `dev test` (§3.8) — headless TestHarness run, the CI entry point (no Live/GUI).
  - `pack` shells out to the official `extensions-cli package` for pure-JS by default;
    `--no-official-cli` forces rackabel's own packer (and the native path always uses it).
  - Drop to raw tools at any time: rackabel commands are thin wrappers over esbuild,
    `extensions-cli`, and the project's own `package.json` scripts, so `npm run build` and any
    project-defined `*:headless` script (these are the *project's* scripts, not SDK-shipped —
    §3.8) keep working with no rackabel lock-in (audio-music research).
- **`--inspect[=host:port]` passthrough** to the Node debugger — owned by `dev`/`dev start`
  (forwarded to the host's node). **Against an already-running daemon it is never a silent
  no-op:** a node process not started with the inspector cannot be attached to afterward, so
  `dev --inspect`/`dev start --inspect` **restart the host child with the inspector enabled**
  when it is currently running without one, announcing what they did (`restarting host with
  --inspect on 127.0.0.1:9229`); symmetrically, dropping the flag on a later `dev` leaves the
  inspector on until the next host restart. `dev status` reports whether the inspector is
  active and on which port. rackabel can emit a VS Code `launch.json` on request via
  `dev --emit-launch-config` (or `dev start --emit-launch-config`) — reusing/extending the
  one create-extension already produces (§4.7) — but never requires hand-editing it.
- **`--clean`** on build; `dev list --json` and `plugin which` for introspection.
- **Scripting hooks** = the lifecycle hooks (§5.3); a power user drops a local `post_build`
  script via the project's own `rackabel.toml` `[hooks]` table — **no plugin manifest, no
  enable step** (§5.5) — reserving the full `rackabel-plugin.toml` flow for installed
  third-party plugins.

---

## 8. Roadmap

Milestone slices. Each ships something usable; later slices are explicitly deferred.

**0.2 — Extensions reach parity with the existing M4L scope (build/deploy).**
- `[extension]` in `rackabel.toml`; generate + validate `manifest.json`.
- `build` (esbuild + rackabel's polyfill banner, `--release`, `--clean`, `--typecheck`).
- `deploy` (User Library resolution + echo; native-dep graph walk; `--undo`).
- `pack` → `.ablx` (shell out to `extensions-cli package` for pure-JS now; native
  multi-target via rackabel's own packer after).
- `new` reuses/extends `create-extension` + vendors the gated toolkit (SDK-not-found path);
  working default template.
- `doctor` extended to Extensions (host layout, bundled node floor, Dev Mode, SDK, drift);
  no-Live and Intel cases.
- Three-part error style + `explain`; native-dep `--fix` (no pnpm jargon, §3.7).
*Defer:* the managed host, the registry, logging — 0.2 is "build it, deploy it, ship it".

**0.3 — The managed dev host (the headline).**
- `dev start/stop/status` daemon owning the wrapper (wrong-PID SIGHUP impossible);
  re-implements the launcher's multi-extension SIGHUP model in Rust (§3.1).
- `dev` (bare) = build→deploy→reload chained in code (deploy-before-reload impossible);
  working-set scoping (`dev <NAME…>`/`--only`) for monorepos (§3.3).
- `dev register/unregister/enable/disable/list/reload` + `~/.rackabel/registry.toml`;
  `--recursive` monorepo with name-collision disambiguation; minimumApiVersion pre-filter.
- `dev logs` with per-extension leveled sinks; surface silent activate failures.
- `dev test` headless path: project vitest/TestHarness tests + `*:headless` scripts, with a
  best-effort generic smoke and a harness-presence report (the CI entry point, §3.8).
- Crash recovery (non-TTY auto-respawn + crash-loop signal) + liveness banner; multi-Live
  persisted choice; Dev-Mode-off block-and-wait.

**0.4 — Extensibility.**
- PATH-convention subcommands + the env contract + `plugin which`; built-in precedence +
  shadow warnings + `plugin run` (§5.1/§5.6); additive-only tier-2 contract (§5.2).
- Templates: `new --template gh:…` on-demand + `new --update` (copier-style, answers
  persisted, §5.5).
- `plugin install/list/enable/disable/search`, `plugins.lock`, sideloading; threat-model
  surfacing (§5.7).
- Upgrade-time reserved-namespace collision detection: warn when a newly-added built-in shadows
  an installed plugin (§5.6). This must ship **here** — before the first built-in that would
  trigger it (`publish`/`login`, 0.6+) — so the detection machinery predates the collision.

**0.5 — Lifecycle hooks + ship-quality.**
- `rackabel-plugin.toml` named hooks (`post_build`/`doctor_check`/`new_template`/…) with
  the per-hook I/O contract (§5.3), disabled-by-default, versioned `hook_api` +
  `plugin migrate`; project-local `[hooks]` (no manifest, §5.5).
- `validate` stable-identifier drift contract; changelog/version-bump gates.
- `--json` everywhere; finalized exit codes + precedence (§7).

**0.6+ — Aspirational, gated on the SDK.**
- Per-extension reload (needs host support for runtime extension add/remove).
- Official-container integrity hash / signing, if/when Ableton ships one (the `.ablx`
  format and `extensions-cli package` exist **now**, §2 pack/§9.5; signing does not).
- WASM capability-sandboxed plugin tier for an untrusted curated index (the only real
  sandboxing — until then hooks are unsandboxed, §5.7).
- Windows/Linux host management once daemon/signal mechanics there are specified (host
  *location* is already resolvable via the official CLI's resolver, §9.3).
- Session-state preservation across reload (re-select device, re-open test set).
- `rackabel login`/`publish` (browser OAuth, never a PAT-portal) **only if** an official
  distribution channel/marketplace appears — deferred until then. Note these names are
  reserved (§5.6): when added, an installed `rackabel-publish` plugin is detected on upgrade
  and the user is warned + offered `rackabel plugin run publish`, never silently shadowed.

**Explicitly deferred and why:**
- A central marketplace/registry for *extensions* — no official one exists; sideloading +
  `.ablx`-over-Discord is the reality; don't bolt on a late, two-identity store (Open VSX
  postmortem).
- Code signing / native-binary policy — no SDK scheme announced (§9.5, §9.8).
- Cross-OS packing — SDK supports only same-OS-different-arch.
- Anticipating long-running/background extensions — the v1.0 model is run-once,
  right-click-only; don't architect the dev host for a lifecycle that doesn't exist.

---

## 9. Open questions

Honest unresolved decisions, with the tradeoff each one bounds.

1. **Runtime reload granularity.** Does the host's `initialize({extensions})` support
   adding/removing a *single* extension at runtime, or is whole-host restart the only
   primitive? *Bounds:* whether 0.6 per-extension reload is possible or whether debounced
   whole-host reload is the permanent ceiling. We ship whole-host reload now and gate
   per-extension on this answer.
   **ANSWERED (empirically, 2026-06-07, 0.3 recon).** The host *does* contain
   single-extension `loadExtension`/`unloadExtension` command handlers (verified via
   strings in `ExtensionHostNodeModule.node`, incl. their error messages) — but they are
   commands **Live sends to the host** over its private `exthost-ctrl-ipc-channel` Unix
   socket, not anything the public dev entrypoint exposes. `require(EH_MOD)` exports only
   `initialize`/`initializeExtensionHost`, which take the full list once at startup. So
   granular reload exists in the host but is reachable only by speaking Live's private
   control protocol; the supported dev primitive remains whole-host restart. 0.6
   per-extension reload stays gated on Ableton exposing this publicly.

2. **Reload when Developer Mode is off.** When Live runs the host itself, what IPC/signal
   (if any) lets a tool trigger a reload? *Bounds:* whether `rackabel dev` can ever work
   without owning the wrapper. Current stance: it cannot; doctor says so.
   **NARROWED (2026-06-07, 0.3 recon).** Live and the host speak over a private
   `exthost-ctrl-ipc-channel` Unix socket (in `DARWIN_USER_TEMP_DIR`), which carries
   `loadExtension`/`unloadExtension` commands (§9.1) — so a reload channel *exists* but is
   Live's private protocol, unsupported for tools. Stance unchanged. Bonus: Developer Mode
   has **no reliable on-disk boolean** (binary `Preferences.cfg` blob); the shipped
   detection heuristic is process-shape-based — Live running + no Live-spawned host child
   (PPID discrimination) ⇒ Dev Mode ON.

3. **Windows/Linux host management.** Host *location* on Windows is **not** open — the
   official CLI's `resolveExtensionHostDir` already encodes it (`.exe` sibling
   `ExtensionHost` dir and `<root>\Program\ExtensionHost\ExtensionHostNodeModule.node`), so
   rackabel reuses that resolver **for the modern layout**. (Note the resolver covers only the
   modern `Contents/Helpers/ExtensionHost` / `Program\ExtensionHost` paths — **not** the
   legacy macOS `Contents/App-Resources/Extensions/ExtensionHost` alpha layout, which only
   `dev-launch.sh` probes; rackabel re-implements that fallback itself, §3.1. The unknown here
   is whether the alpha layout is still in the wild given the 12.4.5+ floor.) The residual
   unknown is **daemon/signal mechanics** on
   Windows (no SIGHUP), not where the host lives; Linux is presumed unsupported. *Bounds:*
   how much of the *dev daemon* is macOS-only at GA. We ship macOS-first, design the env
   contract OS-agnostic, resolve host location cross-platform now, defer the Windows daemon
   to 0.6+.

4. **Deploy-alone usability.** Is `manifest.json` + bundle in the User Library sufficient
   for a *production* Live (Dev Mode off) to list and run the extension via Live's own
   host, with no rackabel running? Strongly indicated, not verified. *Bounds:* whether
   `deploy`/`pack` output is genuinely self-contained or whether something else is needed.

5. **Container signing (the format itself is settled).** The `.ablx` container exists
   **now** and `extensions-cli package` produces it today (archiver zip of `manifest.json`
   + entry + `-i` includes); rackabel shells out to it for pure-JS and uses its own packer
   for native deps (§2 pack). **The open part is narrower:** is an **integrity-hash /
   signing/verification** scheme coming for `.ablx`? *Bounds:* whether `pack` should add a
   hash/signature step later. We emit `.ablx` now and defer signing.

6. **Does an incompatible minimumApiVersion abort init for ALL extensions?** Asserted
   "verified" in an earlier draft and attributed to `dev-launch.sh` — **wrong**: the launcher
   does **no** apiVersion filtering, registering every manifest-bearing dir unconditionally
   (§3.2). Nothing in the grounding repo demonstrates abort-all, so it is **demoted to an open
   question**. *Bounds:* whether one incompatible `minimumApiVersion` takes down the whole
   `initialize` set or is ignored per-extension. **Safe under both branches:** rackabel's
   *added* pre-filter (§3.2) drops incompatible extensions before init and surfaces them, and
   `validate`/`doctor` gate on it — so abort-all is prevented and drop-one is made explicit. The
   residual cosmetic unknown (error text / Live banner) is design-irrelevant. *To verify:* run
   the host with one over-floor manifest among valid ones; observe whether the valid ones load.
   **ANSWERED (empirically, 2026-06-07, 0.3 recon): ABORT-ALL, confirmed.** The host
   negotiates one `hostApiVersion` (1.0.0) with Live; an extension whose
   `minimumApiVersion` exceeds it throws `Error: Incompatible API version` as an
   `uncaughtException` during activation and the **entire host process exits with code 1**,
   taking every sibling down. Evidence: this machine's own `ExtensionHost.txt` records a
   real session (extension `moisesai.moisesai`) with exactly that uncaught exception
   followed by `Process is exiting with code: 1`; the host module's strings corroborate
   (`Incompatible API version`, `hostApiVersion`, `Missing required field:
   minimumApiVersion`). The §3.2 pre-filter is therefore **mandatory**, not defensive —
   and it shipped in 0.3 (verified live: an over-floor extension shows `Skipped:` while
   siblings load).

7. **Storage/temp dirs under Live's own host.** When Live (not the dev launcher) runs the
   host, what `storageDirectory`/`tempDirectory` does it assign? *Bounds:* whether dev
   state carries over to production. We mirror the launcher's layout and flag the risk.
   **PARTIALLY ANSWERED (empirically, 2026-06-07, 0.3 recon).** Live-managed extensions
   install under `~/Library/Application Support/Ableton/Extensions/<id>/` (namespaced ids,
   e.g. `phil-schalm.livewire`) and get `storageDirectory` under
   `~/Library/Application Support/Ableton/Extension Data/<id>/` — the **same Extension
   Data base the dev convention uses**, so dev storage carries over when the slug matches
   the id. The exact `tempDirectory` Live passes remains unverified (requires a
   Dev-Mode-OFF session; the host module reads a `TEMPDIR` env hint, suggesting Live sets
   it). Residual risk narrowed to temp-dir mismatch only.

8. **Native-binary policy.** Does Live re-validate/re-sign `.node` files or apply any
   policy that could reject a packed `node_modules`? *Bounds:* whether `pack`'s native
   path is safe as-is. Unverified; doctor checks ABI match, not policy.

9. **SDK distribution at GA.** Will `@ableton-extensions/sdk`/`cli` go to public npm, or
   stay gated Centercode tarballs? *Bounds:* whether `new` can `npm install` them or must
   keep vendoring. We vendor now and add a public-npm fast path behind a feature check.

10. **Max for Live lifecycle specifics.** Neither SDK repo has M4L artifacts; the
    `.amxd` packaging, "freeze"/consolidation, and install-path details for v2 devices
    still need separate investigation. *Bounds:* how much of §2's device behavior is
    inherited from the existing scaffold vs newly designed. We keep the existing device
    path and treat M4L deepening as its own track.

11. **Host interface stability.** Is `ExtensionHostNodeModule.node` + `initialize` a supported
    public surface or an internal the official CLI abstracts? *Bounds:* daemon fragility across
    Live releases. We isolate all host-poking behind one module. (Note: `extensions-cli run` is
    single-shot/single-extension/source-dir with *no* reload/watch/registry — §3.1 — so it
    cannot own the dev loop; at most the one-off behind `rackabel dev PATH`. The daemon
    re-implements the launcher's SIGHUP model, whose residual risk — host internals Live could
    change — is exactly why it lives behind one module.)

12. **Headless CI scope vs the interactive loop (tension, resolved).** Interactive `rackabel
    dev` is Live/Dev-Mode/macOS-gated; CI is headless. No conflict: `dev test` is the no-Live CI
    entry point (§3.8), `dev` stays the GUI loop. *Bounds:* how much `activate()`/command behavior
    the (proof-of-concept) TestHarness exercises vs real-Live-only behavior, documented per API.

13. **Will the SDK export a stable testing entry point at GA (under which package name)?** Today
    `TestHarness` lives in SDK *source*, is not exported from any published subpath, the docs use
    a **stale scope** (`@ableton/extensions-sdk/testing` vs shipped `@ableton-extensions/sdk`),
    and it is marked proof-of-concept (§3.8). *Bounds:* whether `dev test` can target a stable
    importable export or must keep driving project-supplied vitest tests. We scope to
    project-provided tests now; add a stable-export fast path behind a feature check at GA.
