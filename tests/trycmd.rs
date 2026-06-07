//! trycmd transcript acceptance tests (DESIGN §6.2). Each `.trycmd` is a literal
//! terminal session; `[..]` globs cover variable parts (absolute paths, timings,
//! hashes). trycmd runs non-TTY so output is plain (NO_COLOR also set per-case).
//!
//! Command-owners add their own `.trycmd` files under `tests/cli/`; this harness
//! picks up the whole glob. As stub commands are filled in, their transcripts move
//! from "error: … isn't implemented yet" to the real §6.2 musician transcripts.

#[test]
fn cli() {
    // Hermeticity: pin HOME to a throwaway temp dir, FRESH PER TEST-PROCESS RUN (keyed on
    // the pid), so any command that writes state under `$HOME/.rackabel` — e.g. `new`'s
    // answers::save on the SDK-not-found path — can never touch the developer's real
    // ~/.rackabel.
    //
    // We deliberately DO NOT set a suite-level RACKABEL_HOME. trycmd merges per-case env
    // with the suite env so that the SUITE value wins for the same key (`Env::update`
    // does `step.add.extend(suite.add)`); a suite RACKABEL_HOME would therefore silently
    // clobber the inline `RACKABEL_HOME=…` that the stateful dev/validate transcripts set
    // on their command lines, and the stateful registry transcripts would all share one
    // home and race (trycmd does not guarantee case order). With no suite RACKABEL_HOME,
    // each command's inline value takes effect: the dev transcripts pin a DISTINCT
    // `rk-home-<file>` (so registry CRUD / name collisions / the `dev test` surface case
    // are isolated from each other), and everything else falls back to the sandboxed
    // `$HOME/.rackabel`.
    let scratch = std::env::temp_dir().join(format!("rackabel-trycmd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::create_dir_all(&scratch);

    // The inline `rk-home-<file>` paths are relative to each transcript's run cwd (its
    // `.in` template dir, or the crate root for transcripts with no `.in`), so they can
    // persist between runs — wipe any residue up front so every run starts empty. (These
    // dirs are also gitignored.)
    for stale in [
        std::path::Path::new("rk-home"),
        std::path::Path::new("rk-home-surface"),
        std::path::Path::new("rk-home-plugin"),
        std::path::Path::new("tests/cli/dev_registry.in/rk-home-reg"),
        std::path::Path::new("tests/cli/dev_register_names.in/rk-home-names"),
        std::path::Path::new("tests/cli/hooks_surface.in/rk-home-hooks"),
    ] {
        let _ = std::fs::remove_dir_all(stale);
    }

    trycmd::TestCases::new()
        .case("tests/cli/*.trycmd")
        // Keep color out of transcripts regardless of the runner's TTY state.
        .env("NO_COLOR", "1")
        .env("RACKABEL_NO_INPUT", "1")
        // Sandbox HOME so no transcript leaks into the real ~/.rackabel.
        .env("HOME", scratch.display().to_string());
}
