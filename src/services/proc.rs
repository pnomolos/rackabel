//! A small subprocess helper: run a command, capture output, map failures.
//!
//! Command-owners (build via node/esbuild, pack via the official CLI) spawn
//! processes; this centralizes capturing stdout/stderr and turning a non-zero exit
//! into a framed [`RkError`] with the raw output gated behind `--raw`. The `--raw`
//! passthrough (inherit stdio) variant is provided for the dev loop (0.3) but is
//! safe to call now.

use std::path::Path;
use std::process::Command;

use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};

/// The captured result of a finished process.
#[derive(Debug)]
pub struct Captured {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Captured {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// Run `program` with `args` in `cwd`, capturing stdout/stderr. The returned
/// `Captured` carries the exit status; the caller decides how to frame a failure
/// (the exit class differs between build vs. pack). A spawn failure (program not
/// found) is itself framed with `code`/`class`.
pub fn capture(
    program: &str,
    args: &[&str],
    cwd: &Path,
    code: ErrorCode,
    class: ExitClass,
) -> CmdResult<Captured> {
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| {
            RkError::new(
                code,
                class,
                format!("could not run `{program}`"),
                format!("make sure `{program}` is available, then retry"),
            )
            .at(format!("{program} {}", args.join(" ")))
            .raw(e.into())
        })?;
    Ok(Captured {
        status: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Run `program` with stdio inherited (the `--raw` passthrough / dev-loop variant).
/// Returns the exit status code (or `None` if killed by a signal). Does not capture.
pub fn passthrough(program: &str, args: &[&str], cwd: &Path) -> CmdResult<Option<i32>> {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .map_err(|e| {
            RkError::new(
                ErrorCode::BuildFailed,
                ExitClass::BuildRuntime,
                format!("could not run `{program}`"),
                format!("make sure `{program}` is available, then retry"),
            )
            .at(format!("{program} {}", args.join(" ")))
            .raw(e.into())
        })?;
    Ok(status.code())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_runs_echo() {
        let cwd = std::env::temp_dir();
        let out = capture(
            "true",
            &[],
            &cwd,
            ErrorCode::BuildFailed,
            ExitClass::BuildRuntime,
        )
        .unwrap();
        assert!(out.success());
    }

    #[test]
    fn capture_missing_program_is_framed() {
        let cwd = std::env::temp_dir();
        let err = capture(
            "definitely-not-a-real-binary-xyz",
            &[],
            &cwd,
            ErrorCode::PackFailed,
            ExitClass::BuildRuntime,
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::PackFailed);
    }
}
