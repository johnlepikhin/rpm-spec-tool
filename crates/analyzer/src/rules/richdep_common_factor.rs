//! RPM593 `richdep-common-factor` — flag rich-dep `or` chains whose
//! every operand is an `and` group sharing a common subterm (or the
//! dual: an `and` chain of `or` groups with a common subterm).
//!
//! `(A and B) or (A and C)` factors to `A and (B or C)`.

use rpm_spec::ast::{BoolDep, DepExpr, Span, SpecFile, TagValue};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{collect_top_level_preamble, dep_expr_canonical_eq};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM593",
    name: "richdep-common-factor",
    description: "Rich-dep `or`-chain whose every `and`-operand shares a common subterm — \
                  factor it out (`(A and B) or (A and C)` → `A and (B or C)`).",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Rich-dep `or`-chain whose every `and`-operand shares a common subterm — factor it out (`(A and B) or (A and C)` → `A and (B or C)`).
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RichdepCommonFactor {
    diagnostics: Vec<Diagnostic>,
}

impl RichdepCommonFactor {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RichdepCommonFactor {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for item in collect_top_level_preamble(spec) {
            let TagValue::Dep(expr) = &item.value else {
                continue;
            };
            if has_common_factor(expr) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "rich-dep operands share a common subterm — factor it out (`(A and B) or \
                         (A and C)` → `A and (B or C)`)",
                        item.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "rewrite as `factor and/or (rest_1 op rest_2 op …)`",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn has_common_factor(expr: &DepExpr) -> bool {
    match expr {
        DepExpr::Atom(_) => false,
        DepExpr::Rich(b) => bool_dep_has_common_factor(b),
        _ => false,
    }
}

fn bool_dep_has_common_factor(b: &BoolDep) -> bool {
    match b {
        BoolDep::Or(xs) => or_of_and_has_common(xs) || xs.iter().any(has_common_factor),
        BoolDep::And(xs) => and_of_or_has_common(xs) || xs.iter().any(has_common_factor),
        BoolDep::With(xs) => xs.iter().any(has_common_factor),
        BoolDep::Without { left, right } => has_common_factor(left) || has_common_factor(right),
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
            has_common_factor(cond)
                || has_common_factor(then)
                || otherwise.as_deref().is_some_and(has_common_factor)
        }
        _ => false,
    }
}

fn or_of_and_has_common(xs: &[DepExpr]) -> bool {
    let groups = collect_and_groups(xs);
    if groups.len() < 2 || groups.len() != xs.len() {
        return false;
    }
    common_subterm(&groups)
}

fn and_of_or_has_common(xs: &[DepExpr]) -> bool {
    let groups = collect_or_groups(xs);
    if groups.len() < 2 || groups.len() != xs.len() {
        return false;
    }
    common_subterm(&groups)
}

fn collect_and_groups(xs: &[DepExpr]) -> Vec<&[DepExpr]> {
    xs.iter()
        .filter_map(|x| {
            let DepExpr::Rich(b) = x else { return None };
            if let BoolDep::And(inner) = b.as_ref() {
                Some(inner.as_slice())
            } else {
                None
            }
        })
        .collect()
}

fn collect_or_groups(xs: &[DepExpr]) -> Vec<&[DepExpr]> {
    xs.iter()
        .filter_map(|x| {
            let DepExpr::Rich(b) = x else { return None };
            if let BoolDep::Or(inner) = b.as_ref() {
                Some(inner.as_slice())
            } else {
                None
            }
        })
        .collect()
}

fn common_subterm(groups: &[&[DepExpr]]) -> bool {
    let first = groups[0];
    for candidate in first {
        if groups[1..]
            .iter()
            .all(|other| other.iter().any(|x| dep_expr_canonical_eq(candidate, x)))
        {
            return true;
        }
    }
    false
}

impl Lint for RichdepCommonFactor {
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
        run_lint::<RichdepCommonFactor>(src)
    }

    #[test]
    fn flags_or_of_and_with_common() {
        let src = "Name: x\nRequires: ((foo and bar) or (foo and baz))\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM593");
    }

    #[test]
    fn flags_and_of_or_with_common() {
        let src = "Name: x\nRequires: ((foo or bar) and (foo or baz))\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_no_common_subterm() {
        let src = "Name: x\nRequires: ((foo and bar) or (baz and qux))\n";
        assert!(run(src).is_empty());
    }
}
