//! RPM032 `macro-redefinition` — defining the same `%global` /
//! `%define` macro twice at the **same scope** in one spec is almost
//! always a copy-paste mistake: rpm honours the last one and the
//! earlier definition is dead code.
//!
//! ### Conditional branches are alternatives, not redefinitions
//!
//! ```rpm
//! %if 0%{?fedora}
//! %global flavor fedora
//! %else
//! %global flavor centos
//! %endif
//! ```
//!
//! is **not** flagged: only one branch executes at build time, so the
//! two `%global` lines never collide. We track macro definitions per
//! conditional branch independently and merge them back as a union
//! after `%endif`, never as a sequence.
//!
//! `%undefine` is treated as a deliberate stack pop and clears the
//! seen-set, so a subsequent `%define` is a fresh definition.

use std::collections::HashMap;

use rpm_spec::ast::{Conditional, MacroDef, MacroDefKind, Span, SpecFile, SpecItem};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM032",
    name: "macro-redefinition",
    description: "A macro is redefined at the same scope; the earlier definition is dead code. \
                  Definitions in alternative %if/%else branches are not redefinitions and are ignored.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// A macro is redefined at the same scope; the earlier definition is dead code. Definitions in alternative %if/%else branches are not redefinitions and are ignored.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MacroRedefinition {
    diagnostics: Vec<Diagnostic>,
}

impl MacroRedefinition {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MacroRedefinition {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut seen: HashMap<String, Span> = HashMap::new();
        walk_items(&spec.items, &mut seen, &mut self.diagnostics);
    }
}

fn walk_items(
    items: &[SpecItem<Span>],
    seen: &mut HashMap<String, Span>,
    out: &mut Vec<Diagnostic>,
) {
    for item in items {
        match item {
            SpecItem::MacroDef(m) => check_macro_def(m, seen, out),
            SpecItem::Conditional(c) => walk_conditional(c, out),
            _ => {}
        }
    }
}

fn walk_conditional(cond: &Conditional<Span, SpecItem<Span>>, out: &mut Vec<Diagnostic>) {
    // Each branch is treated as its own isolated scope: it does *not*
    // inherit the parent's seen-set. This avoids two common false
    // positives:
    //
    // 1. `%global foo 1` at top level, then `%if cond %global foo 2 %endif`
    //    — the second line is a conditional override, not a bug. Only
    //    one of the lines wins at build time in any given configuration.
    //
    // 2. `%if A %global foo 1 %endif %if B %global foo 2 %endif`
    //    — the two `%if` blocks are mutually exclusive alternatives.
    //
    // Trade-off: a true bug like
    //    `%if outer %global foo 1 %if inner %global foo 2 %endif %endif`
    // where both `%global` lines execute on the `outer && inner` path
    // is treated as silent. We accept that — the false-positive rate
    // of the alternative ("inherit parent") is far worse on real specs.
    //
    // A redefinition *within* one branch (same `%if`, two `%global foo`)
    // is still caught.
    for branch in &cond.branches {
        let mut branch_seen: HashMap<String, Span> = HashMap::new();
        walk_items(&branch.body, &mut branch_seen, out);
    }
    if let Some(els) = &cond.otherwise {
        let mut branch_seen: HashMap<String, Span> = HashMap::new();
        walk_items(els, &mut branch_seen, out);
    }
}

fn check_macro_def(
    m: &MacroDef<Span>,
    seen: &mut HashMap<String, Span>,
    out: &mut Vec<Diagnostic>,
) {
    // `%undefine` pops the macro stack — subsequent `%define` is a
    // fresh definition, not a redefinition.
    if matches!(m.kind, MacroDefKind::Undefine) {
        seen.remove(&m.name);
        return;
    }
    match seen.get(&m.name) {
        None => {
            seen.insert(m.name.clone(), m.data);
        }
        Some(&first) => {
            out.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!("macro `{}` is redefined", m.name),
                    m.data,
                )
                .with_label(first, "previously defined here"),
            );
        }
    }
}

impl Lint for MacroRedefinition {
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
        run_lint::<MacroRedefinition>(src)
    }

    #[test]
    fn flags_double_define_at_top_level() {
        let diags = run("%define foo 1\n%define foo 2\nName: x\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM032");
        assert_eq!(diags[0].labels.len(), 1);
    }

    #[test]
    fn flags_global_after_define_at_top_level() {
        let diags = run("%define foo 1\n%global foo 2\nName: x\n");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_undefine_redefine_pair() {
        assert!(run("%define foo 1\n%undefine foo\n%define foo 2\nName: x\n").is_empty(),);
    }

    #[test]
    fn silent_for_distinct_names() {
        assert!(run("%define foo 1\n%define bar 2\nName: x\n").is_empty());
    }

    #[test]
    fn silent_for_if_else_alternatives() {
        // The two branches define the same macro but only one executes.
        // This is the canonical "conditional %global" pattern.
        let src = "%if 0%{?fedora}\n\
%global flavor fedora\n\
%else\n\
%global flavor centos\n\
%endif\n\
Name: x\n";
        assert!(run(src).is_empty(), "alternatives must not flag");
    }

    #[test]
    fn silent_for_multiple_separate_if_blocks() {
        // Each `%if ... %endif` introduces an alternative tree; the
        // macro is `defined-in-some-branch` after the first block, but
        // the subsequent block also redefines only inside an
        // alternative, so still no collision.
        let src = "%if 1\n%define jit 1\n%else\n%define jit 0\n%endif\n\
%if 2\n%define jit 1\n%else\n%define jit 0\n%endif\n\
Name: x\n";
        assert!(
            run(src).is_empty(),
            "consecutive %if blocks are alternatives at top level"
        );
    }

    #[test]
    fn flags_redefinition_inside_same_branch() {
        // Two `%define foo` inside the *same* %if branch is a real bug.
        let src = "%if 1\n%define foo 1\n%define foo 2\n%endif\nName: x\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_nested_conditionals_documented_tradeoff() {
        // Regression lock for the trade-off documented in `walk_conditional`:
        // `%if outer %define foo 1 %if inner %define foo 2 %endif %endif`
        // would actually redefine `foo` at build time when `outer && inner`,
        // but flagging it would require global scope tracking and would
        // trigger many false positives on idiomatic specs. We accept the
        // silent false-negative and pin it here.
        let src = "%if 1\n\
%define foo 1\n\
%if 1\n\
%define foo 2\n\
%endif\n\
%endif\n\
Name: x\n";
        assert!(
            run(src).is_empty(),
            "nested if/define pattern is silently accepted by design"
        );
    }

    #[test]
    fn silent_for_conditional_default_plus_top_level_override() {
        // Common idiom: set a default inside %if, optionally override
        // unconditionally afterwards. The post-%if line always runs
        // and "wins", which is exactly what the maintainer intended.
        let src = "%if 1\n%define foo 1\n%endif\n%define foo 2\nName: x\n";
        assert!(
            run(src).is_empty(),
            "conditional + top-level override is idiomatic"
        );
    }
}
