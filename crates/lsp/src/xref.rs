//! Macro cross-references — goto definition, find references,
//! document highlights.
//!
//! All three operate on the same scan output that powers
//! [`crate::rename`]: an `%foo` cursor sees its `%define foo` and
//! every `%foo` / `%{foo}` / `%{?foo}` sibling. Keywords like
//! `%prep`/`%if` are filtered out by the scanner so they never
//! resolve as a rename or jump target.

use lsp_types::{DocumentHighlight, DocumentHighlightKind, Location, Position, Uri};

use crate::encoding::{LineIndex, PositionEncoding};
use crate::rename::{OccKind, Occurrence, position_to_byte, scan};

/// Look up the macro name at `pos` and return the matching occurrences
/// — the one under the cursor first, then siblings in source order.
fn occurrences_for(
    source: &str,
    pos: Position,
    enc: PositionEncoding,
) -> Option<(String, Vec<Occurrence>)> {
    let cursor = position_to_byte(source, pos, enc)?;
    let all = scan(source);
    let target = all
        .iter()
        .find(|o| cursor >= o.name.start && cursor <= o.name.end)?;
    let name = source[target.name.clone()].to_string();
    let hits: Vec<Occurrence> = all
        .into_iter()
        .filter(|o| source[o.name.clone()] == name)
        .collect();
    Some((name, hits))
}

/// `textDocument/definition` — jump to the `%define`/`%global`
/// statement for the macro under the cursor. Returns `None` when the
/// cursor isn't on a macro or the macro is referenced but not defined
/// in this file.
pub fn goto_definition(
    uri: &Uri,
    source: &str,
    index: &LineIndex,
    enc: PositionEncoding,
    pos: Position,
) -> Option<Location> {
    let (_, hits) = occurrences_for(source, pos, enc)?;
    let def = hits.iter().find(|o| o.kind == OccKind::Definition)?;
    Some(Location {
        uri: uri.clone(),
        range: index.range(source, def.name.start, def.name.end, enc),
    })
}

/// `textDocument/references` — every macro-name occurrence with the
/// same name. `include_declaration` controls whether `%define`/`%global`
/// sites are returned alongside the `%foo` references.
pub fn references(
    uri: &Uri,
    source: &str,
    index: &LineIndex,
    enc: PositionEncoding,
    pos: Position,
    include_declaration: bool,
) -> Vec<Location> {
    let Some((_, hits)) = occurrences_for(source, pos, enc) else {
        return Vec::new();
    };
    hits.into_iter()
        .filter(|o| include_declaration || o.kind != OccKind::Definition)
        .map(|o| Location {
            uri: uri.clone(),
            range: index.range(source, o.name.start, o.name.end, enc),
        })
        .collect()
}

/// `textDocument/documentHighlight` — same set as `references`, but
/// returned as `DocumentHighlight` with kind hints so editors can
/// colour write-sites (definitions) differently from reads.
pub fn document_highlight(
    source: &str,
    index: &LineIndex,
    enc: PositionEncoding,
    pos: Position,
) -> Vec<DocumentHighlight> {
    let Some((_, hits)) = occurrences_for(source, pos, enc) else {
        return Vec::new();
    };
    hits.into_iter()
        .map(|o| {
            let kind = match o.kind {
                OccKind::Definition => DocumentHighlightKind::WRITE,
                OccKind::Reference => DocumentHighlightKind::READ,
            };
            DocumentHighlight {
                range: index.range(source, o.name.start, o.name.end, enc),
                kind: Some(kind),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri() -> Uri {
        "file:///t.spec".parse().unwrap()
    }

    #[test]
    fn goto_definition_returns_define_site() {
        let src = "%define foo bar\n%foo\n";
        let idx = LineIndex::new(src);
        // Cursor inside the reference `%foo`.
        let loc = goto_definition(
            &uri(),
            src,
            &idx,
            PositionEncoding::Utf16,
            Position::new(1, 2),
        )
        .expect("definition");
        // The define site is on line 0; the *name* starts after `%define `.
        assert_eq!(loc.range.start.line, 0);
        assert_eq!(loc.range.start.character, 8);
        assert_eq!(loc.range.end.character, 11);
    }

    #[test]
    fn goto_definition_returns_none_when_no_define() {
        let src = "%foo\n";
        let idx = LineIndex::new(src);
        let loc = goto_definition(
            &uri(),
            src,
            &idx,
            PositionEncoding::Utf16,
            Position::new(0, 2),
        );
        assert!(loc.is_none(), "no %define means no jump target");
    }

    #[test]
    fn references_includes_decl_when_requested() {
        let src = "%define foo 1\n%foo %{foo}\n";
        let idx = LineIndex::new(src);
        let with = references(
            &uri(),
            src,
            &idx,
            PositionEncoding::Utf16,
            Position::new(1, 2),
            true,
        );
        let without = references(
            &uri(),
            src,
            &idx,
            PositionEncoding::Utf16,
            Position::new(1, 2),
            false,
        );
        assert_eq!(with.len(), 3);
        assert_eq!(without.len(), 2);
    }

    #[test]
    fn document_highlight_separates_read_and_write() {
        let src = "%define foo 1\n%foo\n";
        let idx = LineIndex::new(src);
        let hl = document_highlight(src, &idx, PositionEncoding::Utf16, Position::new(0, 9));
        assert_eq!(hl.len(), 2);
        let kinds: Vec<_> = hl.iter().map(|h| h.kind).collect();
        assert!(kinds.contains(&Some(DocumentHighlightKind::WRITE)));
        assert!(kinds.contains(&Some(DocumentHighlightKind::READ)));
    }

    #[test]
    fn keyword_under_cursor_yields_nothing() {
        // `%build` is a section keyword, not a macro — the scanner
        // skips it, so xref should not invent a target.
        let src = "%build\necho hi\n";
        let idx = LineIndex::new(src);
        assert!(
            goto_definition(
                &uri(),
                src,
                &idx,
                PositionEncoding::Utf16,
                Position::new(0, 2)
            )
            .is_none()
        );
        assert!(
            references(
                &uri(),
                src,
                &idx,
                PositionEncoding::Utf16,
                Position::new(0, 2),
                true,
            )
            .is_empty()
        );
        assert!(
            document_highlight(src, &idx, PositionEncoding::Utf16, Position::new(0, 2)).is_empty()
        );
    }
}
