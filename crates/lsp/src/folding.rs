//! `textDocument/foldingRange` — collapsible regions for spec
//! sections and `%if`/`%endif` blocks.
//!
//! Walks the same `SpecFile<Span>` that powers
//! [`crate::outline`], but emits a flat list of [`FoldingRange`]s
//! instead of the nested `DocumentSymbol` tree. Editors collapse on
//! line ranges, so we don't need any kind / hierarchy here — just the
//! start / end line for every foldable construct.

use lsp_types::{FoldingRange, FoldingRangeKind};
use rpm_spec::ast::{Conditional, Section, Span, SpecFile, SpecItem};

/// Build the folding range list for a parsed spec.
pub fn build(spec: &SpecFile<Span>) -> Vec<FoldingRange> {
    let mut out = Vec::new();
    for item in &spec.items {
        push_item(item, &mut out);
    }
    out
}

fn push_item(item: &SpecItem<Span>, out: &mut Vec<FoldingRange>) {
    match item {
        SpecItem::Section(boxed) => push_section(boxed.as_ref(), out),
        SpecItem::Conditional(cond) => push_cond(cond, out),
        // Preamble entries, macro defs, etc. are single-line — nothing
        // to fold.
        _ => {}
    }
}

fn push_section(section: &Section<Span>, out: &mut Vec<FoldingRange>) {
    let Some(span) = section_span(section) else {
        return;
    };
    if let Some(range) = span_to_fold(span) {
        out.push(range);
    }
}

fn push_cond(cond: &Conditional<Span, SpecItem<Span>>, out: &mut Vec<FoldingRange>) {
    if let Some(range) = span_to_fold(&cond.data) {
        out.push(range);
    }
    // Walk into each branch + the else body so nested sections also
    // become foldable. Branch bodies themselves don't carry their own
    // outer span — the `Conditional::data` covers the whole block —
    // so we don't emit a fold per-branch.
    for branch in &cond.branches {
        for nested in &branch.body {
            push_item(nested, out);
        }
    }
    if let Some(otherwise) = &cond.otherwise {
        for nested in otherwise {
            push_item(nested, out);
        }
    }
}

fn section_span(section: &Section<Span>) -> Option<&Span> {
    match section {
        Section::BuildScript { data, .. } => Some(data),
        Section::Package { data, .. } => Some(data),
        Section::Description { data, .. } => Some(data),
        Section::Files { data, .. } => Some(data),
        Section::Scriptlet(s) => Some(&s.data),
        Section::Trigger(t) => Some(&t.data),
        Section::FileTrigger(t) => Some(&t.data),
        Section::Verify { data, .. } => Some(data),
        Section::Changelog { data, .. } => Some(data),
        Section::SourceList { data, .. } => Some(data),
        Section::PatchList { data, .. } => Some(data),
        Section::Sepolicy { data, .. } => Some(data),
        // Upstream is `#[non_exhaustive]` — a new variant just won't
        // show up in the fold list until we add a branch for it.
        _ => None,
    }
}

/// Convert a span to a `FoldingRange`. Returns `None` when the span
/// covers a single line (nothing to fold) or is degenerate.
fn span_to_fold(span: &Span) -> Option<FoldingRange> {
    if span.start_line == 0 || span.end_line == 0 {
        return None;
    }
    // LSP lines are 0-based; analyzer spans are 1-based.
    let start = span.start_line.saturating_sub(1);
    let end = span.end_line.saturating_sub(1);
    if end <= start {
        return None;
    }
    Some(FoldingRange {
        start_line: start,
        start_character: None,
        end_line: end,
        end_character: None,
        kind: Some(FoldingRangeKind::Region),
        collapsed_text: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_analyzer::parse;

    #[test]
    fn each_section_folds() {
        let src = "Name: hello\n\
                   %prep\n\
                   set -x\n\
                   make config\n\
                   %build\n\
                   make\n\
                   make more\n\
                   %install\n\
                   make install\n\
                   %changelog\n\
                   * Mon Jan 01 2024 a <a@b> - 1-1\n\
                   - init\n";
        let outcome = parse(src);
        let ranges = build(&outcome.spec);
        // 4 multi-line sections: %prep, %build, %install, %changelog.
        assert!(
            ranges.len() >= 3,
            "expected at least 3 folds, got {ranges:?}"
        );
        // Every fold must span more than one line.
        for r in &ranges {
            assert!(r.end_line > r.start_line, "single-line fold: {r:?}");
        }
    }

    #[test]
    fn conditional_block_is_foldable() {
        let src = "Name: hello\n\
                   %if 0%{?fedora}\n\
                   %global pyver 3\n\
                   %else\n\
                   %global pyver 2\n\
                   %endif\n";
        let outcome = parse(src);
        let ranges = build(&outcome.spec);
        // The %if/%endif block spans 4 lines (indices 1..5).
        assert!(
            ranges.iter().any(|r| r.start_line == 1 && r.end_line >= 4),
            "expected fold over the %if block: {ranges:?}"
        );
    }
}
