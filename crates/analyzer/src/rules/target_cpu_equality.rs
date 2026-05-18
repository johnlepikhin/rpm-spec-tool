//! RPM439 `target-cpu-equality-to-ifarch` — flag `%if` expressions
//! that compare `%{_target_cpu}` against literal arch strings.
//!
//! `%if "%{_target_cpu}" == "x86_64" || "%{_target_cpu}" == "aarch64"`
//! is exactly `%ifarch x86_64 aarch64`. The native form is shorter
//! and signals "arch guard" to anyone reading the spec.

use rpm_spec::ast::{
    BinOp, CondExpr, Conditional, ExprAst, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::flatten_or;
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM439",
    name: "target-cpu-equality-to-ifarch",
    description: "`%if \"%{_target_cpu}\" == \"ARCH\"` (optionally chained via `||`) should be \
                  written as `%ifarch ARCH …`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%if "%{_target_cpu}" == "ARCH"` (optionally chained via `||`) should be written as `%ifarch ARCH …`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct TargetCpuEquality {
    diagnostics: Vec<Diagnostic>,
}

impl TargetCpuEquality {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let CondExpr::Parsed(ast) = &branch.expr else {
                continue;
            };
            let operands = flatten_or(ast.as_ref().peel_parens());
            let mut archs: Vec<String> = Vec::with_capacity(operands.len());
            for op in operands {
                let Some(arch) = match_target_cpu_eq(op) else {
                    archs.clear();
                    break;
                };
                archs.push(arch);
            }
            if archs.is_empty() {
                continue;
            }
            let list = archs.join(" ");
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`%if` compares `%{{_target_cpu}}` against {n} arch literal(s); use \
                         `%ifarch {list}` instead",
                        n = archs.len(),
                    ),
                    branch.data,
                )
                .with_suggestion(Suggestion::new(
                    format!("rewrite header as `%ifarch {list}`"),
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

/// Match `"%{_target_cpu}" == "ARCH"` (either order) and return the
/// arch literal. The macro side must be exactly `%{_target_cpu}`
/// (with optional surrounding `Paren`s); the arch side must be a
/// plain string literal.
fn match_target_cpu_eq(ast: &ExprAst<Span>) -> Option<String> {
    let ExprAst::Binary {
        kind: BinOp::Eq,
        lhs,
        rhs,
        ..
    } = ast.peel_parens()
    else {
        return None;
    };
    let lhs_peeled = lhs.as_ref().peel_parens();
    let rhs_peeled = rhs.as_ref().peel_parens();
    if let (Some(_), Some(arch)) = (target_cpu_macro(lhs_peeled), arch_string(rhs_peeled)) {
        return Some(arch);
    }
    if let (Some(arch), Some(_)) = (arch_string(lhs_peeled), target_cpu_macro(rhs_peeled)) {
        return Some(arch);
    }
    None
}

fn target_cpu_macro(ast: &ExprAst<Span>) -> Option<()> {
    // RPM's `%if` expression grammar quotes the macro inside a string,
    // so `"%{_target_cpu}"` lands in the AST as `String { value:
    // "%{_target_cpu}" }`, not as a `Macro` node. Match the string
    // form first; fall back to the rare unquoted `%{_target_cpu}`
    // macro-node case for completeness.
    match ast {
        ExprAst::String { value, .. } => {
            let trimmed = value.trim();
            if trimmed == "%{_target_cpu}" || trimmed == "%_target_cpu" {
                Some(())
            } else {
                None
            }
        }
        ExprAst::Macro { text, .. } => {
            let trimmed = text.trim();
            if trimmed == "%{_target_cpu}" || trimmed == "%_target_cpu" {
                Some(())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn arch_string(ast: &ExprAst<Span>) -> Option<String> {
    let ExprAst::String { value, .. } = ast else {
        return None;
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Reject the `%{_target_cpu}` string literal here so we don't
    // mis-classify it as an arch.
    if trimmed.starts_with('%') {
        return None;
    }
    // Heuristic: arch tokens are lowercase alnum + `_`/`-`.
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    Some(trimmed.to_owned())
}

impl<'ast> Visit<'ast> for TargetCpuEquality {
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

impl Lint for TargetCpuEquality {
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
        run_lint::<TargetCpuEquality>(src)
    }

    #[test]
    fn flags_single_target_cpu_equality() {
        let src = "Name: x\n%if \"%{_target_cpu}\" == \"x86_64\"\nLicense: MIT\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM439");
        assert!(diags[0].message.contains("x86_64"));
    }

    #[test]
    fn flags_chain_of_target_cpu_equalities() {
        let src = "Name: x\n\
%if \"%{_target_cpu}\" == \"x86_64\" || \"%{_target_cpu}\" == \"aarch64\"\n\
License: MIT\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("x86_64 aarch64"));
    }

    #[test]
    fn silent_for_non_target_cpu_compare() {
        let src = "Name: x\n%if \"%{_other_macro}\" == \"x86_64\"\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_plain_ifarch() {
        let src = "Name: x\n%ifarch x86_64\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_partial_chain() {
        // Mixed chain — one arm isn't a target_cpu compare.
        let src = "Name: x\n\
%if \"%{_target_cpu}\" == \"x86_64\" || X\n\
License: MIT\n\
%endif\n";
        assert!(run(src).is_empty());
    }
}
