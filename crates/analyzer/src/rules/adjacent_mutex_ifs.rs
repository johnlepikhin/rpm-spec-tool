//! RPM444 `adjacent-mutually-exclusive-ifs-to-elif` — flag two adjacent
//! top-level `%if` blocks whose conditions are mutually exclusive.
//!
//! ```text
//! %if A
//! …
//! %endif
//! %if !A && B
//! …
//! %endif
//! ```
//! The second block fires only when the first didn't, so the pair
//! collapses to `%if A … %elif B … %endif`. The compiler-inserted
//! `¬A` of the elif handles the negation implicitly.
//!
//! Generalises RPM084 (`if-not-x-after-if-x`), which is restricted to
//! the trivial `%ifarch X` / `%ifnarch X` pairing.

use rpm_spec::ast::{
    CondKind, Conditional, FilesContent, PreambleContent, Span, SpecFile, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::boolean_dnf::AtomTable;
use crate::rules::conditional_merge::{HasBodySpan, bodies_source_eq};
use crate::rules::path_cond::{cond_to_dnf, conjoin, is_unsat};
use crate::rules::util::cond_expr_resolvably_eq;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM444",
    name: "adjacent-mutually-exclusive-ifs-to-elif",
    description: "Two adjacent `%if` blocks have mutually exclusive conditions — merge into one \
                  `%if` / `%elif` chain.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Two adjacent `%if` blocks have mutually exclusive conditions — merge into one `%if` / `%elif` chain.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct AdjacentMutexIfs {
    diagnostics: Vec<Diagnostic>,
    source: std::sync::Arc<str>,
}

impl AdjacentMutexIfs {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_pair<B: HasBodySpan>(
        &mut self,
        prev: &Conditional<Span, B>,
        curr: &Conditional<Span, B>,
    ) {
        if !is_simple_if(prev) || !is_simple_if(curr) {
            return;
        }
        let prev_expr = &prev.branches[0].expr;
        let curr_expr = &curr.branches[0].expr;
        if cond_expr_resolvably_eq(prev_expr, curr_expr) {
            return; // RPM076 territory
        }
        // Same body → RPM445 handles that; this rule targets different bodies.
        if bodies_source_eq(&prev.branches[0].body, &curr.branches[0].body, &self.source) {
            return;
        }
        let mut atoms = AtomTable::new();
        let Some(prev_dnf) = cond_to_dnf(prev_expr, &mut atoms, false) else {
            return;
        };
        let Some(curr_dnf) = cond_to_dnf(curr_expr, &mut atoms, false) else {
            return;
        };
        let Some(both) = conjoin(&prev_dnf, &curr_dnf) else {
            return;
        };
        if !is_unsat(&both) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                "adjacent `%if` blocks have mutually exclusive conditions; fold the second \
                 into an `%elif` of the first",
                curr.data,
            )
            .with_suggestion(Suggestion::new(
                "rewrite as `%if A … %elif B … %endif`",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

fn is_simple_if<B>(node: &Conditional<Span, B>) -> bool {
    node.branches.len() == 1
        && matches!(node.branches[0].kind, CondKind::If)
        && node.otherwise.is_none()
}

fn scan_adjacent<B>(
    items: &[B],
    rule: &mut AdjacentMutexIfs,
    extract: impl Fn(&B) -> Option<&Conditional<Span, B>>,
) where
    B: HasBodySpan,
{
    let mut prev: Option<&Conditional<Span, B>> = None;
    for item in items {
        match extract(item) {
            Some(c) => {
                if let Some(p) = prev {
                    rule.check_pair(p, c);
                }
                prev = Some(c);
            }
            #[allow(clippy::collapsible_match)]
            None => {
                if item.body_span().is_some() {
                    prev = None;
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for AdjacentMutexIfs {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        scan_top(self, &spec.items);
        recurse(self, &spec.items);
    }
}

fn scan_top(rule: &mut AdjacentMutexIfs, items: &[SpecItem<Span>]) {
    scan_adjacent::<SpecItem<Span>>(items, rule, |it| {
        if let SpecItem::Conditional(c) = it {
            Some(c)
        } else {
            None
        }
    });
}

fn scan_preamble(rule: &mut AdjacentMutexIfs, items: &[PreambleContent<Span>]) {
    scan_adjacent::<PreambleContent<Span>>(items, rule, |it| {
        if let PreambleContent::Conditional(c) = it {
            Some(c)
        } else {
            None
        }
    });
}

fn scan_files(rule: &mut AdjacentMutexIfs, items: &[FilesContent<Span>]) {
    scan_adjacent::<FilesContent<Span>>(items, rule, |it| {
        if let FilesContent::Conditional(c) = it {
            Some(c)
        } else {
            None
        }
    });
}

fn recurse(rule: &mut AdjacentMutexIfs, items: &[SpecItem<Span>]) {
    for it in items {
        match it {
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    scan_top(rule, &branch.body);
                    recurse(rule, &branch.body);
                }
                if let Some(els) = &c.otherwise {
                    scan_top(rule, els);
                    recurse(rule, els);
                }
            }
            SpecItem::Section(boxed) => match boxed.as_ref() {
                rpm_spec::ast::Section::Package { content, .. } => {
                    scan_preamble(rule, content);
                    recurse_preamble(rule, content);
                }
                rpm_spec::ast::Section::Files { content, .. } => {
                    scan_files(rule, content);
                    recurse_files(rule, content);
                }
                _ => {}
            },
            _ => {}
        }
    }
}

fn recurse_preamble(rule: &mut AdjacentMutexIfs, items: &[PreambleContent<Span>]) {
    for it in items {
        if let PreambleContent::Conditional(c) = it {
            for branch in &c.branches {
                scan_preamble(rule, &branch.body);
                recurse_preamble(rule, &branch.body);
            }
            if let Some(els) = &c.otherwise {
                scan_preamble(rule, els);
                recurse_preamble(rule, els);
            }
        }
    }
}

fn recurse_files(rule: &mut AdjacentMutexIfs, items: &[FilesContent<Span>]) {
    for it in items {
        if let FilesContent::Conditional(c) = it {
            for branch in &c.branches {
                scan_files(rule, &branch.body);
                recurse_files(rule, &branch.body);
            }
            if let Some(els) = &c.otherwise {
                scan_files(rule, els);
                recurse_files(rule, els);
            }
        }
    }
}

impl Lint for AdjacentMutexIfs {
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
        run_lint::<AdjacentMutexIfs>(src)
    }

    #[test]
    fn flags_a_and_not_a_pair_with_different_bodies() {
        let src = "Name: x\n\
%if A\n\
License: MIT\n\
%endif\n\
%if !A\n\
License: GPL\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM444");
    }

    #[test]
    fn flags_a_and_not_a_and_b() {
        let src = "Name: x\n\
%if A\n\
License: MIT\n\
%endif\n\
%if !A && B\n\
License: GPL\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn silent_for_compatible_conditions() {
        let src = "Name: x\n\
%if A\n\
License: MIT\n\
%endif\n\
%if B\n\
License: GPL\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_same_body_pair() {
        // RPM445's territory.
        let src = "Name: x\n\
%if A\n\
License: MIT\n\
%endif\n\
%if !A\n\
License: MIT\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_same_condition() {
        // RPM076's territory.
        let src = "Name: x\n\
%if A\n\
License: MIT\n\
%endif\n\
%if A\n\
License: GPL\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_blocks_separated_by_other_item() {
        let src = "Name: x\n\
%if A\nLicense: MIT\n%endif\n\
Version: 1\n\
%if !A\nLicense: GPL\n%endif\n";
        assert!(run(src).is_empty());
    }
}
