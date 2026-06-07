# rackabel

A CLI for building Ableton Live Extensions and Max for Live devices.

> **rack** + **abel** — pronounced "rackable", because that's what your devices become.

## Status

**0.2 — Live Extensions parity.** Extensions reach full build/deploy/pack/validate
parity with the official toolchain, plus a richer `new` scaffolder and an extended
`doctor`. The Max for Live `[device]` path is preserved (its `.amxd` assembly is still a
later milestone). The managed dev host (`rackabel dev`), git-hosted templates, and
lifecycle hooks land in 0.3–0.5.

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
