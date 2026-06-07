//! trycmd transcript acceptance tests (DESIGN §6.2). Each `.trycmd` is a literal
//! terminal session; `[..]` globs cover variable parts (absolute paths, timings,
//! hashes). trycmd runs non-TTY so output is plain (NO_COLOR also set per-case).
//!
//! Command-owners add their own `.trycmd` files under `tests/cli/`; this harness
//! picks up the whole glob. As stub commands are filled in, their transcripts move
//! from "error: … isn't implemented yet" to the real §6.2 musician transcripts.

#[test]
fn cli() {
    trycmd::TestCases::new()
        .case("tests/cli/*.trycmd")
        // Keep color out of transcripts regardless of the runner's TTY state.
        .env("NO_COLOR", "1")
        .env("RACKABEL_NO_INPUT", "1");
}
