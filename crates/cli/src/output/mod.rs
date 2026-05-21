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
///
/// Precedence in `Auto` mode: explicit flag > `CLICOLOR_FORCE` >
/// `NO_COLOR` > TTY. See <https://no-color.org> and
/// <https://bixense.com/clicolors/>.
pub fn resolve_color(choice: ColorChoice, is_tty: impl FnOnce() -> bool) -> TermColor {
    resolve_color_with_env(choice, is_tty, |k| std::env::var_os(k))
}

/// Inner form that takes an env reader so unit tests can drive it
/// hermetically without touching process-global env state.
fn resolve_color_with_env<F, G>(choice: ColorChoice, is_tty: F, env: G) -> TermColor
where
    F: FnOnce() -> bool,
    G: Fn(&str) -> Option<std::ffi::OsString>,
{
    match choice {
        ColorChoice::Always => TermColor::Always,
        ColorChoice::Never => TermColor::Never,
        ColorChoice::Auto => {
            // CLICOLOR_FORCE: any non-empty, non-"0" value forces colour.
            if let Some(v) = env("CLICOLOR_FORCE") {
                if !v.is_empty() && v != std::ffi::OsStr::new("0") {
                    return TermColor::Always;
                }
            }
            // NO_COLOR: any non-empty value disables colour.
            if let Some(v) = env("NO_COLOR") {
                if !v.is_empty() {
                    return TermColor::Never;
                }
            }
            if is_tty() {
                TermColor::Auto
            } else {
                TermColor::Never
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;

    /// Build a fake env-reader from an in-memory map. Keeps the test
    /// hermetic — no `std::env::set_var` cross-thread races.
    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), OsString::from(*v)))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn no_color_disables_colour_even_on_tty() {
        let env = env_from(&[("NO_COLOR", "1")]);
        let got = resolve_color_with_env(ColorChoice::Auto, || true, env);
        assert!(matches!(got, TermColor::Never));
    }

    #[test]
    fn clicolor_force_enables_colour_even_off_tty() {
        let env = env_from(&[("CLICOLOR_FORCE", "1")]);
        let got = resolve_color_with_env(ColorChoice::Auto, || false, env);
        assert!(matches!(got, TermColor::Always));
    }

    #[test]
    fn auto_with_tty_and_no_env_returns_auto() {
        let env = env_from(&[]);
        let got = resolve_color_with_env(ColorChoice::Auto, || true, env);
        assert!(matches!(got, TermColor::Auto));
    }

    #[test]
    fn explicit_always_overrides_no_color() {
        let env = env_from(&[("NO_COLOR", "1")]);
        let got = resolve_color_with_env(ColorChoice::Always, || true, env);
        assert!(matches!(got, TermColor::Always));
    }

    #[test]
    fn clicolor_force_zero_is_ignored() {
        // "0" must not force colour — matches CLICOLORS semantics.
        let env = env_from(&[("CLICOLOR_FORCE", "0")]);
        let got = resolve_color_with_env(ColorChoice::Auto, || false, env);
        assert!(matches!(got, TermColor::Never));
    }

    #[test]
    fn empty_no_color_is_ignored() {
        // Empty value is treated as unset per no-color.org wording.
        let env = env_from(&[("NO_COLOR", "")]);
        let got = resolve_color_with_env(ColorChoice::Auto, || true, env);
        assert!(matches!(got, TermColor::Auto));
    }

    #[test]
    fn clicolor_force_beats_no_color() {
        // Precedence: CLICOLOR_FORCE wins over NO_COLOR.
        let env = env_from(&[("CLICOLOR_FORCE", "1"), ("NO_COLOR", "1")]);
        let got = resolve_color_with_env(ColorChoice::Auto, || false, env);
        assert!(matches!(got, TermColor::Always));
    }
}
