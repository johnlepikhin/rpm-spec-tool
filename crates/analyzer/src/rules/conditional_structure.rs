//! Structural lints over `%if`/`%elif`/`%else` blocks (Phase 6 + 7b).
//!
//! Phase 6:
//! - RPM070 `deep-conditional-nesting`
//! - RPM071 `unreachable-elif-branch`
//! - RPM073 `empty-conditional-branch`
//! - RPM077 `ifarch-empty-list`
//!
//! Phase 7b:
//! - RPM089 `single-comment-only-branch` — `%if X # TODO %endif`
//! - RPM090 `ifarch-noarch` — `%ifarch noarch` (noarch is not an arch)
//! - RPM091 `duplicate-arch-in-list` — `%ifarch x86_64 x86_64`
//! - RPM092 `conditional-cyclomatic-complexity` — sum of branches in a
//!   `BuildScript` section above threshold
//!
//! All rules anchor on the conditional block's span. `%if` blocks
//! inside shell-bodies are not part of the AST (the parser keeps them
//! as macros within `ShellBody`), so these rules only see AST-level
//! conditionals.

use rpm_spec::ast::{
    CondBranch, CondExpr, CondKind, Conditional, FilesContent, PreambleContent, Section, Span,
    SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, Edit, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{
    cond_expr_resolvably_eq, is_empty_files_body, is_empty_preamble_body, is_empty_top_body,
};
use crate::visit::{self, Visit};

/// Default nesting threshold for RPM070. Beyond 4 levels a human
/// reviewer loses the thread; `kernel.spec` and `libreoffice.spec` are
/// the canonical examples that justify a configurable knob (deferred
/// to the profile system).
const MAX_NESTING_DEPTH: usize = 4;

// =====================================================================
// RPM070 deep-conditional-nesting
// =====================================================================

pub static DEEP_NESTING_METADATA: LintMetadata = LintMetadata {
    id: "RPM070",
    name: "deep-conditional-nesting",
    description: "Conditional nesting beyond 4 levels is hard to read; refactor or split.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct DeepConditionalNesting {
    diagnostics: Vec<Diagnostic>,
    depth: usize,
}

impl DeepConditionalNesting {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_depth(&mut self, anchor: Span) {
        if self.depth > MAX_NESTING_DEPTH {
            self.diagnostics.push(Diagnostic::new(
                &DEEP_NESTING_METADATA,
                Severity::Warn,
                format!(
                    "conditional nesting depth {} exceeds the recommended maximum of {}",
                    self.depth, MAX_NESTING_DEPTH
                ),
                anchor,
            ));
        }
    }
}

impl<'ast> Visit<'ast> for DeepConditionalNesting {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.depth += 1;
        self.check_depth(node.data);
        visit::walk_top_conditional(self, node);
        self.depth -= 1;
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.depth += 1;
        self.check_depth(node.data);
        visit::walk_preamble_conditional(self, node);
        self.depth -= 1;
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.depth += 1;
        self.check_depth(node.data);
        visit::walk_files_conditional(self, node);
        self.depth -= 1;
    }
}

impl Lint for DeepConditionalNesting {
    fn metadata(&self) -> &'static LintMetadata {
        &DEEP_NESTING_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        self.depth = 0;
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM071 unreachable-elif-branch
// =====================================================================

pub static UNREACHABLE_ELIF_METADATA: LintMetadata = LintMetadata {
    id: "RPM071",
    name: "unreachable-elif-branch",
    description: "`%elif` with the same expression as an earlier branch can never fire; likely a typo.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct UnreachableElifBranch {
    diagnostics: Vec<Diagnostic>,
}

impl UnreachableElifBranch {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_branches<T, B>(&mut self, branches: &[CondBranch<Span, B>]) {
        // First branch is the leading `%if`; each subsequent is `%elif`.
        // An `%elif` with the same expression as any earlier branch
        // is unreachable.
        for (i, branch) in branches.iter().enumerate().skip(1) {
            for prior in &branches[..i] {
                if cond_expr_resolvably_eq(&prior.expr, &branch.expr) {
                    self.diagnostics.push(Diagnostic::new(
                        &UNREACHABLE_ELIF_METADATA,
                        Severity::Warn,
                        "this `%elif` repeats an earlier branch's condition and is unreachable",
                        branch.data,
                    ));
                    break;
                }
            }
        }
        let _ = std::marker::PhantomData::<T>;
    }
}

impl<'ast> Visit<'ast> for UnreachableElifBranch {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check_branches::<Span, _>(&node.branches);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check_branches::<Span, _>(&node.branches);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check_branches::<Span, _>(&node.branches);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for UnreachableElifBranch {
    fn metadata(&self) -> &'static LintMetadata {
        &UNREACHABLE_ELIF_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM073 empty-conditional-branch
// =====================================================================

pub static EMPTY_BRANCH_METADATA: LintMetadata = LintMetadata {
    id: "RPM073",
    name: "empty-conditional-branch",
    description: "Conditional block has no real content in any branch — drop the block.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct EmptyConditionalBranch {
    diagnostics: Vec<Diagnostic>,
}

impl EmptyConditionalBranch {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &EMPTY_BRANCH_METADATA,
                Severity::Warn,
                "conditional block has no content in any branch",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "remove the empty `%if`/`%endif` block",
                Vec::new(),
                Applicability::MachineApplicable,
            )),
        );
    }
}

impl<'ast> Visit<'ast> for EmptyConditionalBranch {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        let all_empty = node.branches.iter().all(|b| is_empty_top_body(&b.body))
            && node.otherwise.as_ref().is_none_or(|o| is_empty_top_body(o));
        if all_empty {
            self.emit(node.data);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        let all_empty = node
            .branches
            .iter()
            .all(|b| is_empty_preamble_body(&b.body))
            && node
                .otherwise
                .as_ref()
                .is_none_or(|o| is_empty_preamble_body(o));
        if all_empty {
            self.emit(node.data);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        let all_empty = node.branches.iter().all(|b| is_empty_files_body(&b.body))
            && node
                .otherwise
                .as_ref()
                .is_none_or(|o| is_empty_files_body(o));
        if all_empty {
            self.emit(node.data);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for EmptyConditionalBranch {
    fn metadata(&self) -> &'static LintMetadata {
        &EMPTY_BRANCH_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM077 ifarch-empty-list
// =====================================================================

pub static IFARCH_EMPTY_METADATA: LintMetadata = LintMetadata {
    id: "RPM077",
    name: "ifarch-empty-list",
    description: "`%ifarch`/`%ifos` with no architecture tokens is always false; likely a missing argument.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct IfarchEmptyList {
    diagnostics: Vec<Diagnostic>,
}

impl IfarchEmptyList {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_branch<T, B>(&mut self, branch: &CondBranch<Span, B>) {
        let is_arch_kind = matches!(
            branch.kind,
            CondKind::IfArch
                | CondKind::IfNArch
                | CondKind::IfOs
                | CondKind::IfNOs
                | CondKind::ElifArch
                | CondKind::ElifOs
        );
        if !is_arch_kind {
            return;
        }
        if let CondExpr::ArchList(items) = &branch.expr
            && items.is_empty()
        {
            self.diagnostics.push(Diagnostic::new(
                &IFARCH_EMPTY_METADATA,
                Severity::Warn,
                "`%ifarch`/`%ifos` with no tokens is always false; supply at least one \
                 architecture/OS name",
                branch.data,
            ));
        }
        let _ = std::marker::PhantomData::<T>;
    }
}

impl<'ast> Visit<'ast> for IfarchEmptyList {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        for b in &node.branches {
            self.check_branch::<Span, _>(b);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        for b in &node.branches {
            self.check_branch::<Span, _>(b);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        for b in &node.branches {
            self.check_branch::<Span, _>(b);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for IfarchEmptyList {
    fn metadata(&self) -> &'static LintMetadata {
        &IFARCH_EMPTY_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM089 single-comment-only-branch (Phase 7b)
// =====================================================================

pub static SINGLE_COMMENT_METADATA: LintMetadata = LintMetadata {
    id: "RPM089",
    name: "single-comment-only-branch",
    description: "Conditional branch contains only a comment — likely a TODO left after a refactor.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct SingleCommentOnlyBranch {
    diagnostics: Vec<Diagnostic>,
}

impl SingleCommentOnlyBranch {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(Diagnostic::new(
            &SINGLE_COMMENT_METADATA,
            Severity::Warn,
            "conditional branch contains only a comment — possibly a forgotten TODO",
            anchor,
        ));
    }
}

fn is_only_comment_top(body: &[SpecItem<Span>]) -> bool {
    let real: Vec<_> = body
        .iter()
        .filter(|i| !matches!(i, SpecItem::Blank))
        .collect();
    real.len() == 1 && matches!(real[0], SpecItem::Comment(_))
}
fn is_only_comment_preamble(body: &[PreambleContent<Span>]) -> bool {
    let real: Vec<_> = body
        .iter()
        .filter(|i| !matches!(i, PreambleContent::Blank))
        .collect();
    real.len() == 1 && matches!(real[0], PreambleContent::Comment(_))
}
fn is_only_comment_files(body: &[FilesContent<Span>]) -> bool {
    let real: Vec<_> = body
        .iter()
        .filter(|i| !matches!(i, FilesContent::Blank))
        .collect();
    real.len() == 1 && matches!(real[0], FilesContent::Comment(_))
}

impl<'ast> Visit<'ast> for SingleCommentOnlyBranch {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        for b in &node.branches {
            if is_only_comment_top(&b.body) {
                self.emit(b.data);
            }
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        for b in &node.branches {
            if is_only_comment_preamble(&b.body) {
                self.emit(b.data);
            }
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        for b in &node.branches {
            if is_only_comment_files(&b.body) {
                self.emit(b.data);
            }
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for SingleCommentOnlyBranch {
    fn metadata(&self) -> &'static LintMetadata {
        &SINGLE_COMMENT_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM090 ifarch-noarch (Phase 7b)
// =====================================================================

pub static IFARCH_NOARCH_METADATA: LintMetadata = LintMetadata {
    id: "RPM090",
    name: "ifarch-noarch",
    description: "`%ifarch noarch` is suspicious — `noarch` is a build marker, not an architecture.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct IfarchNoarch {
    diagnostics: Vec<Diagnostic>,
}

impl IfarchNoarch {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_branch<B>(&mut self, branch: &CondBranch<Span, B>) {
        let is_arch = matches!(
            branch.kind,
            CondKind::IfArch | CondKind::IfNArch | CondKind::ElifArch
        );
        if !is_arch {
            return;
        }
        if let CondExpr::ArchList(items) = &branch.expr
            && items.iter().any(|t| t.literal_str() == Some("noarch"))
        {
            self.diagnostics.push(Diagnostic::new(
                &IFARCH_NOARCH_METADATA,
                Severity::Warn,
                "`noarch` in an `%ifarch` token list is suspicious — \
                 `noarch` is a build marker, not an architecture",
                branch.data,
            ));
        }
    }
}

impl<'ast> Visit<'ast> for IfarchNoarch {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        for b in &node.branches {
            self.check_branch(b);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        for b in &node.branches {
            self.check_branch(b);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        for b in &node.branches {
            self.check_branch(b);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for IfarchNoarch {
    fn metadata(&self) -> &'static LintMetadata {
        &IFARCH_NOARCH_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM091 duplicate-arch-in-list (Phase 7b)
// =====================================================================

pub static DUPLICATE_ARCH_METADATA: LintMetadata = LintMetadata {
    id: "RPM091",
    name: "duplicate-arch-in-list",
    description: "Duplicate token in `%ifarch`/`%ifos` list — drop the redundant one.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct DuplicateArchInList {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl DuplicateArchInList {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_branch<B>(&mut self, branch: &CondBranch<Span, B>) {
        let CondExpr::ArchList(items) = &branch.expr else {
            return;
        };
        let mut seen: Vec<&str> = Vec::with_capacity(items.len());
        for t in items {
            let Some(s) = t.literal_str() else { continue };
            if seen.contains(&s) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &DUPLICATE_ARCH_METADATA,
                        Severity::Warn,
                        format!("duplicate `{s}` in arch/os token list"),
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        format!("drop the redundant `{s}`"),
                        self.dedupe_edit(branch.data, s).into_iter().collect(),
                        if self.source.is_some() {
                            Applicability::MachineApplicable
                        } else {
                            Applicability::Manual
                        },
                    )),
                );
            } else {
                seen.push(s);
            }
        }
    }

    /// Construct an [`Edit`] removing the *second* occurrence (with
    /// its preceding whitespace) of `token` from the source slice
    /// covered by the branch span. Returns `None` when source isn't
    /// available or the duplicate can't be located.
    fn dedupe_edit(&self, anchor: Span, token: &str) -> Option<Edit> {
        let source = self.source.as_deref()?;
        let end = anchor.end_byte.min(source.len());
        let start = anchor.start_byte.min(end);
        let slice = source.get(start..end)?;
        // Find first occurrence, skip it, find second.
        let first = find_word_index(slice, token)?;
        let after_first = first + token.len();
        let rel_second = find_word_index(&slice[after_first..], token)?;
        let second = after_first + rel_second;
        // Extend left to also drop a single preceding whitespace.
        let cut_start = slice[..second]
            .char_indices()
            .next_back()
            .filter(|(_, c)| c.is_ascii_whitespace())
            .map(|(i, _)| i)
            .unwrap_or(second);
        Some(Edit::new(
            Span::from_bytes(start + cut_start, start + second + token.len()),
            "",
        ))
    }
}

/// Find `needle` as a whole word (bounded by non-alphanumeric chars)
/// inside `haystack`. Returns the byte offset of the match.
fn find_word_index(haystack: &str, needle: &str) -> Option<usize> {
    let mut idx = 0;
    while let Some(rel) = haystack[idx..].find(needle) {
        let pos = idx + rel;
        let end = pos + needle.len();
        let prev_ok = pos == 0
            || !haystack.as_bytes()[pos - 1].is_ascii_alphanumeric()
                && haystack.as_bytes()[pos - 1] != b'_';
        let next_ok = end >= haystack.len()
            || !haystack.as_bytes()[end].is_ascii_alphanumeric()
                && haystack.as_bytes()[end] != b'_';
        if prev_ok && next_ok {
            return Some(pos);
        }
        idx = pos + needle.len();
    }
    None
}

impl<'ast> Visit<'ast> for DuplicateArchInList {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        for b in &node.branches {
            self.check_branch(b);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        for b in &node.branches {
            self.check_branch(b);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        for b in &node.branches {
            self.check_branch(b);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for DuplicateArchInList {
    fn metadata(&self) -> &'static LintMetadata {
        &DUPLICATE_ARCH_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }
}

// =====================================================================
// RPM092 conditional-cyclomatic-complexity (Phase 7b)
// =====================================================================

/// Default cyclomatic threshold for one `%build`/`%install`/etc.
/// section. Hardcoded; future config knob.
const MAX_BRANCH_SUM: usize = 15;

pub static CYCLOMATIC_METADATA: LintMetadata = LintMetadata {
    id: "RPM092",
    name: "conditional-cyclomatic-complexity",
    description: "Section contains more conditional branches than is comfortable to follow; refactor.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct ConditionalCyclomaticComplexity {
    diagnostics: Vec<Diagnostic>,
}

impl ConditionalCyclomaticComplexity {
    pub fn new() -> Self {
        Self::default()
    }
}

fn count_branches_in_items(items: &[SpecItem<Span>]) -> usize {
    let mut sum = 0;
    for item in items {
        if let SpecItem::Conditional(c) = item {
            sum += c.branches.len() + usize::from(c.otherwise.is_some());
            for b in &c.branches {
                sum += count_branches_in_items(&b.body);
            }
            if let Some(o) = &c.otherwise {
                sum += count_branches_in_items(o);
            }
        }
    }
    sum
}

impl<'ast> Visit<'ast> for ConditionalCyclomaticComplexity {
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        // For now we walk only `BuildScript` bodies — `%prep`/`%build`/
        // `%install` etc. The body is a `ShellBody` (lines), but
        // conditionals inside aren't part of the AST today, so we
        // can't actually count. Instead, count top-level Conditional
        // SpecItems inside the section. (No-op until upstream parses
        // shell-body conditionals structurally.)
        let _ = node;
        visit::walk_section(self, node);
    }
    fn visit_spec(&mut self, spec: &'ast rpm_spec::ast::SpecFile<Span>) {
        // Section coverage isn't enough — count top-level too as a
        // proxy for spec-wide complexity.
        let sum = count_branches_in_items(&spec.items);
        if sum > MAX_BRANCH_SUM {
            self.diagnostics.push(Diagnostic::new(
                &CYCLOMATIC_METADATA,
                Severity::Warn,
                format!("spec has {sum} conditional branches in total — refactor or split"),
                spec.data,
            ));
        }
        visit::walk_spec(self, spec);
    }
}

impl Lint for ConditionalCyclomaticComplexity {
    fn metadata(&self) -> &'static LintMetadata {
        &CYCLOMATIC_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ---- RPM070 deep-conditional-nesting ----

    #[test]
    fn rpm070_flags_depth_5() {
        let src = "Name: x\n%if 1\n%if 1\n%if 1\n%if 1\n%if 1\nVersion: 1\n\
                   %endif\n%endif\n%endif\n%endif\n%endif\n";
        let diags = run(src, DeepConditionalNesting::new());
        assert!(!diags.is_empty(), "expected RPM070 at depth 5: {diags:?}");
        assert_eq!(diags[0].lint_id, "RPM070");
    }

    #[test]
    fn rpm070_silent_at_depth_4() {
        let src = "Name: x\n%if 1\n%if 1\n%if 1\n%if 1\nVersion: 1\n\
                   %endif\n%endif\n%endif\n%endif\n";
        assert!(run(src, DeepConditionalNesting::new()).is_empty());
    }

    #[test]
    fn rpm070_silent_for_flat_chain() {
        // Many siblings, no nesting — should not trigger.
        let src = "Name: x\n%if 0\n%endif\n%if 0\n%endif\n%if 0\n%endif\n";
        assert!(run(src, DeepConditionalNesting::new()).is_empty());
    }

    // ---- RPM071 unreachable-elif-branch ----

    #[test]
    fn rpm071_flags_repeated_elif() {
        let src = "Name: x\n%if 0\nVersion: 1\n%elif 0\nVersion: 2\n%endif\n";
        let diags = run(src, UnreachableElifBranch::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM071");
    }

    #[test]
    fn rpm071_silent_for_distinct_elif() {
        let src = "Name: x\n%if 0\nVersion: 1\n%elif 1\nVersion: 2\n%endif\n";
        assert!(run(src, UnreachableElifBranch::new()).is_empty());
    }

    #[test]
    fn rpm071_silent_for_macro_in_condition() {
        // Both conditions reference macros — we can't statically tell
        // they're equal, so we bail out (conservative).
        let src = "Name: x\n%if 0%{?rhel}\nVersion: 1\n%elif 0%{?rhel}\nVersion: 2\n%endif\n";
        assert!(run(src, UnreachableElifBranch::new()).is_empty());
    }

    // ---- RPM073 empty-conditional-branch ----

    #[test]
    fn rpm073_silent_for_zero_item_block() {
        // Zero-item body means the parser dropped its contents (e.g.
        // an unknown project macro). Don't fire RPM073 — that would
        // be a false positive on the bulk of real specs that use
        // private macros like `%gendep_perl_libs` inside `%if`.
        // Genuine empty-with-blank-line cases are covered by
        // `rpm073_flags_empty_branches_with_blanks`.
        let src = "Name: x\n%if 0\n%endif\n";
        let diags = run(src, EmptyConditionalBranch::new());
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn rpm073_flags_empty_branches_with_blanks() {
        // Blank lines inside count as filler — still empty.
        let src = "Name: x\n%if 0\n\n%else\n\n%endif\n";
        let diags = run(src, EmptyConditionalBranch::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm073_silent_when_branch_has_content() {
        let src = "Name: x\n%if 0\nVersion: 1\n%endif\n";
        assert!(run(src, EmptyConditionalBranch::new()).is_empty());
    }

    // ---- RPM077 ifarch-empty-list ----

    #[test]
    fn rpm077_flags_empty_ifarch() {
        let src = "Name: x\n%ifarch\nVersion: 1\n%endif\n";
        let diags = run(src, IfarchEmptyList::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM077");
    }

    #[test]
    fn rpm077_silent_for_non_empty_ifarch() {
        let src = "Name: x\n%ifarch x86_64\nVersion: 1\n%endif\n";
        assert!(run(src, IfarchEmptyList::new()).is_empty());
    }

    #[test]
    fn rpm077_silent_for_plain_if() {
        // Plain `%if` is not an arch/os branch; should not be touched.
        let src = "Name: x\n%if 0\nVersion: 1\n%endif\n";
        assert!(run(src, IfarchEmptyList::new()).is_empty());
    }

    // ---- RPM089 single-comment-only-branch ----

    #[test]
    fn rpm089_flags_comment_only_branch() {
        let src = "Name: x\n%if 1\n# TODO: fill in\n%endif\n";
        let diags = run(src, SingleCommentOnlyBranch::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM089");
    }

    #[test]
    fn rpm089_silent_for_real_content() {
        let src = "Name: x\n%if 1\n# note\nVersion: 1\n%endif\n";
        assert!(run(src, SingleCommentOnlyBranch::new()).is_empty());
    }

    // ---- RPM090 ifarch-noarch ----

    #[test]
    fn rpm090_flags_ifarch_noarch() {
        let src = "Name: x\n%ifarch noarch\nVersion: 1\n%endif\n";
        let diags = run(src, IfarchNoarch::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM090");
    }

    #[test]
    fn rpm090_silent_for_real_arch() {
        let src = "Name: x\n%ifarch x86_64\nVersion: 1\n%endif\n";
        assert!(run(src, IfarchNoarch::new()).is_empty());
    }

    // ---- RPM091 duplicate-arch-in-list ----

    #[test]
    fn rpm091_flags_duplicate_arch() {
        let src = "Name: x\n%ifarch x86_64 x86_64\nVersion: 1\n%endif\n";
        let mut lint = DuplicateArchInList::new();
        lint.set_source(src);
        let outcome = parse(src);
        lint.visit_spec(&outcome.spec);
        let diags = lint.take_diagnostics();
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM091");
        // MachineApplicable: an edit must be present.
        assert!(!diags[0].suggestions[0].edits.is_empty());
    }

    #[test]
    fn rpm091_silent_for_distinct_arches() {
        let src = "Name: x\n%ifarch x86_64 aarch64\nVersion: 1\n%endif\n";
        assert!(run(src, DuplicateArchInList::new()).is_empty());
    }

    // ---- RPM092 conditional-cyclomatic-complexity ----

    #[test]
    fn rpm092_flags_many_branches() {
        // 16 `%if` blocks at top level → branch count >= 16 > 15.
        let mut src = String::from("Name: x\n");
        for _ in 0..16 {
            src.push_str("%if 0\nLicense: MIT\n%endif\n");
        }
        let diags = run(&src, ConditionalCyclomaticComplexity::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM092");
    }

    #[test]
    fn rpm092_silent_below_threshold() {
        let mut src = String::from("Name: x\n");
        for _ in 0..5 {
            src.push_str("%if 0\nLicense: MIT\n%endif\n");
        }
        assert!(run(&src, ConditionalCyclomaticComplexity::new()).is_empty());
    }
}
