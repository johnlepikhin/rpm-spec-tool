//! RPM472 `redundant-setup-default-name` — flag `%setup -n %{name}-%{version}`
//! and `%autosetup -n %{name}-%{version}` invocations.
//!
//! The default tarball top-directory `%setup` and friends expect is
//! `%{name}-%{version}`, which is exactly what `-n` here repeats.
//! Drop the redundant flag.

use rpm_spec::ast::{ConditionalMacro, MacroKind, MacroRef, Span, SpecFile, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::prep_model::find_prep_body_with_span;
use crate::rules::util::{MACRO_AUTOSETUP, MACRO_SETUP};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM472",
    name: "redundant-setup-default-name",
    description: "`%setup -n %{name}-%{version}` repeats the default top-directory; drop the \
                  redundant `-n` flag.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%setup -n %{name}-%{version}` repeats the default top-directory; drop the redundant `-n` flag.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RedundantSetupDefaultName {
    diagnostics: Vec<Diagnostic>,
}

impl RedundantSetupDefaultName {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RedundantSetupDefaultName {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some((body, prep_span)) = find_prep_body_with_span(spec) else {
            return;
        };
        for line in &body.lines {
            for (i, seg) in line.segments.iter().enumerate() {
                let TextSegment::Macro(m) = seg else { continue };
                let name = m.name.as_str();
                if name != MACRO_SETUP && name != MACRO_AUTOSETUP {
                    continue;
                }
                if trailing_has_redundant_n(&line.segments[i + 1..]) {
                    self.diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            Severity::Warn,
                            format!(
                                "`%{name} -n %{{name}}-%{{version}}` repeats the default \
                                 top-directory; drop the `-n` flag"
                            ),
                            prep_span,
                        )
                        .with_suggestion(Suggestion::new(
                            "remove `-n %{name}-%{version}` — the default top-directory matches",
                            Vec::new(),
                            Applicability::Manual,
                        )),
                    );
                }
            }
        }
    }
}

/// Walk the segments after a `%setup` / `%autosetup` macro looking for
/// `-n %{name}-%{version}`. Returns `true` if that exact pattern is
/// present.
fn trailing_has_redundant_n(segs: &[TextSegment]) -> bool {
    // Build a token list of "interesting" things: literal flag tokens
    // and macro references. Whitespace separates tokens.
    let mut tokens: Vec<Token<'_>> = Vec::new();
    for seg in segs {
        match seg {
            TextSegment::Literal(s) => {
                // If the previous token is a Macro and this literal
                // starts without whitespace, that macro is adjacent to
                // following content (same shell-word).
                if !s.is_empty()
                    && !s.starts_with(char::is_whitespace)
                    && let Some(last) = tokens.last_mut()
                    && let Token::Macro(m) = *last
                {
                    *last = Token::MacroAdjacentToNext(m);
                }
                for tok in s.split_whitespace() {
                    tokens.push(Token::Lit(tok));
                }
                // Track whether the literal ends mid-word — affects
                // adjacency with a following macro.
                if !s.ends_with(char::is_whitespace)
                    && let Some(last) = tokens.last_mut()
                {
                    *last = match *last {
                        Token::Lit(l) => Token::LitOpen(l),
                        other => other,
                    };
                }
            }
            TextSegment::Macro(m) => {
                // If the previous token was a LitOpen, the upcoming
                // macro joins that literal — but the macro itself may
                // still be followed by more content. We only mark the
                // *previous* macro as adjacent-to-next when we see this
                // new macro start with no whitespace gap.
                if let Some(last) = tokens.last_mut()
                    && let Token::Macro(prev) = *last
                {
                    *last = Token::MacroAdjacentToNext(prev);
                }
                tokens.push(Token::Macro(m));
            }
            _ => {}
        }
    }
    // Look for `-n` followed by the `%{name}-%{version}` shape.
    for i in 0..tokens.len() {
        let is_dash_n = matches!(tokens[i], Token::Lit("-n") | Token::LitOpen("-n"));
        if !is_dash_n {
            continue;
        }
        if i + 1 >= tokens.len() {
            return false;
        }
        if is_name_dash_version(&tokens[i + 1..]) {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone, Copy)]
enum Token<'a> {
    /// Whitespace-bounded literal token.
    Lit(&'a str),
    /// Literal token that is adjacent (no whitespace) to a following
    /// macro — i.e. the literal joins with the macro to form one
    /// shell-token.
    LitOpen(&'a str),
    Macro(&'a MacroRef),
    /// Macro reference that is followed (without whitespace) by more
    /// content in the same shell-word.
    MacroAdjacentToNext(&'a MacroRef),
}

/// Match the three-token sequence `%{name}` `-` `%{version}` starting
/// at the head of `tokens`. Accepts both braced and plain macro forms.
///
/// Requires that the `%{version}` macro is NOT adjacent to further
/// content in the same shell-word — e.g. for
/// `-n %{name}-%{version}%{?dashpre}` the trailing `%{?dashpre}` makes
/// the directory name distinct from the default, so the `-n` is not
/// redundant.
fn is_name_dash_version(tokens: &[Token<'_>]) -> bool {
    if tokens.len() < 3 {
        return false;
    }
    let name_ok = is_plain_macro(&tokens[0], "name");
    let dash_ok = matches!(tokens[1], Token::Lit("-") | Token::LitOpen("-"));
    let version_ok = is_plain_macro(&tokens[2], "version");
    // The version macro must be the terminal token of the shell-word —
    // i.e. not glued to a subsequent macro/literal.
    let version_terminal = matches!(tokens[2], Token::Macro(_));
    name_ok && dash_ok && version_ok && version_terminal
}

fn is_plain_macro(tok: &Token<'_>, name: &str) -> bool {
    let m = match tok {
        Token::Macro(m) | Token::MacroAdjacentToNext(m) => *m,
        _ => return false,
    };
    if !matches!(m.kind, MacroKind::Plain | MacroKind::Braced) {
        return false;
    }
    if !matches!(m.conditional, ConditionalMacro::None) {
        return false;
    }
    if !m.args.is_empty() || m.with_value.is_some() {
        return false;
    }
    m.name == name
}

impl Lint for RedundantSetupDefaultName {
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
        run_lint::<RedundantSetupDefaultName>(src)
    }

    #[test]
    fn flags_setup_with_default_name() {
        let src = "Name: x\n%prep\n%setup -q -n %{name}-%{version}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM472");
    }

    #[test]
    fn flags_autosetup_with_default_name() {
        let src = "Name: x\n%prep\n%autosetup -n %{name}-%{version}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_custom_name() {
        let src = "Name: x\n%prep\n%setup -q -n custom-dir\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_setup_without_n() {
        let src = "Name: x\n%prep\n%setup -q\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_setup_with_trailing_macro_after_version() {
        // Real-world repro (krb5.spec): the `-n` value isn't the default
        // because `%{?dashpre}` glues onto the end of `%{version}`.
        let src = "Name: x\n%prep\n%autosetup -S git_am -n %{name}-%{version}%{?dashpre}\n";
        assert!(run(src).is_empty(), "{:?}", run(src));
    }
}
