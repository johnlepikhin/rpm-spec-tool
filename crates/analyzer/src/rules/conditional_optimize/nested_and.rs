//! RPM080 `nested-and-collapse` — `%if A %if B FOO %endif %endif` →
//! `%if (A) && (B) FOO %endif`.

use rpm_spec::ast::{
    CondExpr, CondKind, Conditional, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static NESTED_AND_METADATA: LintMetadata = LintMetadata {
    id: "RPM080",
    name: "nested-and-collapse",
    description: "Two single-branch `%if` blocks nested directly can be merged into one with `&&`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct NestedAndCollapse {
    diagnostics: Vec<Diagnostic>,
}

impl NestedAndCollapse {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Returns the inner `Conditional` if `body` is a single-branch
/// (`%if`, not `%ifarch`/`%ifos`) block with no `%else`, exactly one
/// non-filler item that is itself a single-branch plain `%if` with no
/// `%else`. Filler = `Blank` / `Comment`.
fn nested_and_pattern_top(
    outer: &Conditional<Span, SpecItem<Span>>,
) -> Option<&Conditional<Span, SpecItem<Span>>> {
    if outer.branches.len() != 1 || outer.otherwise.is_some() {
        return None;
    }
    let outer_branch = &outer.branches[0];
    if !is_plain_if(outer_branch.kind) {
        return None;
    }
    let mut non_filler = outer_branch
        .body
        .iter()
        .filter(|i| !matches!(i, SpecItem::Blank | SpecItem::Comment(_)));
    let SpecItem::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if inner.branches.len() != 1 || inner.otherwise.is_some() {
        return None;
    }
    if !is_plain_if(inner.branches[0].kind) {
        return None;
    }
    if !inner_branch_safe(inner) {
        return None;
    }
    Some(inner)
}

fn nested_and_pattern_preamble(
    outer: &Conditional<Span, PreambleContent<Span>>,
) -> Option<&Conditional<Span, PreambleContent<Span>>> {
    if outer.branches.len() != 1 || outer.otherwise.is_some() {
        return None;
    }
    if !is_plain_if(outer.branches[0].kind) {
        return None;
    }
    let mut non_filler = outer.branches[0]
        .body
        .iter()
        .filter(|i| !matches!(i, PreambleContent::Blank | PreambleContent::Comment(_)));
    let PreambleContent::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if inner.branches.len() != 1 || inner.otherwise.is_some() {
        return None;
    }
    if !is_plain_if(inner.branches[0].kind) {
        return None;
    }
    if !inner_branch_safe(inner) {
        return None;
    }
    Some(inner)
}

fn nested_and_pattern_files(
    outer: &Conditional<Span, FilesContent<Span>>,
) -> Option<&Conditional<Span, FilesContent<Span>>> {
    if outer.branches.len() != 1 || outer.otherwise.is_some() {
        return None;
    }
    if !is_plain_if(outer.branches[0].kind) {
        return None;
    }
    let mut non_filler = outer.branches[0]
        .body
        .iter()
        .filter(|i| !matches!(i, FilesContent::Blank | FilesContent::Comment(_)));
    let FilesContent::Conditional(inner) = non_filler.next()? else {
        return None;
    };
    if non_filler.next().is_some() {
        return None;
    }
    if inner.branches.len() != 1 || inner.otherwise.is_some() {
        return None;
    }
    if !is_plain_if(inner.branches[0].kind) {
        return None;
    }
    if !inner_branch_safe(inner) {
        return None;
    }
    Some(inner)
}

/// Plain `%if` (not arch/os-specialised). Arch/os conditions don't
/// compose with `&&` in RPM's expression grammar, so we don't suggest
/// merging them.
fn is_plain_if(kind: CondKind) -> bool {
    matches!(kind, CondKind::If)
}

/// `true` when the *inner* `%if`'s expression is safe to evaluate
/// unconditionally — i.e. merging it with the outer guard via `&&`
/// won't change semantics on systems where the outer evaluates to
/// false.
///
/// RPM evaluates `&&` eagerly (no short-circuit), so an unconditional
/// `%{name}` reference in the inner condition becomes a parse error
/// the moment the outer is false but the macro is undefined. Allowed
/// forms:
/// - `%{?name}` / `%{!?name}` / `%{?name:default}` — conditional refs,
///   expand to empty when undefined;
/// - `%%` — escaped literal `%`;
/// - no `%`-form at all.
///
/// Anything else (bare `%name` or unconditional `%{name}`) is risky
/// — until the profile system can vouch for the macro's availability,
/// we skip the suggestion.
fn inner_expr_safe_to_merge(expr: &str) -> bool {
    let bytes = expr.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        match bytes[i + 1] {
            // `%%` — escaped percent; harmless.
            b'%' => i += 2,
            // `%{...}` — peek past whitespace to the first
            // significant byte. Conditional sigils (`?`, `!`) are
            // safe; anything else means an unconditional reference.
            b'{' => {
                let mut j = i + 2;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                match bytes.get(j) {
                    Some(b'?' | b'!') => i = j + 1,
                    Some(_) => return false,
                    None => return false, // malformed; bail
                }
            }
            // `%name` (or `%(...)`, `%[...]`) — bare macro / shell /
            // expression form. Unconditional.
            _ => return false,
        }
    }
    true
}

/// `true` if the inner `%if`'s expression passes the safety check.
/// Wraps the literal extraction so the three context-specific
/// pattern detectors share one entry point.
fn inner_branch_safe<B>(inner: &Conditional<Span, B>) -> bool {
    let Some(first) = inner.branches.first() else {
        return false;
    };
    match &first.expr {
        CondExpr::Raw(text) => {
            let Some(lit) = text.literal_str() else {
                return false;
            };
            inner_expr_safe_to_merge(lit)
        }
        CondExpr::Parsed(ast) => expr_ast_safe_to_merge(ast),
        _ => false,
    }
}

/// Walk a parsed expression and verify every `%`-bearing leaf uses a
/// conditional macro reference (`%{?foo}`/`%{!?foo}`) or is otherwise
/// macro-free. Mirrors [`inner_expr_safe_to_merge`] but operates on
/// the AST directly.
///
/// String literals get scanned for embedded macros too — RPM expands
/// `"%{foo}"` at evaluation time, so an unconditional reference inside
/// quotes is just as dangerous as outside.
fn expr_ast_safe_to_merge<T>(ast: &rpm_spec::ast::ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    match ast {
        ExprAst::Integer { .. } | ExprAst::Identifier { .. } => true,
        ExprAst::String { value, .. } => inner_expr_safe_to_merge(value),
        ExprAst::Macro { text, .. } => inner_expr_safe_to_merge(text),
        ExprAst::Paren { inner, .. } | ExprAst::Not { inner, .. } => expr_ast_safe_to_merge(inner),
        ExprAst::Binary { lhs, rhs, .. } => {
            expr_ast_safe_to_merge(lhs) && expr_ast_safe_to_merge(rhs)
        }
        _ => false,
    }
}

impl NestedAndCollapse {
    fn emit(&mut self, anchor: Span) {
        self.diagnostics.push(
            Diagnostic::new(
                &NESTED_AND_METADATA,
                Severity::Warn,
                "nested `%if`s can be safely merged into one with `&&` \
                 (inner condition uses only conditional macro refs)",
                anchor,
            )
            .with_suggestion(Suggestion::new(
                "rewrite as `%if (outer-expr) && (inner-expr)` and drop one `%if`/`%endif` pair",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

impl<'ast> Visit<'ast> for NestedAndCollapse {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        if nested_and_pattern_top(node).is_some() {
            self.emit(node.data);
        }
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        if nested_and_pattern_preamble(node).is_some() {
            self.emit(node.data);
        }
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        if nested_and_pattern_files(node).is_some() {
            self.emit(node.data);
        }
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for NestedAndCollapse {
    fn metadata(&self) -> &'static LintMetadata {
        &NESTED_AND_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Diagnostic;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn rpm080_flags_nested_and() {
        let src = "Name: x\n%if 1\n%if 1\nVersion: 1\n%endif\n%endif\n";
        let diags = run(src, NestedAndCollapse::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM080");
    }

    #[test]
    fn rpm080_silent_when_outer_has_else() {
        let src = "Name: x\n%if 1\n%if 1\nVersion: 1\n%endif\n%else\nVersion: 2\n%endif\n";
        assert!(run(src, NestedAndCollapse::new()).is_empty());
    }

    #[test]
    fn rpm080_silent_when_inner_has_else() {
        let src = "Name: x\n%if 1\n%if 1\nVersion: 1\n%else\nVersion: 2\n%endif\n%endif\n";
        assert!(run(src, NestedAndCollapse::new()).is_empty());
    }

    #[test]
    fn rpm080_silent_when_extra_item_outside_inner() {
        let src = "Name: x\n%if 1\nLicense: MIT\n%if 1\nVersion: 1\n%endif\n%endif\n";
        assert!(run(src, NestedAndCollapse::new()).is_empty());
    }

    #[test]
    fn rpm080_silent_for_ifarch_outer() {
        // `%ifarch` can't be merged with a plain `%if` via `&&`.
        let src = "Name: x\n%ifarch x86_64\n%if 1\nVersion: 1\n%endif\n%endif\n";
        assert!(run(src, NestedAndCollapse::new()).is_empty());
    }

    #[test]
    fn rpm080_allows_blank_around_inner() {
        // Blank lines inside outer's body don't count as content.
        let src = "Name: x\n%if 1\n\n%if 1\nVersion: 1\n%endif\n\n%endif\n";
        let diags = run(src, NestedAndCollapse::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm080_flags_conditional_macro_in_inner() {
        // Inner uses `%{?sle_version}` — conditional ref, safe to
        // merge: undefined → empty → `0 >= 1` → false.
        let src = "Name: x\n%if \"%{?_vendor}\" == \"suse\"\n\
                   %if 0%{?sle_version} >= 120000\nVersion: 1\n%endif\n%endif\n";
        let diags = run(src, NestedAndCollapse::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm080_silent_for_unconditional_braced_macro_in_inner() {
        // Inner references `%{pgsql_major}` without `?`. On a system
        // where that macro is undefined, evaluating the merged
        // condition would parse-error. Skip the suggestion.
        let src = "Name: x\n%if 1\n%if %{pgsql_major} >= 17\nVersion: 1\n%endif\n%endif\n";
        assert!(
            run(src, NestedAndCollapse::new()).is_empty(),
            "should bail on unconditional %{{...}} in inner"
        );
    }

    #[test]
    fn rpm080_silent_for_bare_macro_in_inner() {
        // `%pgsql_major` bare form — same risk as `%{pgsql_major}`.
        let src = "Name: x\n%if 1\n%if %pgsql_major >= 17\nVersion: 1\n%endif\n%endif\n";
        assert!(
            run(src, NestedAndCollapse::new()).is_empty(),
            "should bail on bare %name in inner"
        );
    }

    #[test]
    fn rpm080_flags_double_percent_escape_in_inner() {
        // `%%` is an escaped literal `%`, not a macro reference.
        // Inner has no real macro refs — safe.
        let src = "Name: x\n%if 1\n%if \"%%foo\" == \"%foo\"\nVersion: 1\n%endif\n%endif\n";
        // NB: this is a deliberately weird condition; the point is
        // that `%%foo` should NOT count as an unconditional macro.
        // The `\"%foo\"` later WOULD count, but in this test we keep
        // only `%%foo`:
        let safer = "Name: x\n%if 1\n%if 1 == 1\nVersion: 1\n%endif\n%endif\n";
        assert_eq!(run(safer, NestedAndCollapse::new()).len(), 1);
        // The original (with bare `%foo`) is correctly rejected:
        assert!(run(src, NestedAndCollapse::new()).is_empty());
    }

    #[test]
    fn rpm080_unit_inner_expr_safe_to_merge() {
        // Direct unit tests for the safety classifier.
        assert!(inner_expr_safe_to_merge("1"));
        assert!(inner_expr_safe_to_merge("1 || 0"));
        assert!(inner_expr_safe_to_merge("0%{?foo} >= 1"));
        assert!(inner_expr_safe_to_merge("%{!?foo:1}"));
        assert!(inner_expr_safe_to_merge("\"%%x\" == \"y\""));
        assert!(!inner_expr_safe_to_merge("%{foo}"));
        assert!(!inner_expr_safe_to_merge("%foo"));
        assert!(!inner_expr_safe_to_merge("%(echo hi)"));
        assert!(!inner_expr_safe_to_merge("0%{?ok} && %{bad}"));
    }
}
