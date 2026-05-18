//! RPM450 `guarded-item-already-unconditional` — flag dependency
//! atoms that appear both unconditionally and inside an `%if` block
//! of the same package + tag scope.
//!
//! `BuildRequires: foo` at the top combined with
//! ```text
//! %if RHEL
//! BuildRequires: foo
//! %endif
//! ```
//! is dead syntax: the conditional copy never adds anything because the
//! unconditional copy already covers every path. The rule emits one
//! diagnostic per dead-conditional occurrence.
//!
//! Distinct from RPM320 (`duplicate-dependency-atom`), which catches
//! duplicates inside one tag's value list — here the duplicates live on
//! separate `BuildRequires:` lines that the parser splits.

use std::collections::{BTreeMap, BTreeSet};

use rpm_spec::ast::{PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem, TagValue};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{DepTagKey, dep_atom_text};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM450",
    name: "guarded-item-already-unconditional",
    description: "A dependency atom appears both unconditionally and inside an `%if` block of \
                  the same tag; the conditional copy is dead — drop it.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// A dependency atom appears both unconditionally and inside an `%if` block of the same tag; the conditional copy is dead — drop it.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct GuardedItemAlreadyUnconditional {
    diagnostics: Vec<Diagnostic>,
}

impl GuardedItemAlreadyUnconditional {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for GuardedItemAlreadyUnconditional {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Main package: walk top-level SpecItem list.
        let mut main_uncond: BTreeMap<DepTagKey, BTreeSet<String>> = BTreeMap::new();
        let mut main_cond: Vec<CondOccurrence> = Vec::new();
        scan_spec_items(&spec.items, 0, &mut main_uncond, &mut main_cond);
        emit(&main_uncond, &main_cond, &mut self.diagnostics);

        // Subpackages declared as `%package`: walk their PreambleContent.
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Package { content, .. } = boxed.as_ref() else {
                continue;
            };
            let mut sub_uncond: BTreeMap<DepTagKey, BTreeSet<String>> = BTreeMap::new();
            let mut sub_cond: Vec<CondOccurrence> = Vec::new();
            scan_preamble_content(content, 0, &mut sub_uncond, &mut sub_cond);
            emit(&sub_uncond, &sub_cond, &mut self.diagnostics);
        }
    }
}

#[derive(Debug)]
struct CondOccurrence {
    key: DepTagKey,
    atom_text: String,
    span: Span,
}

fn scan_preamble_item(
    item: &PreambleItem<Span>,
    depth: u32,
    uncond: &mut BTreeMap<DepTagKey, BTreeSet<String>>,
    cond: &mut Vec<CondOccurrence>,
) {
    let Some(key) = DepTagKey::from_tag(&item.tag) else {
        return;
    };
    let TagValue::Dep(expr) = &item.value else {
        return;
    };
    // Only flag plain atoms — rich deps have different identity
    // semantics and need their own handling (RPM596/RPM597).
    let rpm_spec::ast::DepExpr::Atom(atom) = expr else {
        return;
    };
    let Some(text) = dep_atom_text(atom) else {
        return;
    };
    if depth == 0 {
        uncond.entry(key).or_default().insert(text);
    } else {
        cond.push(CondOccurrence {
            key,
            atom_text: text,
            span: item.data,
        });
    }
}

fn scan_spec_items(
    items: &[SpecItem<Span>],
    depth: u32,
    uncond: &mut BTreeMap<DepTagKey, BTreeSet<String>>,
    cond: &mut Vec<CondOccurrence>,
) {
    for it in items {
        match it {
            SpecItem::Preamble(p) => scan_preamble_item(p, depth, uncond, cond),
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    scan_spec_items(&branch.body, depth + 1, uncond, cond);
                }
                if let Some(els) = &c.otherwise {
                    scan_spec_items(els, depth + 1, uncond, cond);
                }
            }
            _ => {}
        }
    }
}

fn scan_preamble_content(
    items: &[PreambleContent<Span>],
    depth: u32,
    uncond: &mut BTreeMap<DepTagKey, BTreeSet<String>>,
    cond: &mut Vec<CondOccurrence>,
) {
    for it in items {
        match it {
            PreambleContent::Item(p) => scan_preamble_item(p, depth, uncond, cond),
            PreambleContent::Conditional(c) => {
                for branch in &c.branches {
                    scan_preamble_content(&branch.body, depth + 1, uncond, cond);
                }
                if let Some(els) = &c.otherwise {
                    scan_preamble_content(els, depth + 1, uncond, cond);
                }
            }
            _ => {}
        }
    }
}

fn emit(
    uncond: &BTreeMap<DepTagKey, BTreeSet<String>>,
    cond: &[CondOccurrence],
    diagnostics: &mut Vec<Diagnostic>,
) {
    for occ in cond {
        let Some(set) = uncond.get(&occ.key) else {
            continue;
        };
        if !set.contains(&occ.atom_text) {
            continue;
        }
        diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "`{label}: {atom}` is also listed unconditionally for this package; \
                     the guarded copy is dead — drop it",
                    label = occ.key.label(),
                    atom = occ.atom_text,
                ),
                occ.span,
            )
            .with_suggestion(Suggestion::new(
                "remove the conditional duplicate",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl Lint for GuardedItemAlreadyUnconditional {
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
        run_lint::<GuardedItemAlreadyUnconditional>(src)
    }

    #[test]
    fn flags_when_unconditional_and_conditional_coexist() {
        let src = "Name: x\nBuildRequires: gcc\n\
%if 0%{?rhel}\nBuildRequires: gcc\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM450");
        assert!(diags[0].message.contains("BuildRequires"));
        assert!(diags[0].message.contains("gcc"));
    }

    #[test]
    fn silent_when_only_conditional() {
        let src = "Name: x\n%if 0%{?rhel}\nBuildRequires: gcc\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_only_unconditional() {
        let src = "Name: x\nBuildRequires: gcc\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_across_different_tags() {
        // `Requires:` and `BuildRequires:` are independent — no overlap.
        let src = "Name: x\nRequires: gcc\n%if 0%{?rhel}\nBuildRequires: gcc\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn fires_for_requires_too() {
        let src = "Name: x\nRequires: gcc\n%if 0%{?rhel}\nRequires: gcc\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Requires"));
    }

    #[test]
    fn fires_inside_subpackage() {
        let src = "Name: x\n\
%package devel\n\
Summary: devel\n\
Requires: foo\n\
%if 0%{?rhel}\nRequires: foo\n%endif\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_when_atom_contains_macro() {
        // Macro-resolved name can't be dedup'd safely.
        let src = "Name: x\nBuildRequires: %{libname}\n%if 0%{?rhel}\nBuildRequires: %{libname}\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn fires_for_each_conditional_duplicate() {
        // Two conditional copies of the same unconditional atom → two
        // diagnostics.
        let src = "Name: x\nBuildRequires: gcc\n\
%if 0%{?rhel}\nBuildRequires: gcc\n%endif\n\
%if 0%{?fedora}\nBuildRequires: gcc\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 2, "{diags:?}");
    }

    #[test]
    fn silent_when_atom_differs_in_constraint() {
        // `gcc` vs `gcc >= 10` are different atoms — no dedup.
        let src = "Name: x\nBuildRequires: gcc\n\
%if 0%{?rhel}\nBuildRequires: gcc >= 10\n%endif\n";
        assert!(run(src).is_empty());
    }
}
