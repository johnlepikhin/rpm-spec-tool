//! RPM093 `condition-mentioned-many-times` — a single `%if`
//! expression appearing many times across the spec is a signal that
//! it should be hoisted into a `%global`. Each repeated occurrence
//! tightens coupling between distant parts of the file.
//!
//! Detection: collect the literal text of every `%if`/`%elif`
//! expression in the spec (both `CondExpr::Raw` and `CondExpr::Parsed`
//! — the latter is re-rendered into a canonical string for
//! comparison). Any expression appearing at least [`THRESHOLD`]
//! times triggers one diagnostic per occurrence with a message
//! pointing at the global candidate.
//!
//! Threshold is hardcoded for now; will move to per-lint config when
//! the profile system lands.

use std::collections::HashMap;

use rpm_spec::ast::{CondExpr, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

/// Minimum number of identical occurrences before we suggest
/// factoring out into `%global`.
const THRESHOLD: usize = 5;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM093",
    name: "condition-mentioned-many-times",
    description:
        "Same `%if` expression appears many times across the spec; consider factoring \
         it into a `%global` flag.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct ConditionMentionedManyTimes {
    diagnostics: Vec<Diagnostic>,
    /// Aggregated occurrences keyed by the normalised expression text.
    occurrences: HashMap<String, Vec<Span>>,
}

impl ConditionMentionedManyTimes {
    pub fn new() -> Self {
        Self::default()
    }

    fn record<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let key = match &branch.expr {
                CondExpr::Raw(t) => t.literal_str().map(|s| s.trim().to_owned()),
                CondExpr::Parsed(ast) => Some(canonicalise_ast(ast)),
                _ => None,
            };
            if let Some(key) = key
                && !key.is_empty()
            {
                self.occurrences.entry(key).or_default().push(branch.data);
            }
        }
    }
}

/// Render an [`ExprAst`] into a canonical comparable string. Drops
/// whitespace variations so `0%{?rhel} >= 8` and `0%{?rhel}>=8` match.
fn canonicalise_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> String {
    use rpm_spec::ast::ExprAst;
    match ast {
        ExprAst::Integer { value, .. } => value.to_string(),
        ExprAst::String { value, .. } => format!("\"{value}\""),
        ExprAst::Macro { text, .. } => text.clone(),
        ExprAst::Identifier { name, .. } => name.clone(),
        ExprAst::Paren { inner, .. } => format!("({})", canonicalise_ast(inner)),
        ExprAst::Not { inner, .. } => format!("!{}", canonicalise_ast(inner)),
        ExprAst::Binary { kind, lhs, rhs, .. } => format!(
            "{}{}{}",
            canonicalise_ast(lhs),
            kind.as_str(),
            canonicalise_ast(rhs)
        ),
        _ => String::new(),
    }
}

impl<'ast> Visit<'ast> for ConditionMentionedManyTimes {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.record(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(
        &mut self,
        node: &'ast Conditional<Span, PreambleContent<Span>>,
    ) {
        self.record(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(
        &mut self,
        node: &'ast Conditional<Span, FilesContent<Span>>,
    ) {
        self.record(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for ConditionMentionedManyTimes {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        // Aggregate-time emission: now that visit_spec finished, emit
        // diagnostics for keys above threshold.
        let mut occs = std::mem::take(&mut self.occurrences);
        // Sort keys for deterministic emit order.
        let mut entries: Vec<_> = occs.drain().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (key, spans) in entries {
            if spans.len() < THRESHOLD {
                continue;
            }
            let count = spans.len();
            for span in spans {
                self.diagnostics.push(Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "condition `{key}` appears {count} times across this spec — \
                         consider factoring into a `%global` flag"
                    ),
                    span,
                ));
            }
        }
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = ConditionMentionedManyTimes::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn rpm093_flags_repeated_condition() {
        // 5 identical `%if 0%{?rhel}` blocks → at threshold (>=5).
        let mut src = String::from("Name: x\n");
        for _ in 0..5 {
            src.push_str("%if 0%{?rhel}\nLicense: MIT\n%endif\n");
        }
        let diags = run(&src);
        assert_eq!(diags.len(), 5, "expected one diag per occurrence: {diags:?}");
        assert_eq!(diags[0].lint_id, "RPM093");
    }

    #[test]
    fn rpm093_silent_below_threshold() {
        let mut src = String::from("Name: x\n");
        for _ in 0..4 {
            src.push_str("%if 0%{?rhel}\nLicense: MIT\n%endif\n");
        }
        assert!(run(&src).is_empty());
    }

    #[test]
    fn rpm093_distinct_conditions_dont_aggregate() {
        let src = "Name: x\n\
                   %if 0%{?rhel}\nLicense: MIT\n%endif\n\
                   %if 0%{?fedora}\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }
}
