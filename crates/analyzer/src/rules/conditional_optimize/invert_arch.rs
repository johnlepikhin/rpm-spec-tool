//! RPM082 `invert-empty-if-arch` — `%ifarch X %else FOO %endif` →
//! `%ifnarch X FOO %endif`. Only for arch/os branch kinds.

use rpm_spec::ast::{CondKind, Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static INVERT_EMPTY_IF_ARCH_METADATA: LintMetadata = LintMetadata {
    id: "RPM082",
    name: "invert-empty-if-arch",
    description: "`%ifarch X %else FOO %endif` — empty `%if` branch with content in `%else`; flip kind.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct InvertEmptyIfArch {
    diagnostics: Vec<Diagnostic>,
}

impl InvertEmptyIfArch {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span, hint_kind: &'static str) {
        self.diagnostics.push(
            Diagnostic::new(
                &INVERT_EMPTY_IF_ARCH_METADATA,
                Severity::Warn,
                format!("flip `%{hint_kind}` to its negation and drop the empty branch"),
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "rewrite to use the opposite arch/os keyword without `%else`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

/// `Some(opposite_keyword)` when `kind` is one of the arch/os branch
/// kinds we can flip.
fn flippable_arch_kind(kind: CondKind) -> Option<&'static str> {
    match kind {
        CondKind::IfArch => Some("ifnarch"),
        CondKind::IfNArch => Some("ifarch"),
        CondKind::IfOs => Some("ifnos"),
        CondKind::IfNOs => Some("ifos"),
        _ => None,
    }
}

fn check_invert_empty<B>(
    node: &Conditional<Span, B>,
    branch_filler: impl Fn(&B) -> bool,
    branch_real: impl Fn(&B) -> bool,
) -> Option<&'static str> {
    // Must be exactly one branch + a non-empty `%else`.
    if node.branches.len() != 1 {
        return None;
    }
    let other = node.otherwise.as_ref()?;
    let head = &node.branches[0];
    let hint = flippable_arch_kind(head.kind)?;
    // `%if` body must be empty (only blanks/comments).
    if !head.body.iter().all(&branch_filler) {
        return None;
    }
    // `%else` body must have real content.
    if !other.iter().any(&branch_real) {
        return None;
    }
    Some(hint)
}

impl<'ast> Visit<'ast> for InvertEmptyIfArch {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        if let Some(hint) = check_invert_empty(
            node,
            |i| matches!(i, SpecItem::Blank | SpecItem::Comment(_)),
            |i| !matches!(i, SpecItem::Blank | SpecItem::Comment(_)),
        ) {
            self.emit(node.data, hint);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        if let Some(hint) = check_invert_empty(
            node,
            |i| matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)),
            |i| !matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)),
        ) {
            self.emit(node.data, hint);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        if let Some(hint) = check_invert_empty(
            node,
            |i| matches!(i, FilesContent::Blank | FilesContent::Comment(_)),
            |i| !matches!(i, FilesContent::Blank | FilesContent::Comment(_)),
        ) {
            self.emit(node.data, hint);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for InvertEmptyIfArch {
    fn metadata(&self) -> &'static LintMetadata {
        &INVERT_EMPTY_IF_ARCH_METADATA
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
    fn rpm082_flags_empty_ifarch_branch() {
        let src = "Name: x\n%ifarch x86_64\n%else\nBuildArch: noarch\n%endif\n";
        let diags = run(src, InvertEmptyIfArch::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM082");
        assert!(diags[0].message.contains("ifnarch"));
    }

    #[test]
    fn rpm082_silent_for_plain_if() {
        // Only fires on arch/os kinds.
        let src = "Name: x\n%if 1\n%else\nBuildArch: noarch\n%endif\n";
        assert!(run(src, InvertEmptyIfArch::new()).is_empty());
    }

    #[test]
    fn rpm082_silent_when_if_branch_has_content() {
        let src = "Name: x\n%ifarch x86_64\nVersion: 1\n%else\nBuildArch: noarch\n%endif\n";
        assert!(run(src, InvertEmptyIfArch::new()).is_empty());
    }

    #[test]
    fn rpm082_silent_when_no_else() {
        let src = "Name: x\n%ifarch x86_64\n%endif\n";
        assert!(run(src, InvertEmptyIfArch::new()).is_empty());
    }
}
