//! ANSI palette for the `matrix coverage` human renderer.
//!
//! Sibling-module to `coverage.rs`. Same shape as
//! `commands/profile/style.rs` (cheap `Copy` painter, plain mode for
//! tests) but with verdict-specific colour methods so the call sites
//! read naturally (`style.dead(tag)` vs `style.wrap("0;31", tag)`).
//!
//! Auto-detection respects both `--color` (CLI flag) and the
//! `NO_COLOR` environment variable (https://no-color.org). `Always`
//! overrides `NO_COLOR` — the user asked for colour explicitly, so
//! we honour the explicit ask over the conservative env default.

use std::io::IsTerminal;

use crate::app::ColorChoice;

/// Painter for the coverage renderer. Cheap (`Copy`); construct once
/// in `render_human` and pass `&Style` down.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Style {
    enabled: bool,
}

impl Style {
    /// Resolve `auto` against stdout TTY status AND `NO_COLOR`. The
    /// explicit `--color always` overrides `NO_COLOR` because the
    /// user's CLI flag is the more recent signal of intent.
    pub(crate) fn new(choice: ColorChoice) -> Self {
        let enabled = match choice {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => {
                std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
            }
        };
        Self { enabled }
    }

    /// Painter that never emits ANSI. Used in unit tests so the
    /// existing `stdout.contains("[DEAD]")` assertions keep working.
    #[cfg(test)]
    pub(crate) fn plain() -> Self {
        Self { enabled: false }
    }

    fn wrap(&self, escape: &str, s: &str) -> String {
        if self.enabled {
            format!("\x1b[{escape}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    /// Verdict tag colours. The palette mirrors the operator's
    /// instinctive read of severity:
    ///
    /// * green = healthy / informational (every profile is fine)
    /// * red   = dead code, attention
    /// * yellow = build-conditional (not bad, but context-dependent)
    /// * blue  = analyser couldn't decide
    pub(crate) fn always_tag(&self, s: &str) -> String {
        self.wrap("1;32", s) // bold green
    }
    pub(crate) fn dead_tag(&self, s: &str) -> String {
        self.wrap("1;31", s) // bold red
    }
    pub(crate) fn conditional_tag(&self, s: &str) -> String {
        self.wrap("1;33", s) // bold yellow
    }
    pub(crate) fn indet_tag(&self, s: &str) -> String {
        self.wrap("1;34", s) // bold blue
    }

    /// Indeterminate-reason category labels: yellow `[config]`
    /// (operator can fix) versus magenta `[tool]` (tool-side
    /// limitation, no operator action). Bold so they pop out of the
    /// rollup table.
    pub(crate) fn config_cat(&self, s: &str) -> String {
        self.wrap("1;33", s) // bold yellow
    }
    pub(crate) fn tool_cat(&self, s: &str) -> String {
        self.wrap("1;35", s) // bold magenta
    }

    /// Structural headers: spec name, `under current build:`,
    /// `under variants:`, the summary header. Bold so they delimit
    /// blocks at a glance.
    pub(crate) fn header(&self, s: &str) -> String {
        self.wrap("1", s)
    }

    /// Dim ancillary text: error codes like `[undefined-macro]`,
    /// `[arith-raw]`, and the `tags:` legend hint. Lower contrast
    /// keeps the eye on the verdicts.
    pub(crate) fn dim(&self, s: &str) -> String {
        self.wrap("2", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_is_identity() {
        let s = Style::plain();
        assert_eq!(s.always_tag("[ALWAYS]"), "[ALWAYS]");
        assert_eq!(s.dead_tag("[DEAD]"), "[DEAD]");
        assert_eq!(s.indet_tag("[INDET]"), "[INDET]");
        assert_eq!(s.header("==>"), "==>");
        assert_eq!(s.dim("[undefined-macro]"), "[undefined-macro]");
    }

    #[test]
    fn never_disables_even_on_tty() {
        let s = Style::new(ColorChoice::Never);
        assert_eq!(s.dead_tag("[DEAD]"), "[DEAD]");
    }

    #[test]
    fn always_emits_escapes() {
        // `--color always` unconditionally enables escapes. The
        // explicit-override-honours-CLI policy with respect to
        // `NO_COLOR` env is documented in `Style::new`; testing it
        // here would require env mutation that the `unsafe_code`
        // forbid would reject — the integration tests (piped
        // stdout in `Stdio::piped` mode, `is_terminal()` returns
        // false) exercise the auto-disable path.
        let s = Style::new(ColorChoice::Always);
        assert_eq!(s.dead_tag("[DEAD]"), "\x1b[1;31m[DEAD]\x1b[0m");
        assert_eq!(s.always_tag("[ALWAYS]"), "\x1b[1;32m[ALWAYS]\x1b[0m");
        assert_eq!(s.config_cat("[config]"), "\x1b[1;33m[config]\x1b[0m");
        assert_eq!(s.tool_cat("[tool]  "), "\x1b[1;35m[tool]  \x1b[0m");
    }
}
