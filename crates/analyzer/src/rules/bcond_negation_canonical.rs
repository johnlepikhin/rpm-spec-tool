//! RPM436 `bcond-negation-canonical` — flag `%if !%{with NAME}` and
//! `%if !%{without NAME}` patterns that can be canonicalised by
//! flipping the polarity.
//!
//! `!%{with tests}` is the same condition as `%{without tests}` —
//! both fire exactly when the `tests` bcond is OFF. Specs that ship
//! one or the other style mixed in the same file are inconsistent;
//! prefer the polarity that matches the underlying bcond intent.

use rpm_spec::ast::{
    CondExpr, Conditional, ExprAst, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM436",
    name: "bcond-negation-canonical",
    description: "`%if !%{with NAME}` / `%if !%{without NAME}` can be canonicalised by flipping \
                  polarity (`!%{with X}` → `%{without X}`).",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%if !%{with NAME}` / `%if !%{without NAME}` can be canonicalised by flipping polarity (`!%{with X}` → `%{without X}`).
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct BcondNegationCanonical {
    diagnostics: Vec<Diagnostic>,
}

impl BcondNegationCanonical {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            if let Some((from, to, name)) = match_negated_bcond(ast) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        format!(
                            "`!%{{{from} {name}}}` is just `%{{{to} {name}}}`; flip the polarity \
                             instead of negating"
                        ),
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        format!("rewrite as `%{{{to} {name}}}`"),
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

/// If `ast` is `!%{with NAME}` or `!%{without NAME}` (possibly with
/// extra paren wrappers), return `(from, to, name)`: the original
/// keyword, its flipped form, and the bcond name.
fn match_negated_bcond(ast: &ExprAst<Span>) -> Option<(&'static str, &'static str, String)> {
    let ExprAst::Not { inner, .. } = ast else {
        return None;
    };
    let ExprAst::Macro { text, .. } = inner.as_ref().peel_parens() else {
        return None;
    };
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("%{")
        .and_then(|s| s.strip_suffix('}'))?;
    let mut parts = body.split_ascii_whitespace();
    let head = parts.next()?;
    let name = parts.next()?;
    if parts.next().is_some() {
        return None; // extra tokens — not a clean with/without ref
    }
    match head {
        "with" => Some(("with", "without", name.to_owned())),
        "without" => Some(("without", "with", name.to_owned())),
        _ => None,
    }
}

impl<'ast> Visit<'ast> for BcondNegationCanonical {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for BcondNegationCanonical {
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
        run_lint::<BcondNegationCanonical>(src)
    }

    #[test]
    fn flags_negated_with() {
        let src = "Name: x\n%if !%{with tests}\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM436");
        assert!(diags[0].message.contains("without tests"));
    }

    #[test]
    fn flags_negated_without() {
        let src = "Name: x\n%if !%{without gui}\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("with gui"));
    }

    #[test]
    fn silent_for_canonical_with() {
        let src = "Name: x\n%if %{with tests}\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_negated_non_bcond_macro() {
        let src = "Name: x\n%if !%{some_other_macro}\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }
}
