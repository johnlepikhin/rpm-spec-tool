//! RPM438 `empty-optional-macro-arm` — flag adjacent optional-macro
//! pairs where one arm expands to nothing.
//!
//! `%{?foo:bar}%{!?foo:}` is the same as `%{?foo:bar}`: the second
//! macro covers the "foo not defined" case but supplies an empty body,
//! which is the default behaviour of an absent macro reference. The
//! redundant arm is dead syntax.

use rpm_spec::ast::{
    ConditionalMacro, MacroKind, MacroRef, PreambleItem, Scriptlet, Section, Span, Text,
    TextSegment, Trigger,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM438",
    name: "empty-optional-macro-arm",
    description: "Adjacent `%{?foo:…}%{!?foo:…}` pair where one arm is empty — the empty arm \
                  is a no-op and can be dropped.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Adjacent `%{?foo:…}%{!?foo:…}` pair where one arm is empty — the empty arm is a no-op and can be dropped.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct EmptyOptionalMacroArm {
    diagnostics: Vec<Diagnostic>,
    /// Current anchor span — preamble / section / scriptlet / trigger.
    anchor: Option<Span>,
}

impl EmptyOptionalMacroArm {
    pub fn new() -> Self {
        Self::default()
    }

    fn scan(&mut self, text: &Text) {
        let Some(anchor) = self.anchor else { return };
        let segs = &text.segments;
        for i in 0..segs.len().saturating_sub(1) {
            let TextSegment::Macro(a) = &segs[i] else {
                continue;
            };
            let TextSegment::Macro(b) = &segs[i + 1] else {
                continue;
            };
            if !is_adjacent_pair(a, b) {
                continue;
            }
            let empty_a = arm_is_empty(a);
            let empty_b = arm_is_empty(b);
            // If both arms are empty, RPM494's case (full no-op); skip.
            // If neither, no empty arm to drop; skip.
            if empty_a == empty_b {
                continue;
            }
            let which = if empty_b { "second" } else { "first" };
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "adjacent `%{{?N:…}}%{{!?N:…}}` pair has an empty {which} arm; \
                         drop the empty arm — it is a no-op"
                    ),
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "remove the empty-arm optional-macro reference",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

/// `true` when `a` and `b` form an `%{?N:…}` + `%{!?N:…}` pair with the
/// same `N` (or the symmetric flip).
fn is_adjacent_pair(a: &MacroRef, b: &MacroRef) -> bool {
    if a.name != b.name {
        return false;
    }
    if !matches!(a.kind, MacroKind::Plain | MacroKind::Braced)
        || !matches!(b.kind, MacroKind::Plain | MacroKind::Braced)
    {
        return false;
    }
    if !a.args.is_empty() || !b.args.is_empty() {
        return false;
    }
    matches!(
        (&a.conditional, &b.conditional),
        (ConditionalMacro::IfDefined, ConditionalMacro::IfNotDefined)
            | (ConditionalMacro::IfNotDefined, ConditionalMacro::IfDefined)
    )
}

/// `true` when the macro's arm body (the bit after `:`) is literally empty
/// (`Some("")`). The `None` shape — `%{?foo}` without `:body` — is the
/// conditional-expansion form (expand to value if defined), NOT an empty arm.
fn arm_is_empty(m: &MacroRef) -> bool {
    match m.with_value.as_ref() {
        None => false,
        Some(t) => t.segments.is_empty() || t.literal_str().is_some_and(|s| s.is_empty()),
    }
}

impl<'ast> Visit<'ast> for EmptyOptionalMacroArm {
    fn visit_preamble(&mut self, node: &'ast PreambleItem<Span>) {
        let prev = self.anchor.replace(node.data);
        visit::walk_preamble(self, node);
        self.anchor = prev;
    }

    fn visit_section(&mut self, node: &'ast Section<Span>) {
        let prev = match node {
            Section::BuildScript { data, .. }
            | Section::Verify { data, .. }
            | Section::Sepolicy { data, .. } => self.anchor.replace(*data),
            _ => self.anchor,
        };
        visit::walk_section(self, node);
        self.anchor = prev;
    }

    fn visit_scriptlet(&mut self, node: &'ast Scriptlet<Span>) {
        let prev = self.anchor.replace(node.data);
        visit::walk_scriptlet(self, node);
        self.anchor = prev;
    }

    fn visit_trigger(&mut self, node: &'ast Trigger<Span>) {
        let prev = self.anchor.replace(node.data);
        visit::walk_trigger(self, node);
        self.anchor = prev;
    }

    fn visit_text(&mut self, node: &'ast Text) {
        self.scan(node);
        visit::walk_text(self, node);
    }
}

impl Lint for EmptyOptionalMacroArm {
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
        run_lint::<EmptyOptionalMacroArm>(src)
    }

    #[test]
    fn flags_pair_with_empty_neg_arm() {
        // `%{?foo:bar}%{!?foo:}` — empty `!?` arm.
        let src = "Name: x\nVersion: %{?foo:bar}%{!?foo:}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM438");
    }

    #[test]
    fn flags_pair_with_empty_pos_arm() {
        // `%{?foo:}%{!?foo:bar}` — empty `?` arm.
        let src = "Name: x\nVersion: %{?foo:}%{!?foo:bar}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_for_unpaired_macros() {
        let src = "Name: x\nVersion: %{?foo:bar}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_both_arms_present() {
        // `%{?foo:a}%{!?foo:b}` — both arms non-empty; the pair is
        // doing real conditional work.
        let src = "Name: x\nVersion: %{?foo:a}%{!?foo:b}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_both_arms_empty() {
        // Both empty is RPM494's case (drop entirely); RPM438 stays silent.
        let src = "Name: x\nVersion: %{?foo:}%{!?foo:}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_first_macro_lacks_arm_body() {
        // `%{?version_override}` has with_value=None — that is the
        // conditional-expansion form ("expand to value if defined"), NOT
        // an empty arm. Adjacent `%{!?version_override:260.1}` supplies
        // the default. No diagnostic is appropriate.
        let src = "Name: x\nVersion: %{?version_override}%{!?version_override:260.1}\n";
        let diags = run(src);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn silent_for_different_macro_names() {
        let src = "Name: x\nVersion: %{?foo:bar}%{!?baz:}\n";
        assert!(run(src).is_empty());
    }
}
