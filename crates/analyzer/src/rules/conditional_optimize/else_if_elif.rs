//! RPM100 `collapse-else-if-into-elif` — `%else` containing a single
//! `%if` block can be folded into an `%elif`, dropping one nesting
//! level.

use rpm_spec::ast::{CondKind, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static COLLAPSE_ELSE_IF_METADATA: LintMetadata = LintMetadata {
    id: "RPM100",
    name: "collapse-else-if-into-elif",
    description: "`%else` containing a single `%if` block can be folded into an `%elif` — \
         drops one nesting level.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct CollapseElseIfIntoElif {
    diagnostics: Vec<Diagnostic>,
}

impl CollapseElseIfIntoElif {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &COLLAPSE_ELSE_IF_METADATA,
                Severity::Warn,
                "`%else` contains a single nested `%if` — collapse into `%elif`",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "rewrite the `%else` + nested `%if` as `%elif <inner-cond>`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

/// `Some(inner_span)` when `body` (the `%else` arm) holds exactly one
/// real item and that item is a `%if` block whose head is plain
/// `CondKind::If`. The inner may itself have `%elif` arms and/or an
/// `%else` — in those cases the whole inner chain (including its own
/// trailing `%else`) merges cleanly into the outer chain via
/// `%elif <inner-cond>`. Filler items (Blank/Comment) are tolerated.
fn else_holds_single_if_top(body: &[SpecItem<Span>]) -> Option<Span> {
    let mut non_filler = body
        .iter()
        .filter(|i| !matches!(i, SpecItem::Blank | SpecItem::Comment(_)));
    let SpecItem::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if !matches!(inner.branches.first()?.kind, CondKind::If) {
        return None;
    }
    Some(inner.data)
}

fn else_holds_single_if_preamble(body: &[PreambleContent<Span>]) -> Option<Span> {
    let mut non_filler = body
        .iter()
        .filter(|i| !matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)));
    let PreambleContent::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if !matches!(inner.branches.first()?.kind, CondKind::If) {
        return None;
    }
    Some(inner.data)
}

fn else_holds_single_if_files(body: &[FilesContent<Span>]) -> Option<Span> {
    let mut non_filler = body
        .iter()
        .filter(|i| !matches!(i, FilesContent::Blank | FilesContent::Comment(_)));
    let FilesContent::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if !matches!(inner.branches.first()?.kind, CondKind::If) {
        return None;
    }
    Some(inner.data)
}

impl<'ast> Visit<'ast> for CollapseElseIfIntoElif {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        if let Some(body) = &node.otherwise
            && else_holds_single_if_top(body).is_some()
        {
            self.emit(node.data);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        if let Some(body) = &node.otherwise
            && else_holds_single_if_preamble(body).is_some()
        {
            self.emit(node.data);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        if let Some(body) = &node.otherwise
            && else_holds_single_if_files(body).is_some()
        {
            self.emit(node.data);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for CollapseElseIfIntoElif {
    fn metadata(&self) -> &'static LintMetadata {
        &COLLAPSE_ELSE_IF_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Diagnostic;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn rpm100_flags_else_holding_single_if() {
        let src = "Name: x\n%if A\nLicense: MIT\n%else\n%if B\nLicense: GPL\n%endif\n%endif\n";
        let diags = run(src, CollapseElseIfIntoElif::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM100");
    }

    #[test]
    fn rpm100_silent_when_else_has_more_content() {
        let src = "Name: x\n%if A\nLicense: MIT\n%else\nLicense: BSD\n%if B\nLicense: GPL\n%endif\n%endif\n";
        assert!(run(src, CollapseElseIfIntoElif::new()).is_empty());
    }

    #[test]
    fn rpm100_silent_when_no_else() {
        let src = "Name: x\n%if A\n%if B\nLicense: GPL\n%endif\n%endif\n";
        assert!(run(src, CollapseElseIfIntoElif::new()).is_empty());
    }
}
