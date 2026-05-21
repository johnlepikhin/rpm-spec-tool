//! RPM446 `same-body-arch-blocks-to-arch-list` — flag two adjacent
//! `%ifarch` blocks that wrap the same body but list different arches.
//!
//! ```text
//! %ifarch x86_64
//! BuildRequires: foo
//! %endif
//! %ifarch aarch64
//! BuildRequires: foo
//! %endif
//! ```
//! collapses to a single `%ifarch x86_64 aarch64 … %endif`.
//!
//! Specialisation of RPM445 (`same-body-different-conditions-merge`);
//! RPM445 deliberately skips the `%ifarch` / `%ifarch` case so this
//! rule owns it with the correct merge syntax (space-separated arch
//! list, not `||`).

use rpm_spec::ast::{
    CondExpr, CondKind, Conditional, FilesContent, PreambleContent, Span, SpecFile, SpecItem, Text,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::conditional_merge::{HasBodySpan, bodies_source_eq};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM446",
    name: "same-body-arch-blocks-to-arch-list",
    description: "Two adjacent `%ifarch` blocks have the same body but different arch lists — \
                  merge into one block listing all arches.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Two adjacent `%ifarch` blocks have the same body but different arch lists — merge into one block listing all arches.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct SameBodyArchBlocks {
    diagnostics: Vec<Diagnostic>,
    source: std::sync::Arc<str>,
}

impl SameBodyArchBlocks {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_pair<B: HasBodySpan>(
        &mut self,
        prev: &Conditional<Span, B>,
        curr: &Conditional<Span, B>,
    ) {
        if !is_simple_ifarch(prev) || !is_simple_ifarch(curr) {
            return;
        }
        let prev_archs = arch_list(&prev.branches[0].expr);
        let curr_archs = arch_list(&curr.branches[0].expr);
        let (Some(prev_archs), Some(curr_archs)) = (prev_archs, curr_archs) else {
            return;
        };
        // Identical lists → RPM076 covers that.
        if same_set(prev_archs, curr_archs) {
            return;
        }
        if !bodies_source_eq(&prev.branches[0].body, &curr.branches[0].body, &self.source) {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                "adjacent `%ifarch` blocks share the same body but list different arches; \
                 merge into one `%ifarch` with both arch lists combined",
                curr.data,
            )
            .with_suggestion(Suggestion::new(
                "merge as `%ifarch <arch1> <arch2> … <archN>` with one body",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

fn is_simple_ifarch<B>(node: &Conditional<Span, B>) -> bool {
    node.branches.len() == 1
        && matches!(node.branches[0].kind, CondKind::IfArch)
        && node.otherwise.is_none()
}

fn arch_list(expr: &CondExpr<Span>) -> Option<&[Text]> {
    match expr {
        CondExpr::ArchList(list) => Some(list),
        _ => None,
    }
}

fn same_set(a: &[Text], b: &[Text]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut a_lits: Vec<&str> = a.iter().filter_map(|t| t.literal_str()).collect();
    let mut b_lits: Vec<&str> = b.iter().filter_map(|t| t.literal_str()).collect();
    a_lits.sort();
    b_lits.sort();
    a_lits == b_lits
}

fn scan_adjacent<B>(
    items: &[B],
    rule: &mut SameBodyArchBlocks,
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

impl<'ast> Visit<'ast> for SameBodyArchBlocks {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        scan_top(self, &spec.items);
        recurse(self, &spec.items);
    }
}

fn scan_top(rule: &mut SameBodyArchBlocks, items: &[SpecItem<Span>]) {
    scan_adjacent::<SpecItem<Span>>(items, rule, |it| {
        if let SpecItem::Conditional(c) = it {
            Some(c)
        } else {
            None
        }
    });
}

fn scan_preamble(rule: &mut SameBodyArchBlocks, items: &[PreambleContent<Span>]) {
    scan_adjacent::<PreambleContent<Span>>(items, rule, |it| {
        if let PreambleContent::Conditional(c) = it {
            Some(c)
        } else {
            None
        }
    });
}

fn scan_files(rule: &mut SameBodyArchBlocks, items: &[FilesContent<Span>]) {
    scan_adjacent::<FilesContent<Span>>(items, rule, |it| {
        if let FilesContent::Conditional(c) = it {
            Some(c)
        } else {
            None
        }
    });
}

fn recurse(rule: &mut SameBodyArchBlocks, items: &[SpecItem<Span>]) {
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

fn recurse_preamble(rule: &mut SameBodyArchBlocks, items: &[PreambleContent<Span>]) {
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

fn recurse_files(rule: &mut SameBodyArchBlocks, items: &[FilesContent<Span>]) {
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

impl Lint for SameBodyArchBlocks {
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
        run_lint::<SameBodyArchBlocks>(src)
    }

    #[test]
    fn flags_adjacent_arch_blocks_same_body() {
        let src = "Name: x\n\
%ifarch x86_64\n\
BuildRequires: foo\n\
%endif\n\
%ifarch aarch64\n\
BuildRequires: foo\n\
%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM446");
    }

    #[test]
    fn silent_for_different_bodies() {
        let src = "Name: x\n\
%ifarch x86_64\n\
BuildRequires: foo\n\
%endif\n\
%ifarch aarch64\n\
BuildRequires: bar\n\
%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_same_arch_list() {
        let src = "Name: x\n\
%ifarch x86_64\nBuildRequires: foo\n%endif\n\
%ifarch x86_64\nBuildRequires: foo\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_mixed_ifarch_and_if() {
        let src = "Name: x\n\
%ifarch x86_64\nBuildRequires: foo\n%endif\n\
%if A\nBuildRequires: foo\n%endif\n";
        assert!(run(src).is_empty());
    }
}
