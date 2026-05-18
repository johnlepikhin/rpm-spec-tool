//! RPM456 `branch-item-subset` — flag conditionals where one branch's
//! items are a strict subset of another's.
//!
//! When the items don't depend on each other's order (commutative
//! context such as preamble dep tags), the common subset can be hoisted
//! above the conditional, leaving only the truly branch-specific bits
//! inside.
//!
//! Restricted to preamble-level conditionals; shell-body and `%files`
//! conditionals are not commutative in general (order changes
//! `%defattr` / install behaviour) and are deliberately skipped.

use std::collections::HashMap;

use rpm_spec::ast::{Conditional, PreambleContent, Span};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
// `body_text` not used here — we read each item's span directly, which
// is simpler for the multiset comparison.
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM456",
    name: "branch-item-subset",
    description: "One branch of a conditional contains a strict subset of another branch's items \
                  — hoist the shared items above the conditional.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// One branch of a conditional contains a strict subset of another branch's items — hoist the shared items above the conditional.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct BranchItemSubset {
    diagnostics: Vec<Diagnostic>,
    source: std::sync::Arc<str>,
}

impl BranchItemSubset {
    pub fn new() -> Self {
        Self::default()
    }

    fn check(&mut self, node: &Conditional<Span, PreambleContent<Span>>) {
        // Need at least two branches (or one + else) for a subset claim.
        let total_branches = node.branches.len() + usize::from(node.otherwise.is_some());
        if total_branches < 2 {
            return;
        }
        // Per-branch item lines, sorted-as-set.
        let mut sets: Vec<(Vec<String>, Span)> = Vec::with_capacity(total_branches);
        for branch in &node.branches {
            let Some(set) = canonical_lines(&branch.body, &self.source) else {
                return;
            };
            if set.is_empty() {
                return;
            }
            sets.push((set, branch.data));
        }
        if let Some(els) = node.otherwise.as_deref() {
            let Some(set) = canonical_lines(els, &self.source) else {
                return;
            };
            if set.is_empty() {
                return;
            }
            sets.push((set, node.data));
        }
        // Find any (A, B) pair where A's multiset is a strict submultiset
        // of B's. Emit once per conditional (don't spam every pair).
        for (i, (set_a, _)) in sets.iter().enumerate() {
            for (j, (set_b, _)) in sets.iter().enumerate() {
                if i == j {
                    continue;
                }
                if is_strict_submultiset(set_a, set_b) {
                    self.diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            Severity::Warn,
                            "one branch's items are a strict subset of another's — hoist the \
                             shared items above the conditional",
                            sets[i].1,
                        )
                        .with_suggestion(Suggestion::new(
                            "lift the shared items out; keep only the branch-specific ones inside",
                            Vec::new(),
                            Applicability::Manual,
                        )),
                    );
                    return;
                }
            }
        }
    }
}

/// Sorted line texts of a body, ignoring blank / comment items. Returns
/// `None` if any item is something we don't track (conditional, etc.) —
/// nested control flow defeats the simple set comparison.
fn canonical_lines(body: &[PreambleContent<Span>], source: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for it in body {
        match it {
            PreambleContent::Item(p) => {
                let text = item_text(Some(p.data), source)?;
                out.push(text);
            }
            PreambleContent::Blank | PreambleContent::Comment(_) => {}
            // Nested conditional inside a branch — bail out; we can't
            // canonicalise its set without recursive expansion.
            PreambleContent::Conditional(_) => return None,
            _ => return None,
        }
    }
    out.sort();
    Some(out)
}

fn item_text(span: Option<Span>, source: &str) -> Option<String> {
    let sp = span?;
    let end = sp.end_byte.min(source.len());
    let start = sp.start_byte.min(end);
    let slice = source.get(start..end)?;
    // Trim trailing whitespace + newline so cosmetic differences don't
    // hide a true subset relationship.
    Some(slice.trim_end().to_owned())
}

fn is_strict_submultiset(a: &[String], b: &[String]) -> bool {
    if a.len() >= b.len() {
        return false;
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for x in b {
        *counts.entry(x.as_str()).or_insert(0) += 1;
    }
    for x in a {
        match counts.get_mut(x.as_str()) {
            Some(n) if *n > 0 => *n -= 1,
            _ => return false,
        }
    }
    true
}

impl<'ast> Visit<'ast> for BranchItemSubset {
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
}

impl Lint for BranchItemSubset {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: std::sync::Arc<str>) {
        self.source = source;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<BranchItemSubset>(src)
    }

    #[test]
    fn flags_branch_subset_of_else() {
        // %if branch has {foo}; %else has {foo, bar}. {foo} ⊂ {foo, bar}.
        let src = "Name: x\n\
%package devel\n\
Summary: dev\n\
%if A\n\
Requires: foo\n\
%else\n\
Requires: foo\n\
Requires: bar\n\
%endif\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM456");
    }

    #[test]
    fn flags_branch_subset_of_other_branch() {
        let src = "Name: x\n\
%package devel\n\
Summary: dev\n\
%if A\n\
Requires: foo\n\
Requires: bar\n\
Requires: baz\n\
%elif B\n\
Requires: foo\n\
Requires: bar\n\
%endif\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_for_independent_branches() {
        let src = "Name: x\n\
%package devel\n\
Summary: dev\n\
%if A\nRequires: foo\n%else\nRequires: bar\n%endif\n\
%description devel\nbody\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_equal_sets() {
        // Identical sets — not a STRICT subset. RPM074 covers this.
        let src = "Name: x\n\
%package devel\n\
Summary: dev\n\
%if A\nRequires: foo\n%else\nRequires: foo\n%endif\n\
%description devel\nbody\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_single_branch() {
        let src = "Name: x\n\
%package devel\n\
Summary: dev\n\
%if A\nRequires: foo\n%endif\n\
%description devel\nbody\n";
        assert!(run(src).is_empty());
    }
}
