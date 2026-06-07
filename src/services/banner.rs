//! The polyfill banner (DESIGN §4.6; SPEC B §1) — OWNED BY THE BUILD AGENT.
//!
//! The build-owner fills in the verbatim banner constant + injection. The
//! foundation provides only the symbol so the module tree compiles. Do NOT rely on
//! this value in 0.2 foundation code; it is a placeholder until the build branch
//! lands the byte-identical banner from SPEC B §1.

/// Placeholder for the esbuild `banner.js` value. Replaced by the build-owner with
/// the byte-identical banner from SPEC B §1 (URL, URLSearchParams, TextEncoder/
/// Decoder, atob/btoa, Request/Response/Headers, stream classes, setImmediate,
/// clearImmediate, performance).
pub const POLYFILL_BANNER: &str = "";
