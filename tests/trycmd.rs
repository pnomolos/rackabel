//! trycmd transcript acceptance tests (DESIGN §6.2). Each `.trycmd` is a literal
//! terminal session; `[..]` globs cover variable parts (absolute paths, timings,
//! hashes). trycmd runs non-TTY so output is plain (NO_COLOR also set per-case).
//!
//! Command-owners add their own `.trycmd` files under `tests/cli/`; this harness
//! picks up the whole glob. As stub commands are filled in, their transcripts move
//! from "error: … isn't implemented yet" to the real §6.2 musician transcripts.

#[test]
fn cli() {
    // Hermeticity: pin RACKABEL_HOME (and HOME) to a throwaway temp dir for the whole
    // suite so any command that writes state — e.g. `new`'s answers::save on the
    // SDK-not-found path — can never touch the developer's real ~/.rackabel. Cases that
    // need a per-case sandbox (the validate transcripts) still pin RACKABEL_HOME=rk-home
    // inline; an inline env value in the command line overrides this suite-level default.
    let scratch = std::env::temp_dir().join(format!("rackabel-trycmd-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch);

    trycmd::TestCases::new()
        .case("tests/cli/*.trycmd")
        // Keep color out of transcripts regardless of the runner's TTY state.
        .env("NO_COLOR", "1")
        .env("RACKABEL_NO_INPUT", "1")
        // Sandbox the state root + home so no transcript leaks into the real ~/.rackabel.
        .env("RACKABEL_HOME", scratch.display().to_string())
        .env("HOME", scratch.display().to_string());
}
