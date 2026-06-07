//! Interactive prompt wrappers (DESIGN UX rules + `--no-input`).
//!
//! Every prompt here honors `--no-input`: under it, instead of prompting, the
//! function returns a deterministic [`RkError`] (usage class for a missing answer,
//! environment class for an environment block). Picking from a list is *always* a
//! numbered pick-list, never free-text (UX rule 2). These wrap `inquire`, which has
//! a clean non-TTY story: it errors rather than silently defaulting.

use crate::context::Ctx;
use crate::error::{CmdResult, ErrorCode, ExitClass, RkError};

/// A text prompt with an Enter-to-accept default. Under `--no-input`, if a default
/// is present it is returned (the `--yes` semantics: accept defaults); if there is
/// no default, it is a usage error (no answer to invent).
pub fn text(label: &str, default: Option<&str>, ctx: &Ctx) -> CmdResult<String> {
    if ctx.no_input {
        return match default {
            Some(d) => Ok(d.to_string()),
            None => Err(RkError::new(
                // No explain code for usage-class prompts (clap-style); reuse a
                // project code for the frame but force the usage exit class.
                ErrorCode::ManifestIncomplete,
                ExitClass::Usage,
                format!("`{label}` is required but no value was given"),
                format!(
                    "pass it as a flag (running with --no-input, so I won't prompt for `{label}`)"
                ),
            )),
        };
    }

    let mut p = inquire::Text::new(label);
    if let Some(d) = default {
        p = p.with_default(d);
    }
    p.prompt().map_err(|e| prompt_err(label, e))
}

/// A confirm (yes/no) prompt with a default. `--no-input` returns the default.
pub fn confirm(label: &str, default: bool, ctx: &Ctx) -> CmdResult<bool> {
    if ctx.no_input {
        return Ok(default);
    }
    inquire::Confirm::new(label)
        .with_default(default)
        .prompt()
        .map_err(|e| prompt_err(label, e))
}

/// A numbered single-select pick-list (never free-text — UX rule 2). Returns the
/// chosen index. Under `--no-input` this is a usage error: a pick cannot be
/// invented deterministically except by an explicit "newest"/"first" rule that the
/// *caller* applies before reaching here (see services that pick newest-and-echo).
pub fn select(label: &str, options: &[String], ctx: &Ctx) -> CmdResult<usize> {
    if ctx.no_input {
        return Err(RkError::new(
            ErrorCode::UserLibraryAmbiguous,
            ExitClass::Usage,
            format!("`{label}` needs a choice but --no-input forbids prompting"),
            "pass the relevant flag to choose explicitly, or run without --no-input",
        ));
    }
    let choice = inquire::Select::new(label, options.to_vec())
        .prompt()
        .map_err(|e| prompt_err(label, e))?;
    Ok(options.iter().position(|o| *o == choice).unwrap_or(0))
}

/// Map an inquire error (incl. cancellation / non-TTY) into a framed usage error.
fn prompt_err(label: &str, e: inquire::InquireError) -> RkError {
    use inquire::InquireError::*;
    let (problem, help): (String, &str) = match e {
        OperationCanceled | OperationInterrupted => (
            "cancelled".to_string(),
            "rerun when you're ready, or pass the value as a flag",
        ),
        NotTTY => (
            format!("can't prompt for `{label}` — no interactive terminal"),
            "pass the value as a flag, or run with --no-input and supply defaults",
        ),
        other => (
            format!("could not read `{label}`: {other}"),
            "pass the value as a flag",
        ),
    };
    RkError::new(
        ErrorCode::ManifestIncomplete,
        ExitClass::Usage,
        problem,
        help,
    )
}
