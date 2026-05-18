//! RPM595 `richdep-nested-same-operator-flatten` — flag rich-dep
//! `and` / `or` / `with` groups whose direct child uses the same
//! operator, allowing the nested parentheses to be flattened.

use rpm_spec::ast::{BoolDep, DepExpr, Span, SpecFile, TagValue};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::collect_top_level_preamble;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM595",
    name: "richdep-nested-same-operator-flatten",
    description: "Rich-dep group contains a nested child with the same operator — flatten the \
                  parentheses (`A and (B and C)` → `A and B and C`).",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Rich-dep group contains a nested child with the same operator — flatten the parentheses (`A and (B and C)` → `A and B and C`).
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RichdepNestedFlatten {
    diagnostics: Vec<Diagnostic>,
}

impl RichdepNestedFlatten {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RichdepNestedFlatten {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for item in collect_top_level_preamble(spec) {
            let TagValue::Dep(expr) = &item.value else {
                continue;
            };
            if has_nested_same_operator(expr) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "rich-dep nests the same operator one level inside another — flatten the \
                         parentheses",
                        item.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "remove the inner parens; the operator is associative",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn has_nested_same_operator(expr: &DepExpr) -> bool {
    match expr {
        DepExpr::Atom(_) => false,
        DepExpr::Rich(b) => bool_has_nested_same(b),
        _ => false,
    }
}

fn bool_has_nested_same(b: &BoolDep) -> bool {
    match b {
        BoolDep::And(xs) => {
            has_child_of_same_kind(xs, Kind::And) || xs.iter().any(has_nested_same_operator)
        }
        BoolDep::Or(xs) => {
            has_child_of_same_kind(xs, Kind::Or) || xs.iter().any(has_nested_same_operator)
        }
        BoolDep::With(xs) => {
            has_child_of_same_kind(xs, Kind::With) || xs.iter().any(has_nested_same_operator)
        }
        BoolDep::Without { left, right } => {
            has_nested_same_operator(left) || has_nested_same_operator(right)
        }
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
            has_nested_same_operator(cond)
                || has_nested_same_operator(then)
                || otherwise.as_deref().is_some_and(has_nested_same_operator)
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    And,
    Or,
    With,
}

fn has_child_of_same_kind(xs: &[DepExpr], kind: Kind) -> bool {
    for x in xs {
        let DepExpr::Rich(b) = x else { continue };
        let same = matches!(
            (kind, b.as_ref()),
            (Kind::And, BoolDep::And(_))
                | (Kind::Or, BoolDep::Or(_))
                | (Kind::With, BoolDep::With(_))
        );
        if same {
            return true;
        }
    }
    false
}

impl Lint for RichdepNestedFlatten {
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
        run_lint::<RichdepNestedFlatten>(src)
    }

    #[test]
    fn flags_nested_and() {
        let src = "Name: x\nRequires: (foo and (bar and baz))\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM595");
    }

    #[test]
    fn flags_nested_or() {
        let src = "Name: x\nRequires: (foo or (bar or baz))\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_mixed_operators() {
        let src = "Name: x\nRequires: (foo and (bar or baz))\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_flat_chain() {
        let src = "Name: x\nRequires: (foo and bar and baz)\n";
        assert!(run(src).is_empty());
    }
}
