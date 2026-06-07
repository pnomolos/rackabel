//! Locate & shell out to the vendored `extensions-cli` (DESIGN §4.7, §2 pack) —
//! OWNED BY THE PACK AGENT.
//!
//! The pack-owner fills in locating the vendored CLI and driving
//! `extensions-cli package` for the pure-JS `.ablx` path. The foundation provides
//! only the module so the tree compiles.

// Intentionally empty in the foundation; the pack-owner lands the body.
