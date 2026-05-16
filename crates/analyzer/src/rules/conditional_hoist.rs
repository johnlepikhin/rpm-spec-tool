//! Phase 7c hoisting lints (RPM097/098).
//!
//! Both rules look at a `%if`/`%elif`/`%else` block and find the
//! longest common prefix / suffix that **every** branch shares —
//! including the `%else` arm when present. When at least one item is
//! common across all branches, the hint is to lift it out of the
//! conditional (before `%if` for a prefix, after `%endif` for a
//! suffix), shrinking the block and de-duplicating the lines that
//! had to be copied into every arm.
//!
//! **Auto-fix v1:** Manual. The Edit construction needs careful
//! newline handling at the boundaries (insertion line, item
//! separators, trailing whitespace) — leaving that for a follow-up
//! sweep. The detection itself, anchored at the conditional span,
//! is the bulk of the value.
//!
//! ## Comparison semantics
//!
//! Two items are "common" when their source-byte ranges, line-trim
//! normalised, are equal. This catches typical RPM patterns where
//! the same `BuildRequires:` line is copy-pasted into every arm of a
//! distribution-flavour switch.

use rpm_spec::ast::{Conditional, FilesContent, PreambleContent, Span, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::conditional_merge::HasBodySpan;
use crate::visit::{self, Visit};

// =====================================================================
// Shared utilities
// =====================================================================

/// Source slice covered by one body item, line-trim-end normalised
/// so trailing whitespace differences don't hide a duplicate.
fn item_text<B: HasBodySpan>(item: &B, source: &str) -> Option<String> {
    let sp = item.body_span()?;
    let slice = source.get(sp.start_byte..sp.end_byte)?;
    Some(
        slice
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Collect bodies as slices, including `%else`.
fn all_bodies<B>(node: &Conditional<Span, B>) -> Vec<&[B]> {
    let mut out: Vec<&[B]> = node.branches.iter().map(|b| b.body.as_slice()).collect();
    if let Some(other) = &node.otherwise {
        out.push(other.as_slice());
    }
    out
}

/// Number of items at the start that appear in every body. Bails
/// out (returns 0) when any body lacks a comparable span at the
/// candidate position.
fn common_prefix_len<B: HasBodySpan>(bodies: &[&[B]], source: &str) -> usize {
    if bodies.is_empty() || bodies.iter().any(|b| b.is_empty()) {
        return 0;
    }
    let min_len = bodies.iter().map(|b| b.len()).min().unwrap_or(0);
    let mut k = 0;
    while k < min_len {
        let Some(first) = item_text(&bodies[0][k], source) else {
            break;
        };
        let all_equal = bodies[1..]
            .iter()
            .all(|b| matches!(item_text(&b[k], source), Some(t) if t == first));
        if !all_equal {
            break;
        }
        k += 1;
    }
    k
}

/// Number of items at the end shared by every body. Mirror of
/// [`common_prefix_len`].
fn common_suffix_len<B: HasBodySpan>(bodies: &[&[B]], source: &str) -> usize {
    if bodies.is_empty() || bodies.iter().any(|b| b.is_empty()) {
        return 0;
    }
    let min_len = bodies.iter().map(|b| b.len()).min().unwrap_or(0);
    let mut k = 0;
    while k < min_len {
        let Some(first) = item_text(&bodies[0][bodies[0].len() - 1 - k], source) else {
            break;
        };
        let all_equal = bodies[1..].iter().all(|b| {
            let idx = b.len() - 1 - k;
            matches!(item_text(&b[idx], source), Some(t) if t == first)
        });
        if !all_equal {
            break;
        }
        k += 1;
    }
    k
}

/// If hoisting would leave at least one branch empty, suggest only
/// when there's room — otherwise the user might prefer a different
/// refactor (e.g. dropping the whole block via RPM073). Returns
/// `true` when all branches have *more* items than the prefix/suffix
/// length, so something remains in each.
fn at_least_one_item_remains<B>(bodies: &[&[B]], k: usize) -> bool {
    bodies.iter().all(|b| b.len() > k)
}

// =====================================================================
// RPM097 hoist-common-prefix-from-branches
// =====================================================================

pub static HOIST_PREFIX_METADATA: LintMetadata = LintMetadata {
    id: "RPM097",
    name: "hoist-common-prefix-from-branches",
    description: "All branches of this conditional start with the same item(s); lift them above the block.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct HoistCommonPrefix {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl HoistCommonPrefix {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span, count: usize) {
        self.diagnostics.push(
            Diagnostic::new(
                &HOIST_PREFIX_METADATA,
                Severity::Warn,
                format!(
                    "all branches start with the same {count} item(s) — \
                     hoist them above the `%if` block"
                ),
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "move the shared prefix outside the conditional and drop it from each branch",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl HoistCommonPrefix {
    fn check<B: HasBodySpan>(&mut self, node: &Conditional<Span, B>) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let bodies = all_bodies(node);
        // Need at least two arms (branch + branch or branch + else).
        if bodies.len() < 2 {
            return;
        }
        let k = common_prefix_len::<B>(&bodies, &source);
        if k == 0 || !at_least_one_item_remains::<B>(&bodies, k) {
            return;
        }
        self.emit(node.data, k);
    }
}

impl<'ast> Visit<'ast> for HoistCommonPrefix {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for HoistCommonPrefix {
    fn metadata(&self) -> &'static LintMetadata {
        &HOIST_PREFIX_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }
}

// =====================================================================
// RPM098 hoist-common-suffix-from-branches
// =====================================================================

pub static HOIST_SUFFIX_METADATA: LintMetadata = LintMetadata {
    id: "RPM098",
    name: "hoist-common-suffix-from-branches",
    description: "All branches of this conditional end with the same item(s); lift them below the block.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct HoistCommonSuffix {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl HoistCommonSuffix {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span, count: usize) {
        self.diagnostics.push(
            Diagnostic::new(
                &HOIST_SUFFIX_METADATA,
                Severity::Warn,
                format!(
                    "all branches end with the same {count} item(s) — \
                     hoist them below the `%endif`"
                ),
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "move the shared suffix after `%endif` and drop it from each branch",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl HoistCommonSuffix {
    fn check<B: HasBodySpan>(&mut self, node: &Conditional<Span, B>) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let bodies = all_bodies(node);
        if bodies.len() < 2 {
            return;
        }
        let k = common_suffix_len::<B>(&bodies, &source);
        if k == 0 || !at_least_one_item_remains::<B>(&bodies, k) {
            return;
        }
        self.emit(node.data, k);
    }
}

impl<'ast> Visit<'ast> for HoistCommonSuffix {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for HoistCommonSuffix {
    fn metadata(&self) -> &'static LintMetadata {
        &HOIST_SUFFIX_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ---- RPM097 prefix ----

    #[test]
    fn rpm097_flags_common_buildrequires_prefix() {
        let src = "Name: x\n%if A\n\
                   BuildRequires: common\n\
                   BuildRequires: a-specific\n\
                   %else\n\
                   BuildRequires: common\n\
                   BuildRequires: b-specific\n\
                   %endif\n";
        let diags = run(src, HoistCommonPrefix::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM097");
    }

    #[test]
    fn rpm097_silent_when_branches_differ_from_start() {
        let src = "Name: x\n%if A\n\
                   BuildRequires: a\n\
                   %else\n\
                   BuildRequires: b\n\
                   %endif\n";
        assert!(run(src, HoistCommonPrefix::new()).is_empty());
    }

    #[test]
    fn rpm097_silent_when_no_else() {
        // Single branch — nothing to hoist against.
        let src = "Name: x\n%if A\nBuildRequires: common\n%endif\n";
        assert!(run(src, HoistCommonPrefix::new()).is_empty());
    }

    #[test]
    fn rpm097_silent_when_hoist_would_empty_branch() {
        // Identical bodies — RPM074 (identical branches) would
        // already trigger; this rule stays silent so we don't
        // double-report.
        let src = "Name: x\n%if A\n\
                   BuildRequires: common\n\
                   %else\n\
                   BuildRequires: common\n\
                   %endif\n";
        assert!(run(src, HoistCommonPrefix::new()).is_empty());
    }

    // ---- RPM098 suffix ----

    #[test]
    fn rpm098_flags_common_suffix() {
        let src = "Name: x\n%if A\n\
                   BuildRequires: a-specific\n\
                   BuildRequires: common\n\
                   %else\n\
                   BuildRequires: b-specific\n\
                   BuildRequires: common\n\
                   %endif\n";
        let diags = run(src, HoistCommonSuffix::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM098");
    }

    #[test]
    fn rpm098_silent_when_no_common_suffix() {
        let src = "Name: x\n%if A\n\
                   BuildRequires: common\n\
                   BuildRequires: a\n\
                   %else\n\
                   BuildRequires: common\n\
                   BuildRequires: b\n\
                   %endif\n";
        assert!(run(src, HoistCommonSuffix::new()).is_empty());
    }
}
