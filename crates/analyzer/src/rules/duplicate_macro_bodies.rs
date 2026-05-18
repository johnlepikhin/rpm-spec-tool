//! RPM497 `duplicate-macro-bodies` — flag two or more top-level macro
//! definitions whose bodies are identical but whose names differ.
//!
//! ```text
//! %global compiler gcc
//! %global cc       gcc
//! ```
//! Both expand to `gcc`; one of them is dead weight. Pick a single
//! canonical name and remove the other.

use std::collections::BTreeMap;

use rpm_spec::ast::{MacroDef, MacroDefKind, Span, SpecFile, SpecItem, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM497",
    name: "duplicate-macro-bodies",
    description: "Two or more macro definitions share the same body — consolidate to one \
                  canonical name.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Bodies shorter than this don't justify the diagnostic — short
/// literal bodies are too noisy.
const MIN_BODY_LEN: usize = 3;

/// Two or more macro definitions share the same body — consolidate to one canonical name.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct DuplicateMacroBodies {
    diagnostics: Vec<Diagnostic>,
}

impl DuplicateMacroBodies {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for DuplicateMacroBodies {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut buckets: BTreeMap<String, Vec<(&str, Span)>> = BTreeMap::new();
        for it in &spec.items {
            let SpecItem::MacroDef(md) = it else {
                continue;
            };
            if matches!(md.kind, MacroDefKind::Undefine) {
                continue;
            }
            // Skip parametric macros — their bodies often share
            // template structure but the parameter mangling differs.
            if md.opts.is_some() {
                continue;
            }
            let body = canonical_body(md);
            if body.len() < MIN_BODY_LEN {
                continue;
            }
            buckets
                .entry(body)
                .or_default()
                .push((md.name.as_str(), md.data));
        }
        for (_body, entries) in buckets {
            // Need at least two DISTINCT names with the same body.
            let mut names: Vec<&&str> = entries.iter().map(|(n, _)| n).collect();
            names.sort();
            names.dedup();
            if names.len() < 2 {
                continue;
            }
            for (name, span) in &entries {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        METADATA.default_severity,
                        format!(
                            "macro `{name}` shares its body with another macro definition in \
                             this spec — pick one canonical name"
                        ),
                        *span,
                    )
                    .with_suggestion(Suggestion::new(
                        "delete the duplicate definition and redirect call sites to the \
                         retained name",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn canonical_body(md: &MacroDef<Span>) -> String {
    let mut out = String::new();
    for seg in &md.body.segments {
        match seg {
            TextSegment::Literal(s) => out.push_str(s),
            TextSegment::Macro(m) => {
                use rpm_spec::ast::MacroKind;
                match m.kind {
                    MacroKind::Braced => out.push_str(&format!("%{{{}}}", m.name)),
                    MacroKind::Plain => out.push_str(&format!("%{}", m.name)),
                    _ => out.push_str(&format!("%{{{}}}", m.name)),
                }
            }
            _ => {}
        }
    }
    out.trim().to_owned()
}

impl Lint for DuplicateMacroBodies {
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
        run_lint::<DuplicateMacroBodies>(src)
    }

    #[test]
    fn flags_two_macros_with_same_body() {
        let src = "Name: x\n%global compiler gcc-toolset-13\n%global cc gcc-toolset-13\n";
        let diags = run(src);
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(diags.iter().all(|d| d.lint_id == "RPM497"));
    }

    #[test]
    fn silent_for_unique_bodies() {
        let src = "Name: x\n%global compiler gcc\n%global cc clang\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_single_def() {
        let src = "Name: x\n%global compiler gcc-toolset-13\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_very_short_body() {
        // Bodies shorter than MIN_BODY_LEN are too noisy.
        let src = "Name: x\n%global a 1\n%global b 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_parametric_macros() {
        // Parametric macros aren't compared — template overlap is normal.
        let src = "Name: x\n%define helper(x:) echo %{x}\n%define helper2(x:) echo %{x}\n";
        assert!(run(src).is_empty());
    }
}
