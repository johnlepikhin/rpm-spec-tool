//! `textDocument/prepareRename` and `textDocument/rename` for
//! user-defined macros.
//!
//! Scope:
//! * Renames `%define`/`%global`/`%undefine NAME` definition sites.
//! * Renames every `%NAME`, `%{NAME}`, `%{?NAME}`, `%{!?NAME}`,
//!   `%{?NAME:VALUE}` reference in the same file.
//! * Does NOT rename built-in directives (`%prep`, `%if`, …) — those
//!   are grammar keywords, not macros.
//! * Does NOT touch sub-package names, source/patch numbers, or tags.
//!
//! Implementation is a text scan rather than an AST walk: the `Text`
//! AST type doesn't carry per-segment spans for macro names, so a
//! byte-level scanner is more robust than reconstructing positions
//! from the AST. The scanner recognises the same surface forms the
//! `rpm-spec` parser does.

use std::collections::HashMap;
use std::ops::Range;

use lsp_types::{
    Position, Range as LspRange, TextDocumentPositionParams, TextEdit, Uri, WorkspaceEdit,
};
use rpm_spec_profile::macro_lexer::{MacroKind, is_ident_start, iter_macro_refs, scan_ident};

use crate::encoding::{LineIndex, PositionEncoding};
use crate::hover::DIRECTIVES;

/// One macro-name occurrence in the source. `byte_range` covers
/// *only the identifier* — never the leading `%`, the surrounding
/// braces, or the `?` / `!?` prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub name: Range<usize>,
    pub kind: OccKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OccKind {
    /// `%define`/`%global`/`%undefine NAME`.
    Definition,
    /// `%NAME` / `%{NAME}` / `%{?NAME}` / `%{!?NAME}` / `%{?NAME:V}`.
    Reference,
}

/// Compute the rename "anchor" range — the source bytes of the macro
/// name currently under the cursor. Returns `None` when the cursor
/// isn't on a renameable macro name (whitespace, value text, a
/// keyword like `%prep`, etc.).
pub fn prepare(
    source: &str,
    pos: Position,
    index: &LineIndex,
    enc: PositionEncoding,
) -> Option<LspRange> {
    let byte = position_to_byte(source, pos, enc)?;
    let occ = scan(source).into_iter().find(|o| {
        // `<=` on the upper bound so the very last byte of the name
        // (where the cursor often parks after typing) still counts.
        byte >= o.name.start && byte <= o.name.end
    })?;
    Some(index.range(source, occ.name.start, occ.name.end, enc))
}

/// Build the `WorkspaceEdit` that renames every occurrence of the
/// macro under the cursor to `new_name`. Returns `None` when the
/// cursor isn't on a macro, or when `new_name` is not a valid macro
/// identifier.
pub fn rename(
    uri: &Uri,
    source: &str,
    index: &LineIndex,
    enc: PositionEncoding,
    params: &TextDocumentPositionParams,
    new_name: &str,
) -> Option<WorkspaceEdit> {
    if !is_valid_macro_name(new_name) {
        return None;
    }
    let cursor = position_to_byte(source, params.position, enc)?;
    let occurrences = scan(source);
    // Identify the macro under the cursor.
    let target = occurrences
        .iter()
        .find(|o| cursor >= o.name.start && cursor <= o.name.end)?;
    let target_name = &source[target.name.clone()];
    if target_name == new_name {
        // No-op rename: return an empty edit so the client doesn't
        // think something failed.
        return Some(WorkspaceEdit::default());
    }
    let edits: Vec<TextEdit> = occurrences
        .iter()
        .filter(|o| &source[o.name.clone()] == target_name)
        .map(|o| TextEdit {
            range: index.range(source, o.name.start, o.name.end, enc),
            new_text: new_name.to_string(),
        })
        .collect();
    if edits.is_empty() {
        return None;
    }
    let mut changes = HashMap::new();
    changes.insert(uri.clone(), edits);
    Some(WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

/// Find every macro-name occurrence in `source`.
///
/// Token scanning is delegated to
/// [`rpm_spec_profile::macro_lexer::iter_macro_refs`]; this function
/// adds rename-specific post-processing on top:
///
/// * `%define` / `%global` / `%undefine NAME` produce a
///   [`OccKind::Definition`] occurrence at the *operand* identifier,
///   not at the keyword itself.
/// * RPM keywords (`%if`, `%prep`, `%with`, …) are filtered out — they
///   are grammar, not user-renameable macros.
/// * Shell-expansion (`%(...)`) and arithmetic (`%[...]`) bodies are
///   skipped so identifiers inside arbitrary shell don't appear as
///   rename candidates.
pub(crate) fn scan(source: &str) -> Vec<Occurrence> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    for r in iter_macro_refs(bytes) {
        match r.kind {
            // Skip literal `%%`, shell `%(...)`, arithmetic `%[...]`.
            MacroKind::LiteralPercent | MacroKind::ShellExpansion | MacroKind::ArithmeticExpr => {
                continue;
            }
            MacroKind::Plain => {
                // `%define`/`%global`/`%undefine NAME` — emit the
                // operand as a Definition.
                if matches!(r.name, "define" | "global" | "undefine") {
                    let mut j = r.full_range.end;
                    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                        j += 1;
                    }
                    if let Some(op_end) = scan_ident(bytes, j) {
                        out.push(Occurrence {
                            name: j..op_end,
                            kind: OccKind::Definition,
                        });
                    }
                    continue;
                }
                // Other RPM keywords — not macros.
                if is_keyword(r.name) {
                    continue;
                }
                out.push(Occurrence {
                    name: r.name_range.clone(),
                    kind: OccKind::Reference,
                });
            }
            MacroKind::Braced { .. } => {
                out.push(Occurrence {
                    name: r.name_range.clone(),
                    kind: OccKind::Reference,
                });
            }
        }
    }
    out
}

/// Macro names follow the same rules as identifiers: must start with
/// a letter / underscore, then letters / digits / underscores.
fn is_valid_macro_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let first = name.as_bytes()[0];
    if !is_ident_start(first) {
        return false;
    }
    name.bytes()
        .skip(1)
        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// `%NAME` keyword check. Reuses [`crate::hover::DIRECTIVES`] (the
/// same canonical list the hover and completion modules consult), and
/// adds a handful of names that aren't directives but are still RPM
/// keywords from a macro-rename perspective.
fn is_keyword(name: &str) -> bool {
    // Built-in directives (section headers, scriptlets, conditionals,
    // …). Comparison is case-sensitive — RPM directive names are.
    if DIRECTIVES.iter().any(|(k, _)| *k == name) {
        return true;
    }
    // `%with`, `%without`, `%dnl`, `%lua` are macro forms used as
    // arguments rather than user-renameable identifiers.
    matches!(
        name,
        "with" | "without" | "dnl" | "lua" | "expand" | "shrink" | "quote" | "S" | "P" | "F"
    )
}

// ---------------------------------------------------------------------------
// Position ↔ byte
// ---------------------------------------------------------------------------

/// Convert an LSP `Position` to a byte offset in `source`. UTF-8
/// encoding uses byte columns directly; UTF-16 walks codepoints to
/// honour the negotiated client encoding.
pub(crate) fn position_to_byte(
    source: &str,
    pos: Position,
    enc: PositionEncoding,
) -> Option<usize> {
    let mut line_start = 0usize;
    for _ in 0..pos.line {
        let nl = source[line_start..].find('\n')?;
        line_start += nl + 1;
    }
    let line = &source[line_start..];
    let line_end = line
        .find('\n')
        .map(|n| line_start + n)
        .unwrap_or(source.len());
    let line_slice = &source[line_start..line_end];

    let col = pos.character as usize;
    let offset_in_line = match enc {
        PositionEncoding::Utf8 => col.min(line_slice.len()),
        PositionEncoding::Utf16 => {
            let mut consumed: usize = 0;
            let mut byte_off = 0;
            for c in line_slice.chars() {
                if consumed >= col {
                    break;
                }
                consumed += c.len_utf16();
                byte_off += c.len_utf8();
            }
            byte_off
        }
    };
    Some(line_start + offset_in_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(source: &str) -> Vec<(&str, OccKind)> {
        scan(source)
            .into_iter()
            .map(|o| (&source[o.name], o.kind))
            .collect()
    }

    #[test]
    fn scan_finds_define_and_refs() {
        let src = "%define foo 1\n%build\necho %foo %{foo} %{?foo}\n";
        let found = names(src);
        // Expected: foo(def), foo(ref), foo(ref), foo(ref).
        // `%build` is a keyword and must NOT appear.
        let foos: Vec<_> = found
            .iter()
            .filter(|(n, _)| *n == "foo")
            .map(|(_, k)| *k)
            .collect();
        assert_eq!(
            foos,
            vec![
                OccKind::Definition,
                OccKind::Reference,
                OccKind::Reference,
                OccKind::Reference,
            ]
        );
        assert!(!found.iter().any(|(n, _)| *n == "build"));
    }

    #[test]
    fn scan_skips_double_percent() {
        let src = "echo 100%% done %foo\n";
        let found = names(src);
        assert_eq!(found, vec![("foo", OccKind::Reference)]);
    }

    #[test]
    fn scan_handles_conditional_refs() {
        let src = "%{!?foo:bar} %{?baz}";
        let found = names(src);
        assert_eq!(
            found,
            vec![("foo", OccKind::Reference), ("baz", OccKind::Reference)],
        );
    }

    #[test]
    fn scan_skips_shell_command_substitution() {
        // `%(...)` is a shell call; macro refs inside still resolve at
        // build time, but for renaming we conservatively skip the body
        // — capturing them would risk false matches in arbitrary shell.
        let src = "%define foo %(echo $bar)\n%foo\n";
        let found = names(src);
        assert_eq!(
            found,
            vec![("foo", OccKind::Definition), ("foo", OccKind::Reference)],
        );
    }

    #[test]
    fn prepare_returns_range_at_cursor() {
        let src = "%define foo 1\n%foo\n";
        let idx = LineIndex::new(src);
        // Cursor inside `foo` on line 1 (the reference).
        let r = prepare(src, Position::new(1, 2), &idx, PositionEncoding::Utf16);
        let r = r.expect("expected rename anchor");
        assert_eq!(r.start.line, 1);
        assert_eq!(r.start.character, 1); // after `%`
        assert_eq!(r.end.character, 4);
    }

    #[test]
    fn prepare_returns_none_for_keyword() {
        let src = "%build\nmake\n";
        let idx = LineIndex::new(src);
        let r = prepare(src, Position::new(0, 2), &idx, PositionEncoding::Utf16);
        assert!(r.is_none());
    }

    #[test]
    fn rename_replaces_all_occurrences() {
        let src = "%define foo 1\n%build\necho %foo %{foo}\n";
        let idx = LineIndex::new(src);
        let uri: Uri = "file:///x.spec".parse().unwrap();
        let params = TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: Position::new(0, 9), // cursor in `foo` of the define
        };
        let edit = rename(&uri, src, &idx, PositionEncoding::Utf16, &params, "bar")
            .expect("workspace edit");
        let edits = edit.changes.unwrap().remove(&uri).unwrap();
        assert_eq!(edits.len(), 3, "edits: {edits:?}");
        for e in &edits {
            assert_eq!(e.new_text, "bar");
        }
    }

    #[test]
    fn rename_rejects_invalid_new_name() {
        let src = "%define foo 1\n%foo\n";
        let idx = LineIndex::new(src);
        let uri: Uri = "file:///x.spec".parse().unwrap();
        let params = TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: Position::new(0, 9),
        };
        assert!(
            rename(&uri, src, &idx, PositionEncoding::Utf16, &params, "1bad").is_none(),
            "leading digit must be rejected",
        );
        assert!(
            rename(
                &uri,
                src,
                &idx,
                PositionEncoding::Utf16,
                &params,
                "with space"
            )
            .is_none(),
            "whitespace must be rejected",
        );
        assert!(
            rename(&uri, src, &idx, PositionEncoding::Utf16, &params, "").is_none(),
            "empty must be rejected",
        );
    }

    #[test]
    fn rename_unrelated_macro_left_alone() {
        let src = "%define foo 1\n%define bar 2\n%foo %bar\n";
        let idx = LineIndex::new(src);
        let uri: Uri = "file:///x.spec".parse().unwrap();
        let params = TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier { uri: uri.clone() },
            position: Position::new(2, 2), // %foo
        };
        let edit = rename(&uri, src, &idx, PositionEncoding::Utf16, &params, "qux").expect("edit");
        let edits = edit.changes.unwrap().remove(&uri).unwrap();
        // Only the two `foo` sites; `bar` is untouched.
        assert_eq!(edits.len(), 2);
        for e in &edits {
            assert_eq!(e.new_text, "qux");
        }
    }
}
