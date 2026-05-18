//! RPM494 `no-op-conditional-macro` — flag standalone `%{?foo:}` or
//! `%{!?foo:}` macro references that expand to nothing.
//!
//! `%{?foo:}` says "if `foo` is defined, expand to empty"; the body
//! is empty, so the reference contributes nothing to the surrounding
//! text. Skipped when the reference is part of an `%{?N:…}%{!?N:…}`
//! pair — RPM438 owns that case.

use rpm_spec::ast::{
    ConditionalMacro, MacroKind, MacroRef, PreambleItem, Scriptlet, Section, Span, Text,
    TextSegment, Trigger,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM494",
    name: "no-op-conditional-macro",
    description: "Standalone `%{?foo:}` or `%{!?foo:}` macro reference expands to nothing — \
                  drop the no-op.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Standalone `%{?foo:}` or `%{!?foo:}` macro reference expands to nothing — drop the no-op.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct NoOpConditionalMacro {
    diagnostics: Vec<Diagnostic>,
    anchor: Option<Span>,
}

impl NoOpConditionalMacro {
    pub fn new() -> Self {
        Self::default()
    }

    fn scan(&mut self, text: &Text) {
        let Some(anchor) = self.anchor else { return };
        let segs = &text.segments;
        for (i, seg) in segs.iter().enumerate() {
            let TextSegment::Macro(m) = seg else {
                continue;
            };
            if !is_empty_arm_conditional(m) {
                continue;
            }
            // Skip when an adjacent macro segment is the opposite arm
            // for the same name — RPM438 handles those pairs.
            if has_opposite_arm_neighbour(segs, i, m) {
                continue;
            }
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`%{{{kind}{name}:}}` is a no-op — the arm body is empty and the macro \
                         contributes nothing",
                        kind = if matches!(m.conditional, ConditionalMacro::IfDefined) {
                            "?"
                        } else {
                            "!?"
                        },
                        name = m.name,
                    ),
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "drop the empty-arm conditional macro reference",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

fn is_empty_arm_conditional(m: &MacroRef) -> bool {
    if !matches!(m.kind, MacroKind::Plain | MacroKind::Braced) {
        return false;
    }
    if !matches!(
        m.conditional,
        ConditionalMacro::IfDefined | ConditionalMacro::IfNotDefined
    ) {
        return false;
    }
    if !m.args.is_empty() {
        return false;
    }
    match m.with_value.as_ref() {
        // `%{?foo}` (no `:body`) is the conditional-expansion form — NOT
        // a no-op. Only the `Some(literal "")` shape is the no-op pattern.
        //
        // A `Some(_)` body whose `segments` is empty means the parser
        // could not materialize a multi-line `:body` — that is a bail-out
        // signal, not an empty body. Require `literal_str() == Some("")`.
        None => false,
        Some(t) => t.literal_str().is_some_and(|s| s.is_empty()),
    }
}

fn has_opposite_arm_neighbour(segs: &[TextSegment], i: usize, m: &MacroRef) -> bool {
    let opposite = match m.conditional {
        ConditionalMacro::IfDefined => ConditionalMacro::IfNotDefined,
        ConditionalMacro::IfNotDefined => ConditionalMacro::IfDefined,
        _ => return false,
    };
    let neighbour_is_opposite = |idx: Option<usize>| -> bool {
        let Some(idx) = idx else { return false };
        let TextSegment::Macro(other) = &segs[idx] else {
            return false;
        };
        other.name == m.name
            && matches!(other.conditional, ref c if std::mem::discriminant(c) == std::mem::discriminant(&opposite))
    };
    let prev_idx = i.checked_sub(1);
    let next_idx = if i + 1 < segs.len() {
        Some(i + 1)
    } else {
        None
    };
    neighbour_is_opposite(prev_idx) || neighbour_is_opposite(next_idx)
}

impl<'ast> Visit<'ast> for NoOpConditionalMacro {
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

impl Lint for NoOpConditionalMacro {
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
        run_lint::<NoOpConditionalMacro>(src)
    }

    #[test]
    fn flags_lone_empty_question() {
        let src = "Name: x\nVersion: 1%{?foo:}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM494");
    }

    #[test]
    fn flags_lone_empty_bang_question() {
        let src = "Name: x\nVersion: 1%{!?foo:}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_conditional_expansion_without_arm() {
        // `%{?foo}` (no `:body`) is real conditional expansion, not a no-op.
        let src = "Name: x\nVersion: 1%{?foo}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_populated_arm() {
        let src = "Name: x\nVersion: 1%{?foo:bar}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_paired_with_opposite_arm() {
        // RPM438's territory — don't double-warn.
        let src = "Name: x\nVersion: %{?foo:bar}%{!?foo:}\n";
        assert!(run(src).is_empty());
    }
}
