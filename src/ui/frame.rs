//! Rendering the three-part error frame and the status/echo vocabulary.
//!
//! The frame (DESIGN §6.1) is always exactly:
//! ```text
//! error: <problem> [RKxxxx]
//!   --> <location>
//!   help: <next action>
//! ```
//! The raw chain is appended only under `--raw`/`--verbose`. Status symbols use the
//! stable vocabulary `[✓]`/`[!]`/`[✗]` (DESIGN doctor checklist). The infer-and-echo
//! helper enforces UX rule 1 ("never hard-fail on a missing field — infer and echo").

use std::io::Write;

use crate::context::Ctx;
use crate::error::RkError;
use crate::ui::color::{ColorMode, Style};

/// Status symbol for a doctor-style line.
#[derive(Clone, Copy, Debug)]
pub enum Symbol {
    Good,
    Warn,
    Bad,
}

impl Symbol {
    fn glyph(self) -> &'static str {
        match self {
            Symbol::Good => "[✓]",
            Symbol::Warn => "[!]",
            Symbol::Bad => "[✗]",
        }
    }

    fn style(self) -> Style {
        match self {
            Symbol::Good => Style::Good,
            Symbol::Warn => Style::Warn,
            Symbol::Bad => Style::Bad,
        }
    }

    /// The colored (or plain) glyph for this symbol.
    pub fn painted(self, mode: ColorMode) -> String {
        self.style().paint(self.glyph(), mode)
    }
}

/// Print the three-part error frame to stderr, color-aware. The raw chain is shown
/// only under `--raw`/`--verbose`.
pub fn print_error(err: &RkError, ctx: &Ctx) {
    let mode = ctx.color_err;
    let mut out = std::io::stderr().lock();

    let code = err.code.as_str();
    let head = format!("error: {} [{code}]", err.problem);
    let _ = writeln!(out, "{}", Style::Bad.paint(&head, mode));

    if let Some(loc) = &err.location {
        // A location is usually one line (a path / value), but some errors carry a
        // multi-line body (e.g. a compiler's own file:line + caret diagnostic). Render
        // the first line after `-->` and indent any continuation lines to line up.
        let mut loc_lines = loc.lines();
        if let Some(first) = loc_lines.next() {
            let _ = writeln!(out, "  --> {first}");
            for line in loc_lines {
                let _ = writeln!(out, "      {line}");
            }
        }
    }
    let help_label = Style::Heading.paint("help:", mode);
    // Indent continuation lines of a multi-line help block to line up under the text.
    let mut help_lines = err.help.lines();
    if let Some(first) = help_lines.next() {
        let _ = writeln!(out, "  {help_label} {first}");
        for line in help_lines {
            let _ = writeln!(out, "        {line}");
        }
    } else {
        let _ = writeln!(out, "  {help_label}");
    }

    if (ctx.raw || ctx.verbose)
        && let Some(raw) = &err.raw
    {
        let _ = writeln!(out, "\n{}", Style::Dim.paint("raw:", mode));
        let _ = writeln!(out, "{raw:#}");
    }

    // Always point at `explain` for the long-form (cargo --explain convention).
    let hint = format!("(run `rackabel explain {code}` for details)");
    let _ = writeln!(out, "{}", Style::Dim.paint(&hint, mode));
}

/// Emit a status line: `[✓] <message>` to stdout.
pub fn emit(sym: Symbol, message: &str, ctx: &Ctx) {
    println!("{} {message}", sym.painted(ctx.color));
}

/// Emit a `[!] <message>` warning to STDERR (color-aware off the error stream's mode).
///
/// Unlike [`emit`], this goes to stderr so it never pollutes a downstream consumer's
/// stdout — used for the §5.1 both-locations plugin warning, which must not corrupt a
/// `plugin run`-ed plugin's own stdout when piped. Callers gate on `ctx.echo_on()`.
pub fn ewarn(message: &str, ctx: &Ctx) {
    let mut out = std::io::stderr().lock();
    let glyph = Symbol::Warn.painted(ctx.color_err);
    let _ = writeln!(out, "{glyph} {message}");
}

/// Emit an indented `help:` continuation under a status line (doctor remedies).
pub fn note(message: &str, ctx: &Ctx) {
    let label = Style::Heading.paint("help:", ctx.color);
    println!("    {label} {message}");
}

/// The infer-and-echo helper (DESIGN UX rule 1). Prints, e.g.:
/// `using inferred name = my-ext (set [extension].name to override)`
/// Suppressed under `--json`.
pub fn echo_inferred(key: &str, value: &str, override_hint: &str, ctx: &Ctx) {
    if !ctx.echo_on() {
        return;
    }
    let line = format!("using inferred {key} = {value} ({override_hint})");
    println!("{}", Style::Dim.paint(&line, ctx.color));
}

/// Echo a resolved value with how it was chosen (Arduino-style), e.g.:
/// `User Library: ~/Music/Ableton/User Library (newest with Extensions)`.
/// Suppressed under `--json`.
pub fn echo_resolved(label: &str, value: &str, how: &str, ctx: &Ctx) {
    if !ctx.echo_on() {
        return;
    }
    let line = format!("{label}: {value} ({how})");
    println!("{}", Style::Dim.paint(&line, ctx.color));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::color::ColorMode;

    #[test]
    fn symbol_glyphs_are_stable() {
        assert_eq!(Symbol::Good.painted(ColorMode::Never), "[✓]");
        assert_eq!(Symbol::Warn.painted(ColorMode::Never), "[!]");
        assert_eq!(Symbol::Bad.painted(ColorMode::Never), "[✗]");
    }
}
