//! RPM499 `macro-name-shadows-bcond-helper` — flag local macro names
//! that look like `%bcond` helper accessors (`with_NAME` /
//! `without_NAME`).
//!
//! `%bcond_with tests` doesn't create a `%{with_tests}` macro — the
//! accessors live in the parametric `%{with tests}` namespace. But a
//! local `%global with_tests 1` reads at a glance as if it were the
//! same thing, which mis-trains spec readers and risks subtle bugs
//! when they reach for the wrong reference.

use std::collections::BTreeSet;

use rpm_spec::ast::{MacroDefKind, Span, SpecFile, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM499",
    name: "macro-name-shadows-bcond-helper",
    description: "Local `%global with_NAME` / `%global without_NAME` shadows the visual shape \
                  of `%{with NAME}` / `%{without NAME}` bcond accessors — pick a different \
                  name to avoid confusion.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Local `%global with_NAME` / `%global without_NAME` shadows the visual shape of `%{with NAME}` / `%{without NAME}` bcond accessors — pick a different name to avoid confusion.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MacroNameShadowsBcondHelper {
    diagnostics: Vec<Diagnostic>,
}

impl MacroNameShadowsBcondHelper {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MacroNameShadowsBcondHelper {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // First pass: collect every bcond name declared in the spec.
        let bcond_names = collect_bcond_names(spec);
        if bcond_names.is_empty() {
            return;
        }
        // Second pass: flag any %global / %define whose name matches
        // `with_<NAME>` or `without_<NAME>` for a declared bcond.
        for it in &spec.items {
            let SpecItem::MacroDef(md) = it else {
                continue;
            };
            if matches!(md.kind, MacroDefKind::Undefine) {
                continue;
            }
            let Some((kind, bcond)) = shadow_target(&md.name, &bcond_names) else {
                continue;
            };
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`{def_kind} {name}` shadows the visual shape of `%{{{kind} {bcond}}}` — \
                         pick a name that doesn't collide with the bcond accessor convention",
                        def_kind = match md.kind {
                            MacroDefKind::Global => "%global",
                            MacroDefKind::Define => "%define",
                            _ => "%global",
                        },
                        name = md.name,
                    ),
                    md.data,
                )
                .with_suggestion(Suggestion::new(
                    "rename the local macro (e.g. prefix with the package name) so it doesn't \
                     read as a bcond accessor",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

fn collect_bcond_names(spec: &SpecFile<Span>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for it in &spec.items {
        if let SpecItem::BuildCondition(bc) = it {
            out.insert(bc.name.clone());
        }
    }
    out
}

/// If `name` shadows `with_<N>` or `without_<N>` for a known bcond N,
/// returns `("with"|"without", N)`.
fn shadow_target<'a>(
    name: &str,
    bcond_names: &'a BTreeSet<String>,
) -> Option<(&'static str, &'a str)> {
    if let Some(rest) = name.strip_prefix("with_")
        && let Some(b) = bcond_names.iter().find(|b| b.as_str() == rest)
    {
        return Some(("with", b.as_str()));
    }
    if let Some(rest) = name.strip_prefix("without_")
        && let Some(b) = bcond_names.iter().find(|b| b.as_str() == rest)
    {
        return Some(("without", b.as_str()));
    }
    None
}

impl Lint for MacroNameShadowsBcondHelper {
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
        run_lint::<MacroNameShadowsBcondHelper>(src)
    }

    #[test]
    fn flags_with_name_shadow() {
        let src = "Name: x\n%bcond_with tests\n%global with_tests 1\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM499");
    }

    #[test]
    fn flags_without_name_shadow() {
        let src = "Name: x\n%bcond_without gui\n%global without_gui 0\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_unrelated_macro_name() {
        let src = "Name: x\n%bcond_with tests\n%global my_other 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_no_bcond_declared() {
        let src = "Name: x\n%global with_tests 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_with_prefix_but_name_differs() {
        let src = "Name: x\n%bcond_with tests\n%global with_other 1\n";
        assert!(run(src).is_empty());
    }
}
