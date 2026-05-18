//! RPM495 `unused-macro-parameter` — flag parametric macro definitions
//! whose option list declares flags the body never references.
//!
//! `%define m(x:y:) echo %{-x}` declares both `-x` and `-y`, but only
//! `%{-x}` is read inside the body. The unused `-y` flag is dead
//! syntax — drop it from the option list.
//!
//! Conservative bail-out: macro bodies containing shell `$(…)` or
//! `%(…)` may reference the option via something the lint can't see;
//! when the body looks shell-heavy, the rule stays silent.

use rpm_spec::ast::{MacroDef, MacroDefKind, Span};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::render_text_with_macros;
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM495",
    name: "unused-macro-parameter",
    description: "Parametric macro declares an option flag in its `(…)` option list that the \
                  body never references.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Parametric macro declares an option flag in its `(…)` option list that the body never references.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct UnusedMacroParameter {
    diagnostics: Vec<Diagnostic>,
}

impl UnusedMacroParameter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for UnusedMacroParameter {
    fn visit_macro_def(&mut self, node: &'ast MacroDef<Span>) {
        if matches!(node.kind, MacroDefKind::Undefine) {
            return;
        }
        let Some(opts) = node.opts.as_deref() else {
            return;
        };
        let options = parse_option_letters(opts);
        if options.is_empty() {
            return;
        }
        let body_text = render_text_with_macros(&node.body);
        // Shell-laden bodies may hide option uses behind `$1`/`$@`/etc.;
        // bail out conservatively when the body contains shell command
        // substitution or RPM shell macros.
        if body_text.contains("$(")
            || body_text.contains("%(")
            || body_text.contains("$*")
            || body_text.contains("$@")
        {
            return;
        }
        for opt in &options {
            let pattern_braced = format!("%{{-{opt}");
            let pattern_plain = format!("%-{opt}");
            if !body_text.contains(&pattern_braced) && !body_text.contains(&pattern_plain) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        format!(
                            "parametric macro `{name}` declares option `-{opt}` but its body \
                             never references `%{{-{opt}}}` — drop the option",
                            name = node.name,
                        ),
                        node.data,
                    )
                    .with_suggestion(Suggestion::new(
                        format!(
                            "remove `{opt}` from the option list `({opts_clean})`",
                            opts_clean = opts.trim_matches(['(', ')'].as_ref())
                        ),
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
        visit::walk_macro_def(self, node);
    }
}

/// Parse a `(x:y:z)`-style option string into a list of option letters
/// (single-character flags). Skips parens, `:` (which marks "takes an
/// argument"), and whitespace. Returns letters in declaration order.
fn parse_option_letters(opts: &str) -> Vec<char> {
    let stripped = opts.trim().trim_matches(['(', ')'].as_ref());
    let mut out = Vec::new();
    for c in stripped.chars() {
        if c.is_ascii_alphabetic() {
            out.push(c);
        } else if c == ':' || c.is_ascii_whitespace() {
            continue;
        } else {
            // Unexpected character — give up rather than mis-parse.
            return Vec::new();
        }
    }
    out
}

impl Lint for UnusedMacroParameter {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<UnusedMacroParameter>(src)
    }

    #[test]
    fn flags_unused_option_y() {
        let src = "Name: x\n%define m(x:y:) echo %{-x}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM495");
        assert!(diags[0].message.contains("-y"));
    }

    #[test]
    fn silent_when_all_options_used() {
        let src = "Name: x\n%define m(x:y:) echo %{-x} %{-y}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_non_parametric_macro() {
        let src = "Name: x\n%global flag 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_shell_heavy_body() {
        // Body uses `$*` — could reference options indirectly. Stay silent.
        let src = "Name: x\n%define m(x:y:) echo $*\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn handles_options_without_colons() {
        // Bare letter option (`-v` flag with no value).
        let src = "Name: x\n%define m(vq) echo %{-v}\n";
        let diags = run(src);
        // `q` is declared but not referenced.
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("-q"));
    }
}
