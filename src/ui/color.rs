//! Color gating (DESIGN tests/determinism rules).
//!
//! Color is emitted only when stdout (or stderr, for the error frame) is a TTY
//! *and* `NO_COLOR` is unset. trycmd and CI run non-TTY, so output is plain there
//! with no extra wiring. The [`ColorMode`] is resolved once into the [`Ctx`] so the
//! whole run is consistent and tests can pin it.
//!
//! [`Ctx`]: crate::context::Ctx

/// Whether to emit ANSI color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    Always,
    Never,
}

impl ColorMode {
    pub fn enabled(self) -> bool {
        matches!(self, ColorMode::Always)
    }

    /// Resolve from the environment + a TTY probe.
    ///
    /// `NO_COLOR` (any value) forces off; otherwise color is on iff the given
    /// stream is a terminal. We probe via `owo_colors`' stream support so the
    /// behavior matches the crate we color with.
    pub fn detect(is_tty: bool) -> Self {
        if std::env::var_os("NO_COLOR").is_some() {
            return ColorMode::Never;
        }
        if is_tty {
            ColorMode::Always
        } else {
            ColorMode::Never
        }
    }
}

/// A handful of named styles used across the UI. We keep this tiny and explicit so
/// the symbol/color vocabulary (DESIGN doctor checklist) lives in one place.
#[derive(Clone, Copy, Debug)]
pub enum Style {
    Good,    // [✓] green
    Warn,    // [!] yellow
    Bad,     // [✗] red
    Dim,     // muted notes
    Heading, // bold
}

impl Style {
    /// Wrap `s` with this style's ANSI codes, or return it unchanged when color
    /// is disabled.
    pub fn paint(self, s: &str, mode: ColorMode) -> String {
        if !mode.enabled() {
            return s.to_string();
        }
        use owo_colors::OwoColorize;
        match self {
            Style::Good => s.green().to_string(),
            Style::Warn => s.yellow().to_string(),
            Style::Bad => s.red().to_string(),
            Style::Dim => s.dimmed().to_string(),
            Style::Heading => s.bold().to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_color_env_forces_never() {
        // Save/restore to avoid leaking into other tests in the same process.
        let prev = std::env::var_os("NO_COLOR");
        // SAFETY: tests are single-threaded per test fn here; we restore after.
        unsafe { std::env::set_var("NO_COLOR", "1") };
        assert_eq!(ColorMode::detect(true), ColorMode::Never);
        match prev {
            Some(v) => unsafe { std::env::set_var("NO_COLOR", v) },
            None => unsafe { std::env::remove_var("NO_COLOR") },
        }
    }

    #[test]
    fn non_tty_is_never() {
        let prev = std::env::var_os("NO_COLOR");
        unsafe { std::env::remove_var("NO_COLOR") };
        assert_eq!(ColorMode::detect(false), ColorMode::Never);
        if let Some(v) = prev {
            unsafe { std::env::set_var("NO_COLOR", v) };
        }
    }

    #[test]
    fn never_mode_does_not_paint() {
        assert_eq!(Style::Good.paint("ok", ColorMode::Never), "ok");
    }
}
