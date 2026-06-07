//! Shared services. Command files call into these; shared logic never lives in a
//! command (SPEC C §1). Frozen-signature services land in the foundation; bodies of
//! `esbuild`/`native_dep` (and `banner`/`official_cli`) are filled by their owners.

pub mod banner;
pub mod esbuild;
pub mod live;
pub mod native_dep;
pub mod node;
pub mod official_cli;
pub mod proc;
pub mod toolkit;
pub mod user_library;
