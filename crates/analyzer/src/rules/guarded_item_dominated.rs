//! RPM451 `guarded-item-dominated-by-weaker-guard` — flag dependency
//! atoms whose `%if` guard is strictly stronger than another guard
//! around the same atom in the same package + tag scope.
//!
//! ```text
//! %if A
//! BuildRequires: foo
//! %endif
//! %if A && B
//! BuildRequires: foo
//! %endif
//! ```
//! The second `BuildRequires: foo` only fires under `A && B`, but the
//! first already fires under the strictly weaker guard `A` — so the
//! second is dead. The rule emits one diagnostic per dominated copy.
//!
//! Companion to RPM450 (`guarded-item-already-unconditional`), which
//! handles the special case of "weaker guard = unconditional". RPM451
//! covers the general case via DNF implication.

use std::collections::BTreeMap;

use rpm_spec::ast::{
    DepExpr, PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem, TagValue,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::boolean_dnf::Dnf;
use crate::rules::path_cond::{
    MAX_PATH_STACK_DEPTH, PathConditions, branch_effective_dnf, compute_frame, else_effective_dnf,
    path_implies,
};
use crate::rules::util::{DepTagKey, dep_atom_text};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM451",
    name: "guarded-item-dominated-by-weaker-guard",
    description: "A guarded dependency atom is dominated by another copy of the same atom \
                  under a strictly weaker guard — drop the redundant stronger-guarded copy.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug)]
struct Occurrence {
    tag: DepTagKey,
    atom_text: String,
    /// `None` for unconditional items (handled by RPM450) — we never
    /// emit on those. `Some(dnf)` otherwise.
    path: Option<Dnf>,
    span: Span,
}

/// A guarded dependency atom is dominated by another copy of the same atom under a strictly weaker guard — drop the redundant stronger-guarded copy.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct GuardedItemDominated {
    diagnostics: Vec<Diagnostic>,
}

impl GuardedItemDominated {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for GuardedItemDominated {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Main package.
        let mut pc = PathConditions::default();
        let mut occs: Vec<Occurrence> = Vec::new();
        walk_top_items(&spec.items, &mut pc, &mut occs);
        emit(&occs, &pc, &mut self.diagnostics);

        // Each `%package` subpackage.
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
    let path = if pc.stack.is_empty() {
        None
    } else {
        pc.current().cloned()
    };
    occs.push(Occurrence {
        tag,
        atom_text,
        path,
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
    // Bucket by (tag, atom_text) for O(N) comparison within each
    // bucket; whole-spec N² over independent atoms is wasted work.
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
        for (i, x) in group.iter().enumerate() {
            let Some(x_path) = &x.path else {
                continue; // unconditional — RPM450 handles
            };
            let mut dominated = false;
            for (j, y) in group.iter().enumerate() {
                if i == j {
                    continue;
                }
                let Some(y_path) = &y.path else {
                    // unconditional Y — RPM450 already fires; skip
                    continue;
                };
                // x dominated by y iff every model of x_path is a
                // model of y_path AND y_path is not symmetric.
                if matches!(path_implies(x_path, y_path, atom_count), Some(true))
                    && !matches!(path_implies(y_path, x_path, atom_count), Some(true))
                {
                    dominated = true;
                    break;
                }
            }
            if dominated {
                diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        format!(
                            "`{label}: {atom}` is dominated by another guard around the same \
                             atom in this package; drop the stronger-guarded copy",
                            label = x.tag.label(),
                            atom = x.atom_text,
                        ),
                        x.span,
                    )
                    .with_suggestion(Suggestion::new(
                        "remove this entry; the weaker-guard copy already covers it",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl Lint for GuardedItemDominated {
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
        run_lint::<GuardedItemDominated>(src)
    }

    #[test]
    fn flags_stronger_guard_dominated_by_weaker() {
        // `%if A BR foo` + `%if A && B BR foo` — second is dominated.
        let src = "Name: x\n\
%if A\n\
BuildRequires: foo\n\
%endif\n\
%if A && B\n\
BuildRequires: foo\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM451");
    }

    #[test]
    fn silent_for_equal_guards() {
        // Same guard on both → mutual implication → no dominance.
        // (RPM320 territory.)
        let src = "Name: x\n\
%if A\n\
BuildRequires: foo\n\
%endif\n\
%if A\n\
BuildRequires: foo\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_independent_guards() {
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
    fn silent_when_one_unconditional() {
        // Unconditional + conditional → RPM450's territory; this rule
        // must not double-warn.
        let src = "Name: x\nBuildRequires: foo\n%if A\nBuildRequires: foo\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn fires_inside_subpackage() {
        let src = "Name: x\n\
%package devel\n\
Summary: devel\n\
%if A\n\
Requires: foo\n\
%endif\n\
%if A && B\n\
Requires: foo\n\
%endif\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_for_different_atoms() {
        let src = "Name: x\n\
%if A\n\
BuildRequires: foo\n\
%endif\n\
%if A && B\n\
BuildRequires: bar\n\
%endif\n";
        assert!(run(src).is_empty());
    }
}
