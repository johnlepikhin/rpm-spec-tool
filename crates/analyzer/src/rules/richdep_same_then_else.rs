//! RPM594 `richdep-same-then-else` — flag `(X if C else X)` and
//! `(X unless C else X)` rich-dep conditionals where both arms pick
//! the same expression.

use rpm_spec::ast::{BoolDep, DepExpr, Span, SpecFile, TagValue};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{collect_top_level_preamble, dep_expr_canonical_eq};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM594",
    name: "richdep-same-then-else",
    description: "Rich-dep `(X if C else X)` (or `unless`) — both arms pick the same expression, \
                  drop the conditional.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Rich-dep `(X if C else X)` (or `unless`) — both arms pick the same expression, drop the conditional.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RichdepSameThenElse {
    diagnostics: Vec<Diagnostic>,
}

impl RichdepSameThenElse {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RichdepSameThenElse {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for item in collect_top_level_preamble(spec) {
            let TagValue::Dep(expr) = &item.value else {
                continue;
            };
            if has_same_arms(expr) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "rich-dep `(X if/unless C else X)` picks the same expression in both \
                         arms — drop the conditional",
                        item.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "replace the conditional with the shared expression alone",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn has_same_arms(expr: &DepExpr) -> bool {
    match expr {
        DepExpr::Atom(_) => false,
        DepExpr::Rich(b) => bool_dep_has_same_arms(b),
        _ => false,
    }
}

fn bool_dep_has_same_arms(b: &BoolDep) -> bool {
    match b {
        BoolDep::If {
            then,
            otherwise,
            cond,
        }
        | BoolDep::Unless {
            then,
            otherwise,
            cond,
        } => {
            let same = otherwise
                .as_deref()
                .is_some_and(|o| dep_expr_canonical_eq(then, o));
            if same {
                return true;
            }
            has_same_arms(cond)
                || has_same_arms(then)
                || otherwise.as_deref().is_some_and(has_same_arms)
        }
        BoolDep::And(xs) | BoolDep::Or(xs) | BoolDep::With(xs) => xs.iter().any(has_same_arms),
        BoolDep::Without { left, right } => has_same_arms(left) || has_same_arms(right),
        _ => false,
    }
}

impl Lint for RichdepSameThenElse {
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
        run_lint::<RichdepSameThenElse>(src)
    }

    #[test]
    fn flags_same_then_and_else() {
        let src = "Name: x\nRequires: (foo if cond else foo)\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM594");
    }

    #[test]
    fn flags_unless_same_arms() {
        let src = "Name: x\nRequires: (foo unless cond else foo)\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_distinct_arms() {
        let src = "Name: x\nRequires: (foo if cond else bar)\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_no_else_arm() {
        let src = "Name: x\nRequires: (foo if cond)\n";
        assert!(run(src).is_empty());
    }
}
