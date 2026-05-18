pub mod human;
pub mod json;
pub mod matrix;
pub mod pretty;
pub mod sarif;

use codespan_reporting::term::termcolor::ColorChoice as TermColor;

use crate::app::ColorChoice;

/// Resolve a user-facing [`ColorChoice`] to a concrete
/// [`termcolor::ColorChoice`]. Auto-mode delegates to the caller-
/// supplied `is_tty` closure so each output sink (stdout / stderr)
/// can make its own TTY decision.
pub fn resolve_color(choice: ColorChoice, is_tty: impl FnOnce() -> bool) -> TermColor {
    match choice {
        ColorChoice::Always => TermColor::Always,
        ColorChoice::Never => TermColor::Never,
        ColorChoice::Auto => {
            if is_tty() {
                TermColor::Auto
            } else {
                TermColor::Never
            }
        }
    }
}
