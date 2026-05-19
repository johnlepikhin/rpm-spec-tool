//! Idiomatic-RPM lints (Phase 7b).
//!
//! - RPM095 `prefer-bcond-for-build-options` — `%if 0%{?with_python}`
//!   pattern → use `%bcond_with python` + `%{with python}`.
//! - RPM096 `if-only-buildrequires` — `%if X BuildRequires: foo %endif`
//!   stylistically chunky; consider `%bcond_with` or conditional
//!   `Requires:` modifier.
//!
//! Both rules emit `Manual` advice: a precise auto-fix needs adding a
//! `%bcond` declaration to the preamble and reworking the condition,
//! which crosses block boundaries.

use rpm_spec::ast::{
    CondExpr, Conditional, ExprAst, FilesContent, PreambleContent, PreambleItem, Span, SpecItem,
    Tag,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

// =====================================================================
// RPM095 prefer-bcond-for-build-options
// =====================================================================

pub static PREFER_BCOND_METADATA: LintMetadata = LintMetadata {
    id: "RPM095",
    name: "prefer-bcond-for-build-options",
    description: "`%if 0%{?with_NAME}` pattern is the build-option idiom; use `%bcond_with NAME` instead.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// `%if 0%{?with_NAME}` pattern is the build-option idiom; use `%bcond_with NAME` instead.
///
/// See [`PREFER_BCOND_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct PreferBcondForBuildOptions {
    diagnostics: Vec<Diagnostic>,
}

impl PreferBcondForBuildOptions {
    pub fn new() -> Self {
        Self::default()
    }

    fn check_expr(&mut self, branch_data: Span, expr: &CondExpr<Span>) {
        let text = match expr {
            CondExpr::Raw(t) => t
                .literal_str()
                .map(str::trim)
                .unwrap_or_default()
                .to_string(),
            // `0%{?with_NAME}` is now parsed as a structural
            // `NumericConcat` (since the parser learnt to fuse a
            // literal digit with a following macro). The lint must
            // recognise this shape too — recover the source-form
            // string from the concat parts and rerun the textual
            // matcher so the diagnostic stays identical.
            CondExpr::Parsed(boxed) => match boxed.as_ref() {
                ExprAst::NumericConcat { parts, .. } => parts
                    .iter()
                    .map(crate::branch_coverage::concat_part_text)
                    .collect(),
                _ => return, // Other Parsed shapes don't match the build-option idiom.
            },
            _ => return,
        };
        if let Some(name) = parse_with_pattern(&text) {
            self.diagnostics.push(
                Diagnostic::new(
                    &PREFER_BCOND_METADATA,
                    Severity::Warn,
                    format!(
                        "`0%{{?with_{name}}}` is the build-option idiom — declare \
                         `%bcond_with {name}` in the preamble and use `%{{with {name}}}` here"
                    ),
                    branch_data,
                )
                .with_suggestion(Suggestion::new(
                    "rewrite as `%bcond_with NAME` + `%{with NAME}`",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
        let _ = branch_data;
    }
}

/// `Some("python")` for `0%{?with_python}`. Returns `None` if `text`
/// doesn't match the pattern.
fn parse_with_pattern(text: &str) -> Option<&str> {
    // Accept `0%{?with_NAME}` (without trailing operators / and-or).
    // Strict on the leading `0%{?with_` and trailing `}` — anything
    // else means the condition is doing something else with the flag.
    let inner = text.strip_prefix("0%{?with_")?.strip_suffix('}')?;
    // The remaining `inner` must be a single identifier (no
    // operators, no nested macros).
    if inner.is_empty() {
        return None;
    }
    if !inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    Some(inner)
}

impl<'ast> Visit<'ast> for PreferBcondForBuildOptions {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        for b in &node.branches {
            self.check_expr(b.data, &b.expr);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        for b in &node.branches {
            self.check_expr(b.data, &b.expr);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        for b in &node.branches {
            self.check_expr(b.data, &b.expr);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for PreferBcondForBuildOptions {
    fn metadata(&self) -> &'static LintMetadata {
        &PREFER_BCOND_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM096 if-only-buildrequires
// =====================================================================

pub static IF_ONLY_BR_METADATA: LintMetadata = LintMetadata {
    id: "RPM096",
    name: "if-only-buildrequires",
    description: "`%if X BuildRequires: foo %endif` is stylistically heavy; consider `%bcond_with` or \
         a conditional dependency clause.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// `%if X BuildRequires: foo %endif` is stylistically heavy; consider `%bcond_with` or a conditional dependency clause.
///
/// See [`IF_ONLY_BR_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct IfOnlyBuildRequires {
    diagnostics: Vec<Diagnostic>,
}

impl IfOnlyBuildRequires {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &IF_ONLY_BR_METADATA,
                Severity::Warn,
                "conditional block contains only `BuildRequires:` items — use `%bcond_with` \
                 or a conditional `Requires:` clause instead",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "rewrite using `%bcond_with NAME` or the rich-dependency `(foo if bar)` form",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

fn is_build_requires_item_top(item: &SpecItem<Span>) -> bool {
    matches!(
        item,
        SpecItem::Preamble(PreambleItem {
            tag: Tag::BuildRequires,
            ..
        })
    )
}

fn is_build_requires_item_preamble(item: &PreambleContent<Span>) -> bool {
    matches!(
        item,
        PreambleContent::Item(PreambleItem {
            tag: Tag::BuildRequires,
            ..
        })
    )
}

impl<'ast> Visit<'ast> for IfOnlyBuildRequires {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        let only_br = node.branches.iter().all(|b| {
            !b.body.is_empty()
                && b.body.iter().all(|i| {
                    matches!(i, SpecItem::Blank | SpecItem::Comment(_))
                        || is_build_requires_item_top(i)
                })
                && b.body.iter().any(is_build_requires_item_top)
        });
        if only_br {
            self.emit(node.data);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        let only_br = node.branches.iter().all(|b| {
            !b.body.is_empty()
                && b.body.iter().all(|i| {
                    matches!(i, PreambleContent::Blank | PreambleContent::Comment(_))
                        || is_build_requires_item_preamble(i)
                })
                && b.body.iter().any(is_build_requires_item_preamble)
        });
        if only_br {
            self.emit(node.data);
        }
        visit::walk_preamble_conditional(self, node);
    }
}

impl Lint for IfOnlyBuildRequires {
    fn metadata(&self) -> &'static LintMetadata {
        &IF_ONLY_BR_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM106 conditional-buildarch (Phase 7d)
// =====================================================================

pub static CONDITIONAL_BUILDARCH_METADATA: LintMetadata = LintMetadata {
    id: "RPM106",
    name: "conditional-buildarch",
    description: "`BuildArch:` inside a `%if` block — RPM uses last-wins semantics, so this is fragile.",
    default_severity: Severity::Allow,
    category: LintCategory::Correctness,
};

/// `BuildArch:` inside a `%if` block — RPM uses last-wins semantics, so this is fragile.
///
/// See [`CONDITIONAL_BUILDARCH_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ConditionalBuildArch {
    diagnostics: Vec<Diagnostic>,
}

impl ConditionalBuildArch {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(Diagnostic::new(
            &CONDITIONAL_BUILDARCH_METADATA,
            Severity::Warn,
            "`BuildArch:` inside a `%if` block — RPM uses the last definition, so the \
             effective arch depends on file order; consider moving the tag outside the \
             conditional or using `%ifarch` instead",
            anchor,
        ));
    }
}

fn is_buildarch_top(item: &SpecItem<Span>) -> Option<Span> {
    if let SpecItem::Preamble(PreambleItem {
        tag: Tag::BuildArch,
        data,
        ..
    }) = item
    {
        Some(*data)
    } else {
        None
    }
}

fn is_buildarch_preamble(item: &PreambleContent<Span>) -> Option<Span> {
    if let PreambleContent::Item(PreambleItem {
        tag: Tag::BuildArch,
        data,
        ..
    }) = item
    {
        Some(*data)
    } else {
        None
    }
}

impl<'ast> Visit<'ast> for ConditionalBuildArch {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        for b in &node.branches {
            for item in &b.body {
                if let Some(span) = is_buildarch_top(item) {
                    self.emit(span);
                }
            }
        }
        if let Some(other) = &node.otherwise {
            for item in other {
                if let Some(span) = is_buildarch_top(item) {
                    self.emit(span);
                }
            }
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        for b in &node.branches {
            for item in &b.body {
                if let Some(span) = is_buildarch_preamble(item) {
                    self.emit(span);
                }
            }
        }
        if let Some(other) = &node.otherwise {
            for item in other {
                if let Some(span) = is_buildarch_preamble(item) {
                    self.emit(span);
                }
            }
        }
        visit::walk_preamble_conditional(self, node);
    }
}

impl Lint for ConditionalBuildArch {
    fn metadata(&self) -> &'static LintMetadata {
        &CONDITIONAL_BUILDARCH_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM107 conditional-name-tag (Phase 7d)
// =====================================================================

pub static CONDITIONAL_NAME_METADATA: LintMetadata = LintMetadata {
    id: "RPM107",
    name: "conditional-name-tag",
    description: "`Name:` inside a `%if` block — the package will have different names in different \
         build contexts, which confuses downstream tooling.",
    default_severity: Severity::Allow,
    category: LintCategory::Correctness,
};

/// `Name:` inside a `%if` block — the package will have different names in different build contexts, which confuses downstream tooling.
///
/// See [`CONDITIONAL_NAME_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ConditionalNameTag {
    diagnostics: Vec<Diagnostic>,
}

impl ConditionalNameTag {
    pub fn new() -> Self {
        Self::default()
    }

    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(Diagnostic::new(
            &CONDITIONAL_NAME_METADATA,
            Severity::Warn,
            "`Name:` inside a `%if` block — the package name then depends on build flags; \
             consider a fixed name and `%bcond` instead",
            anchor,
        ));
    }
}

fn is_name_top(item: &SpecItem<Span>) -> Option<Span> {
    if let SpecItem::Preamble(PreambleItem {
        tag: Tag::Name,
        data,
        ..
    }) = item
    {
        Some(*data)
    } else {
        None
    }
}

fn is_name_preamble(item: &PreambleContent<Span>) -> Option<Span> {
    if let PreambleContent::Item(PreambleItem {
        tag: Tag::Name,
        data,
        ..
    }) = item
    {
        Some(*data)
    } else {
        None
    }
}

impl<'ast> Visit<'ast> for ConditionalNameTag {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        for b in &node.branches {
            for item in &b.body {
                if let Some(span) = is_name_top(item) {
                    self.emit(span);
                }
            }
        }
        if let Some(other) = &node.otherwise {
            for item in other {
                if let Some(span) = is_name_top(item) {
                    self.emit(span);
                }
            }
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        for b in &node.branches {
            for item in &b.body {
                if let Some(span) = is_name_preamble(item) {
                    self.emit(span);
                }
            }
        }
        if let Some(other) = &node.otherwise {
            for item in other {
                if let Some(span) = is_name_preamble(item) {
                    self.emit(span);
                }
            }
        }
        visit::walk_preamble_conditional(self, node);
    }
}

impl Lint for ConditionalNameTag {
    fn metadata(&self) -> &'static LintMetadata {
        &CONDITIONAL_NAME_METADATA
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

    // ---- RPM095 ----

    #[test]
    fn rpm095_flags_with_python() {
        let src = "Name: x\n%if 0%{?with_python}\nBuildRequires: python3-devel\n%endif\n";
        let diags = run(src, PreferBcondForBuildOptions::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM095");
        assert!(diags[0].message.contains("with_python"));
    }

    #[test]
    fn rpm095_silent_for_other_macros() {
        let src = "Name: x\n%if 0%{?rhel}\nBuildRequires: python3-devel\n%endif\n";
        assert!(run(src, PreferBcondForBuildOptions::new()).is_empty());
    }

    // ---- RPM096 ----

    #[test]
    fn rpm096_flags_br_only_block() {
        let src = "Name: x\n%if 1\nBuildRequires: foo\nBuildRequires: bar\n%endif\n";
        let diags = run(src, IfOnlyBuildRequires::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM096");
    }

    #[test]
    fn rpm096_silent_when_mixed_content() {
        let src = "Name: x\n%if 1\nBuildRequires: foo\nVersion: 2\n%endif\n";
        assert!(run(src, IfOnlyBuildRequires::new()).is_empty());
    }

    // ---- RPM106 conditional-buildarch ----

    #[test]
    fn rpm106_flags_buildarch_inside_if() {
        let src = "Name: x\n%if 1\nBuildArch: noarch\n%endif\n";
        let diags = run(src, ConditionalBuildArch::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM106");
    }

    #[test]
    fn rpm106_silent_when_buildarch_outside() {
        let src = "Name: x\nBuildArch: noarch\n%if 1\nVersion: 2\n%endif\n";
        assert!(run(src, ConditionalBuildArch::new()).is_empty());
    }

    // ---- RPM107 conditional-name-tag ----

    #[test]
    fn rpm107_flags_name_inside_if() {
        // The outer `Name: x` is preamble; inside the `%if` we have
        // a second `Name:` definition — the rule fires on it.
        let src = "Name: x\n%if 1\nName: y\n%endif\n";
        let diags = run(src, ConditionalNameTag::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM107");
    }

    #[test]
    fn rpm107_silent_when_name_outside() {
        let src = "Name: x\n%if 1\nLicense: MIT\n%endif\n";
        assert!(run(src, ConditionalNameTag::new()).is_empty());
    }
}
