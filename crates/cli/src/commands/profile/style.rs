//! Inline ANSI palette for the `profile` family.
//!
//! The `profile` renderers all take `out: &mut impl Write` (so tests can
//! capture into a `Vec<u8>`), which makes `termcolor::StandardStream`
//! awkward to plumb through. Instead each call site asks the `Style`
//! to wrap a fragment with ANSI escapes — disabled mode returns the
//! input unchanged, so tests using [`Style::plain`] see plain text.
//!
//! ## Padding caveat
//!
//! ANSI escapes count toward `&str::len()` but render at zero width, so
//! `format!("{label:<W$}", label = style.bold(name))` breaks table
//! alignment. Always **pad first, then colour**:
//!
//! ```ignore
//! let padded = format!("{name:<name_width$}");
//! writeln!(out, "  {} = …", style.bold(&padded))?;
//! ```
//!
//! Trailing whitespace inside the styled span has no visible effect.

use std::io::IsTerminal;

use crate::app::ColorChoice;

/// Painter for plain-`Write` profile renderers. Cheap (`Copy`-ish, only
/// a bool); construct once at dispatch and pass `&Style` down.
#[derive(Debug, Clone, Copy)]
pub(super) struct Style {
    enabled: bool,
}

impl Style {
    /// Resolve `auto` against stdout TTY status. `always` and `never`
    /// are passed through.
    pub(super) fn new(choice: ColorChoice) -> Self {
        let enabled = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => std::io::stdout().is_terminal(),
        };
        Self { enabled }
    }

    /// Painter that never emits ANSI. Used in unit tests so substring
    /// assertions (`output.contains("dist = .el9")`) keep working
    /// against the formatted output unchanged.
    #[cfg(test)]
    pub(super) fn plain() -> Self {
        Self { enabled: false }
    }

    fn wrap(&self, escape: &str, s: &str) -> String {
        if self.enabled {
            format!("\x1b[{escape}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub(super) fn bold(&self, s: &str) -> String {
        self.wrap("1", s)
    }
    pub(super) fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
    pub(super) fn bold_cyan(&self, s: &str) -> String {
        self.wrap("1;36", s)
    }
    pub(super) fn bold_green(&self, s: &str) -> String {
        self.wrap("1;32", s)
    }
    pub(super) fn dim_red(&self, s: &str) -> String {
        self.wrap("2;31", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_is_identity() {
        let s = Style::plain();
        assert_eq!(s.bold("x"), "x");
        assert_eq!(s.dim("y"), "y");
        assert_eq!(s.bold_cyan("z"), "z");
    }

    #[test]
    fn never_disables_even_on_tty() {
        let s = Style::new(ColorChoice::Never);
        assert_eq!(s.bold("hello"), "hello");
    }

    #[test]
    fn always_emits_escapes() {
        let s = Style::new(ColorChoice::Always);
        assert_eq!(s.bold("hi"), "\x1b[1mhi\x1b[0m");
        assert_eq!(s.dim_red("?"), "\x1b[2;31m?\x1b[0m");
    }
}
