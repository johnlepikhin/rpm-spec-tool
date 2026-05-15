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
            // A condition that is already a single macro / identifier /
            // literal (optionally wrapped in `!` or parens) has nothing
            // to factor out — `%if %{build_libatomic}` IS the `%global`
            // flag form. Suggesting "consider factoring into a
            // `%global` flag" for a one-token expression is noise.
            if is_trivial_expression(&branch.expr) {
                continue;
            }
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

/// `true` when `expr` is already a single-token reference and so has
/// no compound structure worth lifting into a named `%global` flag.
/// Covers single macros (`%{X}`, `%{?X}`, `%X`), bare identifiers,
/// integer/string literals, and unary negations / paren-wrappings of
/// the above.
fn is_trivial_expression(expr: &CondExpr<Span>) -> bool {
    match expr {
        CondExpr::Parsed(ast) => is_trivial_ast(ast.as_ref()),
        CondExpr::Raw(t) => match t.literal_str() {
            Some(s) => is_trivial_raw(s.trim()),
            None => false,
        },
        _ => false,
    }
}

fn is_trivial_ast<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    match ast.peel_parens() {
        ExprAst::Macro { .. }
        | ExprAst::Identifier { .. }
        | ExprAst::Integer { .. }
        | ExprAst::String { .. } => true,
        ExprAst::Not { inner, .. } => is_trivial_ast(inner),
        _ => false,
    }
}

/// Best-effort trivial-check for `CondExpr::Raw` text — used when the
/// parser couldn't fit the expression into the modelled grammar. A
/// single macro reference (with optional `!` negation and `0`
/// prefix used by `0%{?rhel}` idioms) is trivial; anything containing
/// boolean / comparison operators is not.
fn is_trivial_raw(s: &str) -> bool {
    let s = s.strip_prefix('!').map(str::trim_start).unwrap_or(s);
    let s = s.strip_prefix('0').unwrap_or(s);
    if !s.starts_with('%') {
        return false;
    }
    // Any space or operator byte = composite expression.
    !s.bytes()
        .any(|b| matches!(b, b' ' | b'\t' | b'&' | b'|' | b'=' | b'<' | b'>' | b'!' | b'+' | b'-' | b'*' | b'/'))
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
        // 5 identical `%if 0%{?rhel} >= 8` blocks → at threshold (>=5).
        // Use a composite comparison so the trivial-condition filter
        // does not suppress the diagnostic.
        let mut src = String::from("Name: x\n");
        for _ in 0..5 {
            src.push_str("%if 0%{?rhel} >= 8\nLicense: MIT\n%endif\n");
        }
        let diags = run(&src);
        assert_eq!(diags.len(), 5, "expected one diag per occurrence: {diags:?}");
        assert_eq!(diags[0].lint_id, "RPM093");
    }

    #[test]
    fn rpm093_silent_below_threshold() {
        let mut src = String::from("Name: x\n");
        for _ in 0..4 {
            src.push_str("%if 0%{?rhel} >= 8\nLicense: MIT\n%endif\n");
        }
        assert!(run(&src).is_empty());
    }

    #[test]
    fn rpm093_distinct_conditions_dont_aggregate() {
        let src = "Name: x\n\
                   %if 0%{?rhel} >= 8\nLicense: MIT\n%endif\n\
                   %if 0%{?fedora} >= 35\nLicense: MIT\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn rpm093_silent_for_trivial_single_macro_reference() {
        // `%if %{build_libatomic}` is the canonical flag-usage form
        // already — "factor into a %global flag" would be a no-op and
        // is misleading noise.
        let mut src = String::from("Name: x\n");
        for _ in 0..6 {
            src.push_str(
                "%if %{build_libatomic}\n\
                 Requires: libatomic\n\
                 %endif\n",
            );
        }
        assert!(
            run(&src).is_empty(),
            "trivial single-macro condition must not trigger RPM093"
        );
    }

    #[test]
    fn rpm093_silent_for_negated_single_macro() {
        // `%if !%{X}` is still single-token-shaped after stripping the
        // unary `!`; the `%global` advice doesn't apply.
        let mut src = String::from("Name: x\n");
        for _ in 0..6 {
            src.push_str("%if !%{X}\nLicense: MIT\n%endif\n");
        }
        assert!(run(&src).is_empty());
    }

    #[test]
    fn rpm093_fires_for_composite_expression_with_macro() {
        // `%{?rhel} >= 8` has structure — a comparison against a
        // literal — which IS worth lifting into a named flag.
        let mut src = String::from("Name: x\n");
        for _ in 0..5 {
            src.push_str("%if %{?rhel} >= 8\nLicense: MIT\n%endif\n");
        }
        let diags = run(&src);
        assert!(!diags.is_empty(), "composite expression should still fire");
    }
}
