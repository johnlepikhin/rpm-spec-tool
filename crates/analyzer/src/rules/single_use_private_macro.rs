//! RPM492 `single-use-private-macro` — flag local `%global NAME BODY`
//! definitions whose `%{NAME}` reference appears exactly once in the
//! spec.
//!
//! Defining a macro for a single consumer adds indirection without
//! the deduplication payoff. Inline the body at the call site and
//! drop the `%global`.
//!
//! Companion to RPM118 (`unused-conditional-global`): RPM118 fires on
//! the zero-references case; RPM492 fires at exactly one reference.
//! Names starting with `_` are treated as candidate public knobs and
//! skipped — they often serve as documented override hooks.

use std::collections::HashMap;

use rpm_spec::ast::{CondExpr, ExprAst, MacroDef, MacroDefKind, Span, SpecFile, Text, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM492",
    name: "single-use-private-macro",
    description: "`%global NAME BODY` is referenced exactly once in the spec — inline the body \
                  at the call site and drop the `%global`.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// `%global NAME BODY` is referenced exactly once in the spec — inline the body at the call site and drop the `%global`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct SingleUsePrivateMacro {
    diagnostics: Vec<Diagnostic>,
    defs: Vec<(String, Span, MacroDefKind)>,
    /// `name → number of references in spec text`.
    counts: HashMap<String, usize>,
}

impl SingleUsePrivateMacro {
    pub fn new() -> Self {
        Self::default()
    }

    fn record_text(&mut self, t: &Text) {
        for seg in &t.segments {
            if let TextSegment::Macro(mr) = seg {
                *self.counts.entry(mr.name.clone()).or_insert(0) += 1;
                for a in &mr.args {
                    self.record_text(a);
                }
                if let Some(v) = &mr.with_value {
                    self.record_text(v);
                }
            }
        }
    }

    fn record_expr(&mut self, expr: &ExprAst<Span>) {
        match expr {
            ExprAst::Macro { text, .. } => {
                if let Some(name) = extract_simple_macro_name(text) {
                    *self.counts.entry(name).or_insert(0) += 1;
                }
            }
            ExprAst::Paren { inner, .. } | ExprAst::Not { inner, .. } => self.record_expr(inner),
            ExprAst::Binary { lhs, rhs, .. } => {
                self.record_expr(lhs);
                self.record_expr(rhs);
            }
            _ => {}
        }
    }
}

fn extract_simple_macro_name(text: &str) -> Option<String> {
    let inner = text.trim().strip_prefix('%')?;
    let body = match inner.strip_prefix('{') {
        Some(rest) => rest.strip_suffix('}')?,
        None => inner,
    };
    let body = body
        .trim_start_matches('?')
        .trim_start_matches('!')
        .trim_start_matches('?');
    let name: String = body
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() { None } else { Some(name) }
}

impl<'ast> Visit<'ast> for SingleUsePrivateMacro {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // First record all reads + defs via the walker; then emit at
        // end with the final counts. Order matters: we must not count
        // the def itself as a read. The default walker visits MacroDef
        // body content via visit_macro_def; we override visit_macro_def
        // to capture the def WITHOUT recursing into the body (which
        // could otherwise inflate counts spuriously).
        visit::walk_spec(self, spec);
        let counts = std::mem::take(&mut self.counts);
        let defs = std::mem::take(&mut self.defs);
        for (name, span, _kind) in defs {
            if name.starts_with('_') {
                continue;
            }
            if matches!(counts.get(&name), Some(1)) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        format!(
                            "`%global {name}` is referenced exactly once — inline the body at \
                             its single call site and drop the definition"
                        ),
                        span,
                    )
                    .with_suggestion(Suggestion::new(
                        "replace the single `%{NAME}` use with the macro's body and remove the \
                         `%global` definition",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }

    fn visit_macro_def(&mut self, node: &'ast MacroDef<Span>) {
        if matches!(node.kind, MacroDefKind::Global) {
            self.defs.push((node.name.clone(), node.data, node.kind));
        }
        // Walk into the body — internal `%{X}` references count toward
        // X's read total, just like any other macro reference.
        visit::walk_macro_def(self, node);
    }

    fn visit_text(&mut self, node: &'ast Text) {
        self.record_text(node);
        visit::walk_text(self, node);
    }

    fn visit_top_conditional(
        &mut self,
        node: &'ast rpm_spec::ast::Conditional<Span, rpm_spec::ast::SpecItem<Span>>,
    ) {
        for branch in &node.branches {
            self.record_cond_expr(&branch.expr);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(
        &mut self,
        node: &'ast rpm_spec::ast::Conditional<Span, rpm_spec::ast::PreambleContent<Span>>,
    ) {
        for branch in &node.branches {
            self.record_cond_expr(&branch.expr);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(
        &mut self,
        node: &'ast rpm_spec::ast::Conditional<Span, rpm_spec::ast::FilesContent<Span>>,
    ) {
        for branch in &node.branches {
            self.record_cond_expr(&branch.expr);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl SingleUsePrivateMacro {
    fn record_cond_expr(&mut self, c: &CondExpr<Span>) {
        match c {
            CondExpr::Parsed(ast) => self.record_expr(ast),
            CondExpr::Raw(t) => self.record_text(t),
            CondExpr::ArchList(items) => {
                for t in items {
                    self.record_text(t);
                }
            }
            _ => {}
        }
    }
}

impl Lint for SingleUsePrivateMacro {
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
        run_lint::<SingleUsePrivateMacro>(src)
    }

    #[test]
    fn flags_single_use_global() {
        let src = "Name: x\n%global local_foo bar\nSummary: %{local_foo}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM492");
    }

    #[test]
    fn silent_for_multiple_uses() {
        let src = "Name: x\n%global local_foo bar\nSummary: %{local_foo}\nVersion: %{local_foo}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_zero_uses() {
        // RPM118's territory — RPM492 fires only at exactly 1.
        let src = "Name: x\n%global local_foo bar\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_underscore_prefix() {
        // Names starting with `_` are conventionally public overrides.
        let src = "Name: x\n%global _my_override bar\nSummary: %{_my_override}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_define_form() {
        // RPM492 only inspects `%global` for now; `%define` has
        // scope-sensitive semantics.
        let src = "Name: x\n%define local_foo bar\nSummary: %{local_foo}\n";
        assert!(run(src).is_empty());
    }
}
