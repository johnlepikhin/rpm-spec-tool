//! RPM437 `optional-macro-boolean-shortening` — flag the verbose
//! `%{?N:1}%{!?N:0}` boolean-presence idiom.
//!
//! Spec authors frequently want "expand to 1 if NAME is defined, 0
//! otherwise". The longhand pairs `%{?N:1}` with `%{!?N:0}`; the
//! standard shorthand is `0%{?N:1}` — a leading literal `0` plus the
//! conditional expansion. Both yield `"01"` (i.e. integer 1) when
//! defined and `"0"` when not.
//!
//! Sibling to RPM438 (`empty-optional-macro-arm`): RPM438 catches the
//! case where one arm is empty; RPM437 catches the 0/1 pair where
//! both arms are populated but the pair is the boolean idiom.

use rpm_spec::ast::{
    ConditionalMacro, MacroKind, MacroRef, PreambleItem, Scriptlet, Section, Span, Text,
    TextSegment, Trigger,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM437",
    name: "optional-macro-boolean-shortening",
    description: "`%{?N:1}%{!?N:0}` is the verbose form of the macro-presence test; use the \
                  shorter `0%{?N:1}` idiom instead.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// `%{?N:1}%{!?N:0}` is the verbose form of the macro-presence test; use the shorter `0%{?N:1}` idiom instead.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct OptionalMacroBooleanShortening {
    diagnostics: Vec<Diagnostic>,
    anchor: Option<Span>,
}

impl OptionalMacroBooleanShortening {
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
            let Some((name, shortened)) = match_boolean_pair(a, b) else {
                continue;
            };
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "verbose `%{{?{name}:…}}%{{!?{name}:…}}` boolean pair — use the \
                         shorter `{shortened}` idiom"
                    ),
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    format!("replace the pair with `{shortened}`"),
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

/// Match a `%{?N:X}%{!?N:Y}` pair where `{X, Y} = {"1", "0"}`. Returns
/// the macro name and the suggested shorthand.
fn match_boolean_pair(a: &MacroRef, b: &MacroRef) -> Option<(String, String)> {
    if a.name != b.name {
        return None;
    }
    if !matches!(a.kind, MacroKind::Plain | MacroKind::Braced)
        || !matches!(b.kind, MacroKind::Plain | MacroKind::Braced)
    {
        return None;
    }
    if !a.args.is_empty() || !b.args.is_empty() {
        return None;
    }
    let (pos_val, neg_val) = match (&a.conditional, &b.conditional) {
        (ConditionalMacro::IfDefined, ConditionalMacro::IfNotDefined) => {
            (arm_text(a)?, arm_text(b)?)
        }
        (ConditionalMacro::IfNotDefined, ConditionalMacro::IfDefined) => {
            (arm_text(b)?, arm_text(a)?)
        }
        _ => return None,
    };
    match (pos_val.as_str(), neg_val.as_str()) {
        ("1", "0") => Some((a.name.clone(), format!("0%{{?{}:1}}", a.name))),
        ("0", "1") => Some((a.name.clone(), format!("0%{{!?{}:1}}", a.name))),
        _ => None,
    }
}

fn arm_text(m: &MacroRef) -> Option<String> {
    let t = m.with_value.as_ref()?;
    let s = t.literal_str()?.trim();
    Some(s.to_owned())
}

impl<'ast> Visit<'ast> for OptionalMacroBooleanShortening {
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

impl Lint for OptionalMacroBooleanShortening {
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
        run_lint::<OptionalMacroBooleanShortening>(src)
    }

    #[test]
    fn flags_pos1_neg0_pair() {
        let src = "Name: x\nVersion: %{?foo:1}%{!?foo:0}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM437");
        assert!(diags[0].message.contains("0%{?foo:1}"));
    }

    #[test]
    fn flags_pos0_neg1_pair() {
        let src = "Name: x\nVersion: %{?foo:0}%{!?foo:1}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("0%{!?foo:1}"));
    }

    #[test]
    fn silent_for_non_boolean_values() {
        let src = "Name: x\nVersion: %{?foo:a}%{!?foo:b}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_unpaired_macro() {
        let src = "Name: x\nVersion: %{?foo:1}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_names_differ() {
        let src = "Name: x\nVersion: %{?foo:1}%{!?bar:0}\n";
        assert!(run(src).is_empty());
    }
}
