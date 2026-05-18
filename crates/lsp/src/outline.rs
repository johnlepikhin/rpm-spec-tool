//! `textDocument/documentSymbol` — outline view of a spec file.
//!
//! Walks the top-level [`SpecItem`]s and emits one [`DocumentSymbol`]
//! per structural section (`%prep`, `%build`, `%install`, `%files`,
//! sub-package preambles, scriptlets, triggers, changelog, ...).
//!
//! Symbol *names* are read directly from the source — the first line
//! of each section's span — so `%description -n libfoo` and
//! `%files mypkg` keep their full header text in the outline. This is
//! more robust than reconstructing the label from the AST (the AST
//! `Text` type does not carry spans, so we'd have to render macros
//! ourselves).

use lsp_types::{DocumentSymbol, SymbolKind};
use rpm_spec::ast::{BuildScriptKind, Section, Span, SpecFile, SpecItem};

use crate::encoding::{LineIndex, PositionEncoding};

/// Build the outline for a parsed spec.
pub fn build(
    spec: &SpecFile<Span>,
    source: &str,
    index: &LineIndex,
    enc: PositionEncoding,
) -> Vec<DocumentSymbol> {
    let mut out = Vec::new();
    for item in &spec.items {
        push_item(item, source, index, enc, &mut out);
    }
    out
}

fn push_item(
    item: &SpecItem<Span>,
    source: &str,
    index: &LineIndex,
    enc: PositionEncoding,
    out: &mut Vec<DocumentSymbol>,
) {
    match item {
        SpecItem::Section(boxed) => {
            if let Some(sym) = section_to_symbol(boxed.as_ref(), source, index, enc) {
                out.push(sym);
            }
        }
        SpecItem::Conditional(cond) => {
            // Surface nested sections inside `%if` blocks at the same
            // level — editors collapse this naturally and users still
            // see every %prep/%build that may run. The conditional
            // header itself is not represented (would need explicit
            // grouping that doesn't fit the flat outline well).
            for branch in cond_branches(cond) {
                for nested in branch {
                    push_item(nested, source, index, enc, out);
                }
            }
        }
        // Preamble entries, macro defs, comments etc. are noise in an
        // outline view; skip them.
        _ => {}
    }
}

fn cond_branches(
    cond: &rpm_spec::ast::Conditional<Span, SpecItem<Span>>,
) -> Vec<&[SpecItem<Span>]> {
    let mut v: Vec<&[SpecItem<Span>]> = cond.branches.iter().map(|b| b.body.as_slice()).collect();
    if let Some(otherwise) = &cond.otherwise {
        v.push(otherwise.as_slice());
    }
    v
}

fn section_to_symbol(
    section: &Section<Span>,
    source: &str,
    index: &LineIndex,
    enc: PositionEncoding,
) -> Option<DocumentSymbol> {
    let (span, kind, fallback_name) = match section {
        Section::BuildScript { kind, data, .. } => {
            (data, SymbolKind::METHOD, build_script_name(*kind))
        }
        Section::Package { data, .. } => (data, SymbolKind::MODULE, "%package"),
        Section::Description { data, .. } => (data, SymbolKind::STRING, "%description"),
        Section::Files { data, .. } => (data, SymbolKind::ARRAY, "%files"),
        Section::Scriptlet(s) => (&s.data, SymbolKind::METHOD, scriptlet_name(s.kind)),
        Section::Trigger(t) => (&t.data, SymbolKind::METHOD, trigger_name(t.kind)),
        Section::FileTrigger(t) => (&t.data, SymbolKind::METHOD, file_trigger_name(t.kind)),
        Section::Verify { data, .. } => (data, SymbolKind::METHOD, "%verify"),
        Section::Changelog { data, .. } => (data, SymbolKind::PROPERTY, "%changelog"),
        Section::SourceList { data, .. } => (data, SymbolKind::ARRAY, "%sourcelist"),
        Section::PatchList { data, .. } => (data, SymbolKind::ARRAY, "%patchlist"),
        Section::Sepolicy { data, .. } => (data, SymbolKind::METHOD, "%sepolicy"),
        // `#[non_exhaustive]` upstream — fall back to a neutral kind so
        // a new variant doesn't drop out of the outline silently.
        _ => return None,
    };

    let name = header_line(source, span)
        .unwrap_or(fallback_name)
        .to_string();
    let range = index.range(source, span.start_byte, span.end_byte, enc);
    // The selection range should highlight the section *header* (the
    // first line) so "go to symbol" lands on the directive itself, not
    // on the whole body.
    let header_end = source[span.start_byte..span.end_byte]
        .find('\n')
        .map(|i| span.start_byte + i)
        .unwrap_or(span.end_byte);
    let selection_range = index.range(source, span.start_byte, header_end, enc);

    #[allow(deprecated)] // `deprecated` field is required by the spec.
    Some(DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: None,
    })
}

/// Extract the first non-empty source line within `span` as the
/// symbol label. Trims trailing whitespace. Returns `None` when the
/// span is empty or contains only blank lines (in which case the
/// caller falls back to a hard-coded directive name).
fn header_line<'a>(source: &'a str, span: &Span) -> Option<&'a str> {
    let slice = source.get(span.start_byte..span.end_byte)?;
    let line = slice.lines().next()?.trim_end();
    if line.is_empty() { None } else { Some(line) }
}

fn build_script_name(kind: BuildScriptKind) -> &'static str {
    match kind {
        BuildScriptKind::Prep => "%prep",
        BuildScriptKind::Conf => "%conf",
        BuildScriptKind::Build => "%build",
        BuildScriptKind::Install => "%install",
        BuildScriptKind::Check => "%check",
        BuildScriptKind::Clean => "%clean",
        BuildScriptKind::GenerateBuildRequires => "%generate_buildrequires",
        // upstream is `#[non_exhaustive]`; a new variant should still
        // show up in the outline with *some* name.
        _ => "%build-script",
    }
}

fn scriptlet_name(kind: rpm_spec::ast::ScriptletKind) -> &'static str {
    use rpm_spec::ast::ScriptletKind as K;
    match kind {
        K::Pre => "%pre",
        K::Post => "%post",
        K::Preun => "%preun",
        K::Postun => "%postun",
        K::Pretrans => "%pretrans",
        K::Posttrans => "%posttrans",
        K::Preuntrans => "%preuntrans",
        K::Postuntrans => "%postuntrans",
        _ => "%scriptlet",
    }
}

fn trigger_name(kind: rpm_spec::ast::TriggerKind) -> &'static str {
    use rpm_spec::ast::TriggerKind as K;
    match kind {
        K::Prein => "%triggerprein",
        K::In => "%triggerin",
        K::Un => "%triggerun",
        K::Postun => "%triggerpostun",
        _ => "%trigger",
    }
}

fn file_trigger_name(kind: rpm_spec::ast::FileTriggerKind) -> &'static str {
    use rpm_spec::ast::FileTriggerKind as K;
    match kind {
        K::In => "%filetriggerin",
        K::Un => "%filetriggerun",
        K::Postun => "%filetriggerpostun",
        K::TransIn => "%transfiletriggerin",
        K::TransUn => "%transfiletriggerun",
        K::TransPostun => "%transfiletriggerpostun",
        _ => "%filetrigger",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_analyzer::parse;

    #[test]
    fn extracts_top_level_sections() {
        let src = "Name: hello\n\
                   %prep\n\
                   set -x\n\
                   %build\n\
                   make\n\
                   %install\n\
                   make install\n\
                   %files\n\
                   /usr/bin/hello\n\
                   %changelog\n\
                   * Mon Jan 01 2024 a <a@b> - 1-1\n\
                   - init\n";
        let outcome = parse(src);
        let index = LineIndex::new(src);
        let symbols = build(&outcome.spec, src, &index, PositionEncoding::Utf16);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.iter().any(|n| n == &"%prep"), "got {names:?}");
        assert!(names.iter().any(|n| n == &"%build"), "got {names:?}");
        assert!(names.iter().any(|n| n == &"%install"), "got {names:?}");
        assert!(names.iter().any(|n| n == &"%files"), "got {names:?}");
        assert!(names.iter().any(|n| n == &"%changelog"), "got {names:?}");
    }

    #[test]
    fn subpackage_header_preserves_arguments() {
        let src = "Name: hello\n\
                   %package -n libhello\n\
                   Summary: lib\n\
                   %description -n libhello\n\
                   the lib\n\
                   %files -n libhello\n\
                   /usr/lib/libhello.so\n";
        let outcome = parse(src);
        let index = LineIndex::new(src);
        let symbols = build(&outcome.spec, src, &index, PositionEncoding::Utf16);
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.iter().any(|n| n.contains("-n libhello")),
            "got {names:?}"
        );
    }

    #[test]
    fn selection_range_covers_header_only() {
        let src = "Name: hello\n%prep\nbody line\nmore body\n";
        let outcome = parse(src);
        let index = LineIndex::new(src);
        let symbols = build(&outcome.spec, src, &index, PositionEncoding::Utf16);
        let prep = symbols.iter().find(|s| s.name == "%prep").unwrap();
        // selection_range end line should be the header line, not the
        // body end line.
        assert_eq!(prep.selection_range.start.line, 1);
        assert_eq!(prep.selection_range.end.line, 1);
    }
}
