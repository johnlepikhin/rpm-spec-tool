//! RPM452 `complementary-guards-same-item` — flag dependency atoms
//! that appear under guards covering the whole assignment space.
//!
//! ```text
//! %if A
//! BuildRequires: foo
//! %endif
//! %if !A
//! BuildRequires: foo
//! %endif
//! ```
//! Whatever the truth value of `A`, exactly one of the two `%if`
//! branches fires — so `foo` is effectively unconditional. The pair
//! can be replaced by a single unconditional `BuildRequires: foo`.
//!
//! Two or more guarded copies of the same atom collectively cover ⊤
//! when their path-DNFs' union is a tautology. This generalises beyond
//! the simple `A` / `!A` pair to any complete cover (e.g. three
//! `%ifarch` blocks listing every supported arch).

use std::collections::{BTreeMap, BTreeSet};

use rpm_spec::ast::{
    DepExpr, PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem, TagValue,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::boolean_dnf::{Cube, Dnf, is_tautology};
use crate::rules::path_cond::{
    MAX_PATH_STACK_DEPTH, PathConditions, branch_effective_dnf, compute_frame, else_effective_dnf,
};
use crate::rules::util::{DepTagKey, dep_atom_text};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM452",
    name: "complementary-guards-same-item",
    description: "Multiple guarded copies of the same dependency atom collectively cover every \
                  truth assignment; the atom is effectively unconditional — merge them.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug)]
struct Occurrence {
    tag: DepTagKey,
    atom_text: String,
    path: Dnf,
    span: Span,
}

/// Multiple guarded copies of the same dependency atom collectively cover every truth assignment; the atom is effectively unconditional — merge them.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ComplementaryGuardsSameItem {
    diagnostics: Vec<Diagnostic>,
}

impl ComplementaryGuardsSameItem {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ComplementaryGuardsSameItem {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut pc = PathConditions::default();
        let mut occs: Vec<Occurrence> = Vec::new();
        walk_top_items(&spec.items, &mut pc, &mut occs);
        emit(&occs, &pc, &mut self.diagnostics);

        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Package { content, .. } = boxed.as_ref() else {
                continue;
            };
            let mut sub_pc = PathConditions::default();
            let mut sub_occs: Vec<Occurrence> = Vec::new();
            walk_preamble_content(content, &mut sub_pc, &mut sub_occs);
            emit(&sub_occs, &sub_pc, &mut self.diagnostics);
        }
    }
}

fn record_preamble(item: &PreambleItem<Span>, pc: &PathConditions, occs: &mut Vec<Occurrence>) {
    let Some(tag) = DepTagKey::from_tag(&item.tag) else {
        return;
    };
    let TagValue::Dep(expr) = &item.value else {
        return;
    };
    let DepExpr::Atom(atom) = expr else {
        return;
    };
    let Some(atom_text) = dep_atom_text(atom) else {
        return;
    };
    // Only guarded items are interesting — an unconditional item is
    // already optimal and RPM450 would have flagged any conditional
    // dup of it.
    if pc.stack.is_empty() {
        return;
    }
    let Some(path) = pc.current() else {
        return; // tainted — can't reason
    };
    if path.is_empty() {
        return; // unsat path; RPM113 handles
    }
    occs.push(Occurrence {
        tag,
        atom_text,
        path: path.clone(),
        span: item.data,
    });
}

fn walk_top_items(items: &[SpecItem<Span>], pc: &mut PathConditions, occs: &mut Vec<Occurrence>) {
    if pc.stack.len() >= MAX_PATH_STACK_DEPTH {
        return;
    }
    for it in items {
        match it {
            SpecItem::Preamble(p) => record_preamble(p, pc, occs),
            SpecItem::Conditional(c) => {
                let mut prior: Vec<&rpm_spec::ast::CondExpr<Span>> = Vec::new();
                for branch in &c.branches {
                    let eff = branch_effective_dnf(&branch.expr, &prior, &mut pc.atoms);
                    let frame = compute_frame(pc.current(), eff);
                    pc.push(frame);
                    walk_top_items(&branch.body, pc, occs);
                    pc.pop();
                    prior.push(&branch.expr);
                }
                if let Some(els) = &c.otherwise {
                    let eff = else_effective_dnf(&prior, &mut pc.atoms);
                    let frame = compute_frame(pc.current(), eff);
                    pc.push(frame);
                    walk_top_items(els, pc, occs);
                    pc.pop();
                }
            }
            _ => {}
        }
    }
}

fn walk_preamble_content(
    items: &[PreambleContent<Span>],
    pc: &mut PathConditions,
    occs: &mut Vec<Occurrence>,
) {
    if pc.stack.len() >= MAX_PATH_STACK_DEPTH {
        return;
    }
    for it in items {
        match it {
            PreambleContent::Item(p) => record_preamble(p, pc, occs),
            PreambleContent::Conditional(c) => {
                let mut prior: Vec<&rpm_spec::ast::CondExpr<Span>> = Vec::new();
                for branch in &c.branches {
                    let eff = branch_effective_dnf(&branch.expr, &prior, &mut pc.atoms);
                    let frame = compute_frame(pc.current(), eff);
                    pc.push(frame);
                    walk_preamble_content(&branch.body, pc, occs);
                    pc.pop();
                    prior.push(&branch.expr);
                }
                if let Some(els) = &c.otherwise {
                    let eff = else_effective_dnf(&prior, &mut pc.atoms);
                    let frame = compute_frame(pc.current(), eff);
                    pc.push(frame);
                    walk_preamble_content(els, pc, occs);
                    pc.pop();
                }
            }
            _ => {}
        }
    }
}

fn emit(occs: &[Occurrence], pc: &PathConditions, diagnostics: &mut Vec<Diagnostic>) {
    let atom_count = pc.atoms.len();
    let mut buckets: BTreeMap<(DepTagKey, &str), Vec<&Occurrence>> = BTreeMap::new();
    for o in occs {
        buckets
            .entry((o.tag, o.atom_text.as_str()))
            .or_default()
            .push(o);
    }
    for (_key, group) in buckets {
        if group.len() < 2 {
            continue;
        }
        let union: BTreeSet<Cube> = group.iter().flat_map(|o| o.path.iter().cloned()).collect();
        if !matches!(is_tautology(&union, atom_count), Some(true)) {
            continue;
        }
        // Emit once per occurrence in the group.
        for o in &group {
            diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`{label}: {atom}` is paired with complementary guards covering every \
                         assignment — make it unconditional",
                        label = o.tag.label(),
                        atom = o.atom_text,
                    ),
                    o.span,
                )
                .with_suggestion(Suggestion::new(
                    "drop the guards and list this entry once at the top level",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

impl Lint for ComplementaryGuardsSameItem {
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
        run_lint::<ComplementaryGuardsSameItem>(src)
    }

    #[test]
    fn flags_a_and_not_a_pair() {
        let src = "Name: x\n\
%if A\n\
BuildRequires: foo\n\
%endif\n\
%if !A\n\
BuildRequires: foo\n\
%endif\n";
        let diags = run(src);
        // Two occurrences, both diagnosed.
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(diags.iter().all(|d| d.lint_id == "RPM452"));
    }

    #[test]
    fn silent_for_non_covering_pair() {
        // A and B don't cover ⊤ (both can be false simultaneously).
        let src = "Name: x\n\
%if A\n\
BuildRequires: foo\n\
%endif\n\
%if B\n\
BuildRequires: foo\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_overlapping_but_not_total_pair() {
        // A and A&&B → union = A, not ⊤.
        let src = "Name: x\n\
%if A\n\
BuildRequires: foo\n\
%endif\n\
%if A && B\n\
BuildRequires: foo\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_single_occurrence() {
        let src = "Name: x\n%if A\nBuildRequires: foo\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn fires_in_subpackage() {
        let src = "Name: x\n\
%package devel\n\
Summary: devel\n\
%if A\nRequires: foo\n%endif\n\
%if !A\nRequires: foo\n%endif\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 2, "{diags:?}");
    }
}
