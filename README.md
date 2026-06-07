# rackabel

A CLI for building Ableton Live Extensions and Max for Live devices.

> **rack** + **abel** — pronounced "rackable", because that's what your devices become.

## Status

**0.3 — Managed dev host.** `rackabel dev` runs the headline edit→reload loop: a
supervised Extension Host daemon, a persistent registry, a file watcher that rebuilds and
hot-reloads on save, per-extension logs, and a no-Live CI entry point — all on top of the
0.2 build/deploy/pack/validate parity, the richer `new` scaffolder, and the extended
`doctor`. The Max for Live `[device]` path is preserved (its `.amxd` assembly is still a
later milestone). Git-hosted templates and lifecycle hooks land in 0.4–0.5.

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
rackabel validate      # ship-readiness checklist (exit 4 on failure)
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

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Deliberate divergences from the spec or the official toolchain are recorded in
[`docs/DEVIATIONS.md`](docs/DEVIATIONS.md); the design is in [`docs/DESIGN.md`](docs/DESIGN.md).
