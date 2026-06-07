# rackabel

A CLI for building Ableton Live Extensions and Max for Live devices.

> **rack** + **abel** — pronounced "rackable", because that's what your devices become.

## Status

**0.4 — Extensibility.** rackabel is now extensible end to end: third-party
`rackabel-<foo>` subcommands (Git/cargo-style dispatch, built-ins always win), a pinned
`plugin install`/`list`/`enable`/`disable`/`search` surface backed by `plugins.lock`, and
Git-hosted **templates** — `new --template` to scaffold and `new --update` to 3-way-merge
later. This sits on top of 0.3's managed dev host (`rackabel dev`: supervised Extension Host
daemon, persistent registry, watch + hot-reload, per-extension logs, no-Live CI) and the 0.2
build/deploy/pack/validate parity, richer `new`, and extended `doctor`. The Max for Live
`[device]` path is preserved (its `.amxd` assembly is still a later milestone); lifecycle
hooks land in 0.5.

## Commands

| Command | What it does |
|---|---|
| `new`      | Scaffold a project (Extension or M4L device) |
| `build`    | Compile/bundle the artifact (no install) |
| `deploy`   | Build + copy into the Live User Library (alias: `install`) |
| `pack`     | Production build → distributable `.ablx` (Extension) / `.amxd` (device) |
| `validate` | Lint the manifest + artifact against ship rules |
| `doctor`   | Diagnose the environment (Live, host module, node, User Library, …) |
| `explain`  | Long-form help for an error code, cargo `--explain` style |
| `dev`      | The managed dev host: edit→build→deploy→reload loop + lifecycle verbs |
| `plugin`   | Install/manage third-party `rackabel-<foo>` subcommands (group; see below) |
| `<foo>`    | Any unknown token runs a `rackabel-<foo>` plugin, if one is installed/on `$PATH` |

`rackabel plugin` is the third-party extension surface:

| `plugin` verb | What it does |
|---|---|
| `plugin install <src>` | Install from `OWNER/REPO` (release asset or pinned clone), a local path, or a `.tgz` — sha256/commit pinned into `plugins.lock` |
| `plugin list`          | Installed plugins with their pin + enabled state (`--json` for the stable shape) |
| `plugin which <name>`  | Where a name resolves: built-in, managed copy, or `$PATH` (and whether it is shadowed) |
| `plugin enable` / `disable` | Toggle whether a managed plugin is reachable as a bare `rackabel <name>` |
| `plugin run <name>`    | Escape hatch: run a plugin explicitly, even if a built-in shadows the name |
| `plugin search <term>` | Search GitHub for `rackabel-plugin`-topic repos (`--json`) |

`rackabel dev` is a command group. Bare `dev` runs the watch loop; the verbs control the
host and its registry:

| `dev` verb | What it does |
|---|---|
| `dev` (bare)        | Start the host if needed, then watch + hot-reload + tail logs in the foreground |
| `dev start` / `stop`| Launch / cleanly stop the daemonized Extension Host |
| `dev status`        | Daemon + per-extension state, resolved Live/host paths, inspector, reload metrics |
| `dev register` / `unregister` | Add / remove a project in the persistent registry (`--recursive` for a workspace) |
| `dev enable` / `disable`      | Toggle whether a registered entry is loaded |
| `dev list`          | Show the registry with status columns (`--json` for the stable shape) |
| `dev watch`         | Explicit watch loop (never auto-starts a daemon) |
| `dev reload`        | Force a whole-host reload now (`--strict` makes a host-incompatible skip fatal) |
| `dev logs`          | Tail/filter the per-extension log sink (`--follow`, `--since`, `--level`, `--json`) |
| `dev test`          | Build + run the project's tests / headless smoke — the no-Live CI entry point |

## Usage — Live Extension

You need Ableton Live Suite 12.4.5+ (with the Extensions beta enabled) and the
Extensions SDK + CLI tarballs downloaded somewhere on disk.

```sh
# Scaffold (vendors the SDK/CLI into the project; --yes accepts defaults)
rackabel new my-ext --kind extension --yes --sdk-dir ~/Downloads/extensions-sdk
cd my-ext
npm install            # or pnpm install — installs the vendored toolchain

rackabel build         # esbuild bundle (banner baked in) → dist/extension.js + manifest.json
rackabel deploy        # build-if-stale, then copy into <User Library>/Extensions/<slug>
rackabel validate      # ship-readiness checklist (exit 4 on failure):
                       #   manifest complete · minimumApiVersion ≤ host · version bumped
                       #   vs the last pack · CHANGELOG entry · native .node present ·
                       #   stable-identifier drift (a renamed extension `name` since the
                       #   last pack warns; --strict makes it fatal)
rackabel pack -o out/  # production build + validate → a distributable .ablx
rackabel doctor        # full environment checklist
```

`deploy --undo` removes the deployed folder; `deploy --fix` builds missing native
dependencies; `deploy --dry-run` prints the plan without touching anything.

## Usage — Max for Live device

```sh
rackabel new my-device --kind audio-effect    # scaffold a device project
cd my-device
rackabel doctor                               # check Max / Live / User Library
rackabel build                                # assemble the .amxd (later milestone)
rackabel install                              # copy into Ableton's User Library
```

Device kinds: `audio-effect` (default), `midi-effect`, `instrument`.

## Usage — managed dev host

The dev host keeps a supervised Extension Host running and hot-reloads your extensions on
save. It needs Ableton Live running with **Developer Mode** on (Preferences → Extensions);
if Live is down or Developer Mode is off, `rackabel dev` tells you what to flip and waits
(or, under `--no-input`, exits `3` deterministically for CI).

```sh
rackabel dev register .          # add the current project to the registry
rackabel dev register ../pkgs --recursive   # add every workspace member under a root

rackabel dev                     # start-if-needed, then watch: edit a source file and it
                                 # rebuilds → deploys → reloads the host automatically
                                 #   rebuilt (120ms) → updated in Live (40ms) → reloaded my-ext

rackabel dev status              # is the host up? which extensions are loaded / skipped?
rackabel dev logs my-ext --follow            # tail one extension's console + lifecycle
rackabel dev reload              # force a reload now (e.g. after editing the manifest)
rackabel dev stop                # stop the host cleanly (no orphaned node processes)
```

Scope the loop to a subset with `--only` or a `--` separator (both match registry names,
never the verbs): `rackabel dev --only my-ext` / `rackabel dev -- my-ext other-ext`.

`dev test` is the headless CI entry point — it builds and runs each target's test harness
with **no Live, no daemon, and no GUI**, emitting a stable `--json` envelope:

```sh
rackabel dev test --json         # { "targets": [ … ], "passed": N, "failed": M }
```

A host-incompatible extension (its `minimum_api_version` exceeds the host's API version)
is **pre-filtered** out of the loaded set and reported as `Skipped:` rather than allowed to
abort the whole host — `dev reload --strict` turns any such skip into a fatal `exit 1`.

The host runs as a daemon under `RACKABEL_HOME`, one per resolved Live install, so closing
the terminal leaves it running; `dev logs` / `dev status` from another shell still work.

## Plugins & subcommands

rackabel is extensible: any leading token that is **not** a built-in is dispatched to a
`rackabel-<foo>` executable (Git/cargo style). Built-ins always win — a plugin can never
shadow `build`, `dev`, etc. — so `rackabel hello` runs `rackabel-hello` only when `hello`
isn't reserved.

```sh
rackabel plugin install owner/repo        # remote: announces what it will fetch/run, asks first
rackabel plugin install ./rackabel-foo    # sideload a local executable (no network)
rackabel plugin install ./foo.tgz         # sideload a tarball
rackabel plugin list                      # what's installed, with its pin + enabled state
rackabel foo --bar                        # run it: argv after the name is forwarded verbatim
rackabel plugin disable foo               # stop dispatching the bare name (run still reaches it)
rackabel plugin run dev                   # escape hatch: run a plugin even if a built-in shadows it
```

Resolution order for a bare `rackabel <foo>`: a built-in (always wins), then the managed
copy under `~/.rackabel/plugins/bin`, then `$PATH`. If the same `rackabel-<foo>` is in both
the managed dir and `$PATH`, the managed one is used and a **one-time** note is printed to
stderr (see `plugin which <foo>`).

Security (§5.7): a **remote** install prints exactly what it will fetch and run, then
requires confirmation — `--yes` consents in a script; `--no-input` (or a non-TTY, or
`--json`) refuses and fetches nothing. Every installed file is **pinned** by sha256 (assets/
sideloads) or commit (clones) in `~/.rackabel/plugins.lock`; a tampered or pin-mismatched
file fails with exit 4 before it ever runs. Pins protect against tampering and silent
updates — rackabel never auto-updates a plugin.

A plugin runs with the §5.2 environment contract: `RACKABEL` (always the current binary),
`RACKABEL_HOME`, and — inside a project — `RACKABEL_PROJECT_ROOT`. Vars are *unset* (not
empty) when they don't apply, and the contract is additive over your existing environment.

## Templates

`new --template` scaffolds from a template repo; `new --update` re-applies it later via a
3-way merge so a project can adopt template improvements without losing local edits.

```sh
rackabel new my-ext --template gh:owner/repo          # remote (confirmation gated)
rackabel new my-ext --template gh:owner/repo@v2       # pin a ref
rackabel new my-ext --template ./local-template       # a local checkout (no network)

# later, after the template author ships improvements:
rackabel new --update --dry-run                       # show the merge plan, change nothing
rackabel new --update                                 # 3-way merge; conflicts get markers (exit 4)
```

A template is a repo with a `rackabel-template.toml` (`[prompts]` drives the wizard;
`[merge].exclude` lists files copied verbatim, never text-merged). Files are rendered with a
deliberately minimal `{{ key }}` placeholder (unknown keys are left verbatim). The chosen
repo, commit, and answers are recorded in the project's `.rackabel-template` so `--update`
re-runs the same template. A remote template is confirmation-gated exactly like a remote
plugin install (`--yes` / `--no-input` semantics). See
[`docs/TEMPLATES.md`](docs/TEMPLATES.md) for the template-author reference.

## Project layout

A rackabel project is a directory with a `rackabel.toml` manifest. Extensions:

```toml
[extension]
name = "My Extension"
author = "Your Name"
version = "0.1.0"
entry = "src/extension.ts"
minimum_api_version = "1.0.0"

[extension.build]
native_deps = []          # npm package names to externalize + bundle on deploy/pack
extra_dist_files = []     # extra files (relative to dist/) shipped alongside the bundle

[extension.pack]
targets = []              # e.g. ["darwin-arm64", "darwin-x64"] for native builds

[dev]
debounce_ms = 200         # watch-loop debounce before a rebuild→reload (default 200)
```

Most fields are inferred when absent (and echoed): `name` from the directory, `author`
from `git config user.name`, `version` `0.1.0`, `entry` `src/extension.ts`,
`minimum_api_version` `1.0.0`. Devices keep the existing schema:

```toml
[device]
name = "my-device"
kind = "audio-effect"
entry = "src/my-device.maxpat"
```

## Global flags & environment

`--no-input` (never prompt — fail deterministically), `--json`, `--verbose`, `--raw`,
`--no-color` (also honors `NO_COLOR`). Path overrides: `--live` / `ABLETON_APP`,
`--user-library` / `ABLETON_USER_LIBRARY`, `--eh-mod` / `ABLETON_EH_MOD`, `--eh-node` /
`ABLETON_EH_NODE`, `--extensions-dir` / `ABLETON_EXTENSIONS_DIR`, `--storage-base` /
`ABLETON_STORAGE_BASE`. State lives under `RACKABEL_HOME` (default `~/.rackabel`).

## Errors

Every expected failure prints a three-part frame and carries a stable code:

```
error: <plain-English problem> [RK1301]
  --> <offending value/location>
  help: <next action>
(run `rackabel explain RK1301` for details)
```

Exit codes: `0` ok, `1` build/runtime, `2` usage, `3` environment-not-ready,
`4` validation. Run `rackabel explain <code>` for the long-form write-up.

**Precedence.** When a command auto-runs several gates (e.g. `deploy --release` runs the
environment check *and* `validate`), the environment check runs first and
short-circuits: `3` is returned before `4` is ever reached, and the command returns the
**single highest-severity** code rather than mixing causes (cause-attribution order:
environment `3` > validation `4` > build/runtime `1`; usage `2` is caught at parse time).
So CI can attribute a failure unambiguously — "this machine isn't set up" (`3`) is never
masked by "my manifest is wrong" (`4`).

## `--json` output

`--json` is supported on `build`, `deploy`, `pack`, `validate`, `doctor`, `dev status`,
`dev list`, `dev logs`, `dev reload`, `dev test`, `plugin install`, `plugin list`,
`plugin which`, and `plugin search`. **stdout always carries exactly one JSON value** —
on success and on failure — so a script can parse it unconditionally:

- **Success** is the command's own object/array (e.g. `validate` → a checklist envelope,
  `dev test` → `{ "targets": [...], "passed": N, "failed": M }`).
- **Failure** is also JSON on stdout, never a human frame on stderr with an empty stdout.
  A *setup/environment* failure (no manifest, bad TOML, no User Library, …) is rendered as
  a uniform **error envelope**:

  ```json
  { "ok": false, "code": "RK0001", "exit": 3,
    "problem": "no manifest found",
    "location": "looked for rackabel.toml in /path and its parents",
    "help": "run `rackabel new` to scaffold one, or cd into a project directory" }
  ```

  (`location` is `null` when the error carries none; `raw` is added only under
  `--raw`/`--verbose`.) A *domain-shaped* failure whose own envelope already encodes the
  outcome (`validate`'s checklist with `ok:false`, `dev test`'s `failed:true`, `dev
  reload`'s result) keeps that single envelope and is **not** double-printed. `exit`
  mirrors the process exit code.

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Deliberate divergences from the spec or the official toolchain are recorded in
[`docs/DEVIATIONS.md`](docs/DEVIATIONS.md); the design is in [`docs/DESIGN.md`](docs/DESIGN.md).
