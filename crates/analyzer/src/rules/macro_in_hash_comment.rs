//! RPM036 `macro-in-hash-comment` — `#`-style comments in RPM specs
//! **are** interpreted: rpm expands macros inside them before
//! discarding the comment. That means `# %{name}-debug` silently
//! executes whatever `%name` evaluates to and may even produce side
//! effects (`%(shell command)`, `%{lua:...}`). The only truly inert
//! comment form is `%dnl`.
//!
//! We flag any `# ...` comment whose text contains a `TextSegment::Macro`
//! and suggest escaping each `%` to `%%`. The actual edit needs to
//! touch the source bytes inside the comment span, so the rule keeps
//! the raw source string from [`Lint::set_source`] and constructs
//! `MachineApplicable` edits when available; otherwise the diagnostic
//! degrades to a `Manual` suggestion.

use rpm_spec::ast::{
    Comment, CommentStyle, Conditional, MacroDef, MacroRef, Span, SpecFile, SpecItem, TextSegment,
};

use crate::diagnostic::{Applicability, Diagnostic, Edit, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

/// Distance in source lines within which a parametric `%define` is considered
/// to belong to the preceding `#` documentation block. RPM specs in the wild
/// (kernel.spec et al.) routinely document parametric macros with 1-10 lines
/// of `#`-prefixed prose immediately above the `%define NAME(opts)`.
const DOC_LOOKAHEAD_LINES: u32 = 10;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM036",
    name: "macro-in-hash-comment",
    description: "`#` comments expand macros — escape each `%` to `%%` or use `%dnl` for a no-expand comment.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// `#` comments expand macros — escape each `%` to `%%` or use `%dnl` for a no-expand comment.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MacroInHashComment {
    diagnostics: Vec<Diagnostic>,
    source: Option<std::sync::Arc<str>>,
    /// Sorted starting lines of parametric `%define`s (those whose `opts` is
    /// `Some(...)`). Used to suppress diagnostics on documentation comments
    /// that describe positional arguments (`%1`, `%*`, `%#`) of a parametric
    /// macro that follows within `DOC_LOOKAHEAD_LINES`.
    parametric_lines: Vec<u32>,
}

impl MacroInHashComment {
    pub fn new() -> Self {
        Self::default()
    }

    /// True if any parametric `%define` starts within the window
    /// `[comment.start_line, comment.start_line + DOC_LOOKAHEAD_LINES]`.
    fn has_parametric_define_within(&self, comment: Span) -> bool {
        let lo = comment.start_line;
        let hi = lo.saturating_add(DOC_LOOKAHEAD_LINES);
        // `parametric_lines` is sorted; binary-search for the first candidate
        // >= lo and check that it's also <= hi.
        let idx = self.parametric_lines.partition_point(|&l| l < lo);
        self.parametric_lines.get(idx).is_some_and(|&l| l <= hi)
    }
}

fn comment_contains_macro(node: &Comment<Span>) -> bool {
    node.text
        .segments
        .iter()
        .any(|seg| matches!(seg, TextSegment::Macro(_)))
}

/// True when the macro reference is a positional placeholder:
/// `%1`..`%9` / `%{1}`..`%{9}` (and `%0`), `%*`, `%**`, or `%#`.
fn is_positional_ref(m: &MacroRef) -> bool {
    m.positional_index().is_some() || m.is_all_positional() || m.is_all_args() || m.is_arg_count()
}

/// True when *every* macro segment in the comment is a positional placeholder.
///
/// We require all-positional (not "any positional") because a comment like
/// `# %1 documents %{my_global}` still has a genuinely-dangerous reference
/// (`%{my_global}`) and must be flagged.
fn comment_macros_all_positional(node: &Comment<Span>) -> bool {
    let mut saw_macro = false;
    for seg in &node.text.segments {
        if let TextSegment::Macro(m) = seg {
            saw_macro = true;
            if !is_positional_ref(m) {
                return false;
            }
        }
    }
    saw_macro
}

/// Walk the spec collecting starting lines of every parametric `%define`.
///
/// "Parametric" means the definition carries an `opts` list — `%define foo()
/// ...` and `%define foo(a:b) ...` both qualify. Plain `%define foo bar` does
/// not, because there are no positional arguments to document.
fn collect_parametric_define_lines(spec: &SpecFile<Span>) -> Vec<u32> {
    let mut out = Vec::new();
    collect_from_items(&spec.items, &mut out);
    out.sort_unstable();
    out.dedup();
    out
}

fn collect_from_items(items: &[SpecItem<Span>], out: &mut Vec<u32>) {
    for item in items {
        match item {
            SpecItem::MacroDef(m) => collect_from_macro_def(m, out),
            SpecItem::Conditional(c) => collect_from_conditional(c, out),
            _ => {}
        }
    }
}

fn collect_from_macro_def(m: &MacroDef<Span>, out: &mut Vec<u32>) {
    if m.opts.is_some() {
        out.push(m.data.start_line);
    }
}

fn collect_from_conditional(cond: &Conditional<Span, SpecItem<Span>>, out: &mut Vec<u32>) {
    for branch in &cond.branches {
        collect_from_items(&branch.body, out);
    }
    if let Some(els) = &cond.otherwise {
        collect_from_items(els, out);
    }
}

/// Build edits that replace every standalone `%` inside the comment's
/// byte range with `%%`. Skips `%%` (already escaped) and `%dnl` —
/// though the latter shouldn't appear inside a `Hash` comment.
fn build_escape_edits(source: &str, span: Span) -> Vec<Edit> {
    let start = span.start_byte.min(source.len());
    let end = span.end_byte.min(source.len());
    if start >= end {
        return Vec::new();
    }
    let slice = &source[start..end];
    let bytes = slice.as_bytes();
    let mut edits = Vec::new();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] != b'%' {
            idx += 1;
            continue;
        }
        // Skip `%%` (already escaped — advance past both bytes).
        if idx + 1 < bytes.len() && bytes[idx + 1] == b'%' {
            idx += 2;
            continue;
        }
        // Insert a `%` before this position to make the existing `%`
        // part of `%%`. The fixer applies edits in descending order,
        // so each insertion is independent.
        let abs = start + idx;
        edits.push(Edit::new(Span::from_bytes(abs, abs), "%".to_owned()));
        idx += 1;
    }
    edits
}

impl<'ast> Visit<'ast> for MacroInHashComment {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        self.parametric_lines = collect_parametric_define_lines(spec);
        crate::visit::walk_spec(self, spec);
    }

    fn visit_comment(&mut self, node: &'ast Comment<Span>) {
        if !matches!(node.style, CommentStyle::Hash) {
            return;
        }
        if !comment_contains_macro(node) {
            return;
        }
        if comment_macros_all_positional(node) && self.has_parametric_define_within(node.data) {
            // Documentation comment describing the positional args of a
            // parametric macro defined a few lines below. RPM does expand
            // `%1` here at the top level, but the spec author's intent is
            // clearly documentation and the convention is too well-established
            // to flag. See kernel.spec for ~20 instances of this pattern.
            return;
        }
        let mut diag = Diagnostic::new(
            &METADATA,
            Severity::Warn,
            "macro reference inside `#` comment will be expanded by rpm",
            node.data,
        );
        let suggestion = match self.source.as_deref() {
            Some(source) => {
                let edits = build_escape_edits(source, node.data);
                if edits.is_empty() {
                    Suggestion::new(
                        "escape each `%` to `%%` or change `#` to `%dnl`",
                        Vec::new(),
                        Applicability::Manual,
                    )
                } else {
                    Suggestion::new(
                        "escape each `%` to `%%` so rpm leaves the text alone",
                        edits,
                        Applicability::MachineApplicable,
                    )
                }
            }
            None => Suggestion::new(
                "escape each `%` to `%%` or change `#` to `%dnl`",
                Vec::new(),
                Applicability::Manual,
            ),
        };
        diag = diag.with_suggestion(suggestion);
        self.diagnostics.push(diag);
    }
}

impl Lint for MacroInHashComment {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: std::sync::Arc<str>) {
        self.source = Some(source);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<MacroInHashComment>(src)
    }

    #[test]
    fn flags_macro_in_hash_comment() {
        let src = "Name: x\n# %{name} is the package\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM036");
        assert!(!diags[0].suggestions.is_empty());
        // Edits should turn `%` into `%%`.
        let edits = &diags[0].suggestions[0].edits;
        assert!(!edits.is_empty(), "expected escape edits");
    }

    #[test]
    fn silent_for_plain_comment() {
        let src = "Name: x\n# plain text without macros\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_dnl_comment() {
        // %dnl comments don't expand macros, so they're safe.
        let src = "Name: x\n%dnl %{name} is the package\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_inline_macro_call() {
        // `%(shell)` would silently run a subshell.
        let src = "Name: x\n# safe: %(echo hi)\n";
        assert!(!run(src).is_empty());
    }

    #[test]
    fn silent_for_doc_comment_describing_parametric_macro_arg() {
        // Documentation pattern: `#` prose lines (referring to %1, %2 …)
        // immediately above a parametric `%define foo(a:) ...`. The author's
        // intent is plainly documentation; suppress the diagnostic.
        let src = "Name: x\n\
                   # foo: Compute something\n\
                   #   %1 - first argument\n\
                   %define foo(a:) echo %1\n";
        let diags = run(src);
        assert!(
            diags.is_empty(),
            "expected zero RPM036 diagnostics on documentation comments, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn still_flags_non_positional_macro_in_comment() {
        // `%{my_global}` is not a positional placeholder — it's a genuine
        // macro reference that rpm will expand. Suppression must not apply.
        let src = "Name: x\n# %{my_global} is the value\n%global my_global 42\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM036");
    }

    #[test]
    fn still_flags_positional_when_no_parametric_define_follows() {
        // No `%define NAME(args)` within the next 10 lines, so the `%1` in
        // the comment is genuinely unsafe (rpm would expand it to whatever
        // the enclosing context defines).
        let src = "Name: x\n# %1 means nothing here\necho hi\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM036");
    }
}
