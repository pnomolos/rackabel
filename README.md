# rackabel

A CLI for building Max for Live devices and Ableton Live extensions.

> **rack** + **abel** — pronounced "rackable", because that's what your devices become.

## Status

Early scaffold. `new`, `doctor`, `install`, and `watch` work; `build` (assembling
the `.amxd` container) is the next milestone.

## Usage

```sh
rackabel new my-device --kind audio-effect   # scaffold a device project
cd my-device
rackabel doctor                              # check Max / Live / User Library
rackabel build                               # assemble build/my-device.amxd
rackabel install                             # copy into Ableton's User Library
rackabel watch                               # rebuild on save
```

Device kinds: `audio-effect` (default), `midi-effect`, `instrument`.

## Project layout

A rackabel project is a directory with a `rackabel.toml` manifest:

```toml
[device]
name = "my-device"
kind = "audio-effect"
entry = "src/my-device.maxpat"
```

## Development

```sh
cargo build
cargo run -- doctor
```
