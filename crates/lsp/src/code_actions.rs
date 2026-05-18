//! Convert analyzer suggestions into LSP code actions (quick fixes).
//!
//! A `textDocument/codeAction` request comes in with a range; we look
//! up every diagnostic whose primary span intersects that range and
//! turn each of its [`Suggestion`]s into a `CodeAction`.
//!
//! [`Suggestion`]: rpm_spec_analyzer::Suggestion

use std::collections::HashMap;

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, Diagnostic as LspDiagnostic, Range, TextEdit,
    Uri, WorkspaceEdit,
};
use rpm_spec_analyzer::{Applicability, Diagnostic as AnalyzerDiagnostic};

use crate::diagnostics::span_to_range;
use crate::encoding::{LineIndex, PositionEncoding};

/// Build the list of code actions that apply to `requested_range`.
///
/// `diagnostics` is the cached analyzer output for the document — the
/// LSP server stores it after every analysis pass and reuses it here.
/// The matching LSP diagnostic (so the client can attribute the fix
/// back to a marker) is looked up by lint id.
pub fn collect(
    uri: &Uri,
    source: &str,
    index: &LineIndex,
    enc: PositionEncoding,
    diagnostics: &[AnalyzerDiagnostic],
    lsp_by_lint: &HashMap<String, LspDiagnostic>,
    requested_range: Range,
) -> Vec<CodeActionOrCommand> {
    let mut out = Vec::new();
    for diag in diagnostics {
        if diag.suggestions.is_empty() {
            continue;
        }
        let diag_range = span_to_range(&diag.primary_span, source, index, enc);
        if !ranges_intersect(diag_range, requested_range) {
            continue;
        }
        let matched_lsp = lsp_by_lint.get(diag.lint_id).cloned();
        for suggestion in &diag.suggestions {
            // Manual-only suggestions are informational; they shouldn't
            // be applied automatically. Drop them from the LSP surface.
            if matches!(suggestion.applicability, Applicability::Manual) {
                continue;
            }
            let edits: Vec<TextEdit> = suggestion
                .edits
                .iter()
                .map(|e| TextEdit {
                    range: span_to_range(&e.span, source, index, enc),
                    new_text: e.replacement.clone(),
                })
                .collect();
            if edits.is_empty() {
                continue;
            }
            let mut changes = HashMap::new();
            changes.insert(uri.clone(), edits);
            let action = CodeAction {
                title: suggestion.message.clone(),
                kind: Some(CodeActionKind::QUICKFIX),
                diagnostics: matched_lsp.clone().map(|d| vec![d]),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    document_changes: None,
                    change_annotations: None,
                }),
                command: None,
                is_preferred: Some(matches!(
                    suggestion.applicability,
                    Applicability::MachineApplicable
                )),
                disabled: None,
                data: None,
            };
            out.push(CodeActionOrCommand::CodeAction(action));
        }
    }
    out
}

fn ranges_intersect(a: Range, b: Range) -> bool {
    // Empty intersection iff one range ends strictly before the other
    // starts. `Position` is lexicographic (line, character).
    !(position_lt(a.end, b.start) || position_lt(b.end, a.start))
}

fn position_lt(a: lsp_types::Position, b: lsp_types::Position) -> bool {
    (a.line, a.character) < (b.line, b.character)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::Position;

    fn r(sl: u32, sc: u32, el: u32, ec: u32) -> Range {
        Range {
            start: Position::new(sl, sc),
            end: Position::new(el, ec),
        }
    }

    #[test]
    fn intersect_disjoint() {
        assert!(!ranges_intersect(r(0, 0, 0, 5), r(1, 0, 1, 5)));
    }

    #[test]
    fn intersect_overlap() {
        assert!(ranges_intersect(r(0, 0, 1, 5), r(1, 0, 2, 0)));
    }

    #[test]
    fn intersect_touch_at_point() {
        // Touching ranges count as intersecting — clients usually
        // surface fixes when the cursor is at the boundary too.
        assert!(ranges_intersect(r(0, 0, 0, 5), r(0, 5, 0, 10)));
    }
}
