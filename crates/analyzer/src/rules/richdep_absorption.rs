//! RPM592 `richdep-absorption` — boolean absorption inside rich deps.
//!
//! `(foo or (foo and bar))` reduces to `(foo)`; `(foo and (foo or bar))`
//! reduces to `(foo)` too. The inner subterm contributes nothing.

use rpm_spec::ast::{BoolDep, DepExpr, Span, SpecFile, TagValue};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{collect_top_level_preamble, dep_expr_canonical_eq};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM592",
    name: "richdep-absorption",
    description: "Rich dep absorbs an inner subterm — `A or (A and B)` reduces to `A`; \
                  `A and (A or B)` reduces to `A`.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Rich dep absorbs an inner subterm — `A or (A and B)` reduces to `A`; `A and (A or B)` reduces to `A`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RichdepAbsorption {
    diagnostics: Vec<Diagnostic>,
}

impl RichdepAbsorption {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RichdepAbsorption {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for item in collect_top_level_preamble(spec) {
            let TagValue::Dep(expr) = &item.value else {
                continue;
            };
            if has_absorption(expr) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "rich dep contains a subterm absorbed by a sibling — `A or (A and B)` \
                         reduces to `A`, `A and (A or B)` reduces to `A`",
                        item.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "drop the absorbed subterm",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn has_absorption(expr: &DepExpr) -> bool {
    match expr {
        DepExpr::Atom(_) => false,
        DepExpr::Rich(b) => bool_dep_has_absorption(b),
        _ => false,
    }
}

fn bool_dep_has_absorption(b: &BoolDep) -> bool {
    match b {
        BoolDep::Or(xs) => check_or_absorption(xs) || xs.iter().any(has_absorption),
        BoolDep::And(xs) => check_and_absorption(xs) || xs.iter().any(has_absorption),
        BoolDep::With(xs) => xs.iter().any(has_absorption),
        BoolDep::Without { left, right } => has_absorption(left) || has_absorption(right),
        BoolDep::If {
            cond,
            then,
            otherwise,
        }
        | BoolDep::Unless {
            cond,
            then,
            otherwise,
        } => {
            has_absorption(cond)
                || has_absorption(then)
                || otherwise.as_deref().is_some_and(has_absorption)
        }
        _ => false,
    }
}

/// Or-absorption: `A or (A and …)` — `A` makes the nested `(A and …)`
/// redundant.
fn check_or_absorption(xs: &[DepExpr]) -> bool {
    for i in 0..xs.len() {
        for j in 0..xs.len() {
            if i == j {
                continue;
            }
            let DepExpr::Rich(b) = &xs[j] else {
                continue;
            };
            if let BoolDep::And(inner) = b.as_ref()
                && inner.iter().any(|y| dep_expr_canonical_eq(y, &xs[i]))
            {
                return true;
            }
        }
    }
    false
}

/// And-absorption: `A and (A or …)` — the nested `(A or …)` contributes
/// nothing extra given `A`.
fn check_and_absorption(xs: &[DepExpr]) -> bool {
    for i in 0..xs.len() {
        for j in 0..xs.len() {
            if i == j {
                continue;
            }
            let DepExpr::Rich(b) = &xs[j] else {
                continue;
            };
            if let BoolDep::Or(inner) = b.as_ref()
                && inner.iter().any(|y| dep_expr_canonical_eq(y, &xs[i]))
            {
                return true;
            }
        }
    }
    false
}

impl Lint for RichdepAbsorption {
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
        run_lint::<RichdepAbsorption>(src)
    }

    #[test]
    fn flags_or_absorption() {
        let src = "Name: x\nRequires: (foo or (foo and bar))\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM592");
    }

    #[test]
    fn flags_and_absorption() {
        let src = "Name: x\nRequires: (foo and (foo or bar))\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_distinct_terms() {
        let src = "Name: x\nRequires: (foo or (bar and baz))\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_plain_atom() {
        let src = "Name: x\nRequires: foo\n";
        assert!(run(src).is_empty());
    }
}
