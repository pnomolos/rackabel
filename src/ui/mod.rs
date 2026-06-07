//! User-facing output: the error frame, color gating, status symbols, prompts.

pub mod color;
pub mod frame;
pub mod prompt;

// Convenience re-exports for command authors. Some are not yet used by a landed
// command (they belong to the frozen UX surface); allow the unused-import warning
// until the parallel command branches consume them.
#[allow(unused_imports)]
pub use frame::{Symbol, echo_inferred, echo_resolved, emit, note, print_error};
