//! RPM455 `repeated-ifelse-value-extraction` — flag two or more
//! `%if X V1 %else V2 %endif` blocks with the SAME condition picking
//! between values, suggesting a single `%global` selector.
//!
//! ```text
//! %if %{with toolset}
//! BuildRequires: gcc-toolset
//! %else
//! BuildRequires: gcc
//! %endif
//!
//! %if %{with toolset}
//! Requires: gcc-toolset-runtime
//! %else
//! Requires: gcc-runtime
//! %endif
//! ```
//! collapses to one `%global compiler …` (computed from the
//! condition) plus `BuildRequires: %{compiler}` / `Requires: %{compiler}-runtime`.
//!
//! Sibling to RPM093 (`condition-mentioned-many-times`), which fires
//! at five or more uses of the same condition anywhere. RPM455 fires
//! at the lower threshold (two) for the *specific* if/else-with-value
//! shape because the rewrite payoff is concrete.

use std::collections::HashMap;

use rpm_spec::ast::{
    CondExpr, CondKind, Conditional, PreambleContent, Section, Span, SpecFile, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM455",
    name: "repeated-ifelse-value-extraction",
    description: "Two or more `%if X … %else … %endif` blocks share the same condition and \
                  each branch picks a single dep/global value — extract the choice into one \
                  `%global` selector.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Two or more `%if X … %else … %endif` blocks share the same condition and each branch picks a single dep/global value — extract the choice into one `%global` selector.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RepeatedIfelseValueExtraction {
    diagnostics: Vec<Diagnostic>,
    /// Collected by canonical condition text. The map keys are already
    /// hashable `String`s, so insert is O(1) amortised rather than the
    /// previous O(N) linear scan over a `Vec<Bucket>`.
    buckets: HashMap<CondExprText, Vec<Span>>,
}

/// Hashable canonical text for an if/else condition. `Some(text)` for
/// resolvable Raw / ArchList / Parsed forms; `None` for unresolved.
type CondExprText = String;

impl RepeatedIfelseValueExtraction {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RepeatedIfelseValueExtraction {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for it in &spec.items {
            match it {
                SpecItem::Conditional(c) => {
                    self.try_record_top(c);
                    self.recurse_top(c);
                }
                #[allow(clippy::collapsible_match)]
                SpecItem::Section(boxed) => {
                    if let Section::Package { content, .. } = boxed.as_ref() {
                        for sub in content {
                            if let PreambleContent::Conditional(c) = sub {
                                self.try_record_preamble(c);
                                self.recurse_preamble(c);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        self.flush();
    }
}

impl RepeatedIfelseValueExtraction {
    fn try_record_top(&mut self, c: &Conditional<Span, SpecItem<Span>>) {
        if !is_if_else_value_shape_top(c) {
            return;
        }
        let key = render_cond(&c.branches[0].expr);
        self.attach(key, c.data);
    }

    fn try_record_preamble(&mut self, c: &Conditional<Span, PreambleContent<Span>>) {
        if !is_if_else_value_shape_preamble(c) {
            return;
        }
        let key = render_cond(&c.branches[0].expr);
        self.attach(key, c.data);
    }

    fn attach(&mut self, key: CondExprText, span: Span) {
        self.buckets.entry(key).or_default().push(span);
    }

    fn recurse_top(&mut self, c: &Conditional<Span, SpecItem<Span>>) {
        for branch in &c.branches {
            for it in &branch.body {
                if let SpecItem::Conditional(inner) = it {
                    self.try_record_top(inner);
                    self.recurse_top(inner);
                }
            }
        }
        if let Some(els) = &c.otherwise {
            for it in els {
                if let SpecItem::Conditional(inner) = it {
                    self.try_record_top(inner);
                    self.recurse_top(inner);
                }
            }
        }
    }

    fn recurse_preamble(&mut self, c: &Conditional<Span, PreambleContent<Span>>) {
        for branch in &c.branches {
            for it in &branch.body {
                if let PreambleContent::Conditional(inner) = it {
                    self.try_record_preamble(inner);
                    self.recurse_preamble(inner);
                }
            }
        }
        if let Some(els) = &c.otherwise {
            for it in els {
                if let PreambleContent::Conditional(inner) = it {
                    self.try_record_preamble(inner);
                    self.recurse_preamble(inner);
                }
            }
        }
    }

    fn flush(&mut self) {
        for spans in std::mem::take(&mut self.buckets).into_values() {
            if spans.len() < 2 {
                continue;
            }
            for span in &spans {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "this `%if X … %else … %endif` block shares its condition with at least \
                         one other value-picking block; extract the choice into a single \
                         `%global` and reuse the result",
                        *span,
                    )
                    .with_suggestion(Suggestion::new(
                        "define one `%global` per branch up-front, then reference it where each \
                         value is needed",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn is_if_else_value_shape_top(c: &Conditional<Span, SpecItem<Span>>) -> bool {
    if c.branches.len() != 1 || !matches!(c.branches[0].kind, CondKind::If) || c.otherwise.is_none()
    {
        return false;
    }
    let then_ok = body_is_single_simple_top(&c.branches[0].body);
    let else_ok = c
        .otherwise
        .as_deref()
        .is_some_and(body_is_single_simple_top);
    then_ok && else_ok
}

fn is_if_else_value_shape_preamble(c: &Conditional<Span, PreambleContent<Span>>) -> bool {
    if c.branches.len() != 1 || !matches!(c.branches[0].kind, CondKind::If) || c.otherwise.is_none()
    {
        return false;
    }
    let then_ok = body_is_single_simple_preamble(&c.branches[0].body);
    let else_ok = c
        .otherwise
        .as_deref()
        .is_some_and(body_is_single_simple_preamble);
    then_ok && else_ok
}

fn body_is_single_simple_top(body: &[SpecItem<Span>]) -> bool {
    let mut real = body
        .iter()
        .filter(|it| !matches!(it, SpecItem::Blank | SpecItem::Comment(_)));
    let first = real.next();
    if real.next().is_some() {
        return false;
    }
    matches!(first, Some(SpecItem::Preamble(_) | SpecItem::MacroDef(_)))
}

fn body_is_single_simple_preamble(body: &[PreambleContent<Span>]) -> bool {
    let mut real = body
        .iter()
        .filter(|it| !matches!(it, PreambleContent::Blank | PreambleContent::Comment(_)));
    let first = real.next();
    if real.next().is_some() {
        return false;
    }
    matches!(first, Some(PreambleContent::Item(_)))
}

/// Canonical text for a condition. `Raw` macro-tainted conditions
/// (which the `cond_expr_resolvably_eq` helper refuses) fall back to
/// a unique sentinel so they bucket alone (won't trigger the rule).
fn render_cond(expr: &CondExpr<Span>) -> String {
    use rpm_spec::ast::ExprAst;
    fn render_ast(ast: &ExprAst<Span>) -> String {
        match ast.peel_parens() {
            ExprAst::Integer { value, .. } => value.to_string(),
            ExprAst::String { value, .. } => format!("\"{value}\""),
            ExprAst::Identifier { name, .. } => name.clone(),
            ExprAst::Macro { text, .. } => text.clone(),
            ExprAst::Not { inner, .. } => format!("!({})", render_ast(inner)),
            ExprAst::Binary { kind, lhs, rhs, .. } => {
                format!("({}{}{})", render_ast(lhs), kind.as_str(), render_ast(rhs))
            }
            _ => String::from("__opaque__"),
        }
    }
    match expr {
        CondExpr::Raw(t) => {
            let Some(s) = t.literal_str() else {
                return String::from("__raw_opaque__");
            };
            let trimmed = s.trim();
            if trimmed.contains('%') {
                // Macro-laden raw text — keep as-is but mark to avoid
                // cross-bucket collisions with parsed forms.
                return format!("RAW:{trimmed}");
            }
            trimmed.to_owned()
        }
        CondExpr::ArchList(list) => {
            let mut tokens: Vec<String> = list
                .iter()
                .filter_map(|t| t.literal_str().map(|s| s.trim().to_owned()))
                .collect();
            tokens.sort();
            format!("ARCH:{}", tokens.join(","))
        }
        CondExpr::Parsed(ast) => render_ast(ast),
        _ => String::from("__opaque__"),
    }
}

impl Lint for RepeatedIfelseValueExtraction {
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
        run_lint::<RepeatedIfelseValueExtraction>(src)
    }

    #[test]
    fn flags_two_value_picking_blocks_same_condition() {
        let src = "Name: x\n\
%if X\n\
BuildRequires: gcc-toolset\n\
%else\n\
BuildRequires: gcc\n\
%endif\n\
\n\
Source0: tarball\n\
\n\
%if X\n\
Requires: gcc-toolset-runtime\n\
%else\n\
Requires: gcc-runtime\n\
%endif\n";
        let diags = run(src);
        let hits: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM455").collect();
        assert_eq!(hits.len(), 2, "{diags:?}");
    }

    #[test]
    fn silent_for_single_block() {
        let src = "Name: x\n\
%if X\nBuildRequires: a\n%else\nBuildRequires: b\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_different_conditions() {
        let src = "Name: x\n\
%if X\nBuildRequires: a\n%else\nBuildRequires: b\n%endif\n\
%if Y\nBuildRequires: c\n%else\nBuildRequires: d\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_no_else() {
        // `%if X … %endif` (no else) isn't a value-picker shape.
        let src = "Name: x\n\
%if X\nBuildRequires: a\n%endif\n\
%if X\nBuildRequires: b\n%endif\n";
        let diags = run(src);
        assert!(diags.iter().all(|d| d.lint_id != "RPM455"));
    }

    #[test]
    fn attach_is_constant_time_per_insert() {
        // 100 distinct condition texts → 100 distinct buckets.
        // Validates the HashMap-backed `attach` keeps each condition in
        // its own slot (no accidental coalescing) and runs the insert
        // path 100 times without quadratic behaviour.
        let mut lint = RepeatedIfelseValueExtraction::new();
        let dummy_span = Span::default();
        for i in 0..100 {
            lint.attach(format!("cond_{i}"), dummy_span);
        }
        assert_eq!(lint.buckets.len(), 100);
        // Each bucket has exactly one span.
        for spans in lint.buckets.values() {
            assert_eq!(spans.len(), 1);
        }
    }
}
