//! `textDocument/inlayHint` — render the expanded value of a macro
//! reference as ghost text next to the `%{…}` token.
//!
//! Source of truth is `rpm_spec_profile::types::MacroRegistry`, which
//! already implements bounded macro expansion
//! (`expand_to_literal(name, depth)`). The server resolves the active
//! profile per `.rpmspec.toml` and caches it; this module just looks
//! up names produced by the same scanner that powers rename + xref.

use lsp_types::{InlayHint, InlayHintKind, InlayHintLabel, Position, Range};
use rpm_spec_profile::Profile;

use crate::encoding::{LineIndex, PositionEncoding};
use crate::rename::{OccKind, Occurrence, scan};

/// Maximum literal length we'll inline. Longer expansions are
/// truncated — packing them into ghost text just clutters the
/// editor.
const MAX_LITERAL_LEN: usize = 40;

/// Expansion recursion budget. Matches the depth `rpmlint`-style
/// tooling uses; deep enough to resolve `%{?dist}` chains, shallow
/// enough to avoid runaway recursion on pathological macros.
const EXPAND_DEPTH: u8 = 4;

/// Build the inlay hint list for the given byte range of `source`.
/// `byte_range` is `None` to mean "the whole document".
pub fn build(
    source: &str,
    index: &LineIndex,
    enc: PositionEncoding,
    profile: &Profile,
    byte_range: Option<(usize, usize)>,
) -> Vec<InlayHint> {
    let (lo, hi) = byte_range.unwrap_or((0, source.len()));
    scan(source)
        .into_iter()
        .filter(|o| o.kind == OccKind::Reference && o.name.start >= lo && o.name.end <= hi)
        .filter_map(|o| hint_for(&o, source, index, enc, profile))
        .collect()
}

fn hint_for(
    occ: &Occurrence,
    source: &str,
    index: &LineIndex,
    enc: PositionEncoding,
    profile: &Profile,
) -> Option<InlayHint> {
    let name = &source[occ.name.clone()];
    let literal = profile.macros.expand_to_literal(name, EXPAND_DEPTH)?;
    let literal = literal.trim();
    if literal.is_empty() {
        return None;
    }
    // Skip hints whose value equals the macro name itself — happens
    // when a name resolves to a placeholder like `%dist` → `%dist`.
    if literal == name || literal == format!("%{name}") {
        return None;
    }
    let display = truncate(literal, MAX_LITERAL_LEN);

    // Anchor the hint right after the closing `}` (for braced forms)
    // or the end of the identifier (for `%name`). The cheapest way is
    // to use the identifier end; the resulting hint sits inside the
    // braces, which is mildly noisier but doesn't require relocating
    // through the source. Acceptable for the first cut.
    let Range { end, .. } = index.range(source, occ.name.start, occ.name.end, enc);
    Some(InlayHint {
        position: end,
        label: InlayHintLabel::String(format!("= {display}")),
        kind: Some(InlayHintKind::TYPE),
        text_edits: None,
        tooltip: None,
        padding_left: Some(true),
        padding_right: Some(false),
        data: None,
    })
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Convert an LSP `Range` into a byte span over `source`. Used to
/// honour the `range` request scope: clients send the visible
/// viewport, we trim the scan accordingly. Returns `None` when the
/// range can't be resolved (out-of-bounds lines, etc.).
pub fn range_to_byte_span(
    source: &str,
    range: Range,
    index: &LineIndex,
    enc: PositionEncoding,
) -> Option<(usize, usize)> {
    let start = position_to_byte(source, range.start, index, enc)?;
    let end = position_to_byte(source, range.end, index, enc)?;
    Some((start, end))
}

fn position_to_byte(
    source: &str,
    pos: Position,
    _index: &LineIndex,
    enc: PositionEncoding,
) -> Option<usize> {
    // Re-implement minimal position→byte locally to avoid pulling
    // `rename::position_to_byte` into the public API. Same algorithm.
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
            let mut consumed = 0usize;
            let mut byte_off = 0usize;
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
    use rpm_spec_profile::types::{MacroEntry, Provenance};

    fn profile_with(entries: &[(&str, &str)]) -> Profile {
        let mut p = Profile::default();
        for (name, body) in entries {
            p.macros.insert(
                *name,
                MacroEntry::literal((*body).to_string(), Provenance::Override),
            );
        }
        p
    }

    #[test]
    fn expands_known_macro() {
        let src = "Release: 1%{?dist}\n";
        let idx = LineIndex::new(src);
        let prof = profile_with(&[("dist", ".fc40")]);
        let hints = build(src, &idx, PositionEncoding::Utf16, &prof, None);
        assert_eq!(hints.len(), 1, "got {hints:?}");
        match &hints[0].label {
            InlayHintLabel::String(s) => assert!(s.contains(".fc40"), "label: {s}"),
            other => panic!("unexpected label: {other:?}"),
        }
    }

    #[test]
    fn unknown_macro_yields_no_hint() {
        let src = "%{not_in_profile}\n";
        let idx = LineIndex::new(src);
        let prof = profile_with(&[]);
        let hints = build(src, &idx, PositionEncoding::Utf16, &prof, None);
        assert!(hints.is_empty());
    }

    #[test]
    fn definition_sites_do_not_get_hints() {
        // `%define foo bar` is a definition — the inlay would
        // duplicate text the user just typed. Skip it.
        let src = "%define foo bar\n";
        let idx = LineIndex::new(src);
        let prof = profile_with(&[("foo", "bar")]);
        let hints = build(src, &idx, PositionEncoding::Utf16, &prof, None);
        assert!(hints.is_empty(), "got {hints:?}");
    }

    #[test]
    fn long_value_truncated() {
        let src = "%{long}\n";
        let idx = LineIndex::new(src);
        let prof = profile_with(&[(
            "long",
            "this_is_a_very_long_macro_value_that_exceeds_the_inline_threshold",
        )]);
        let hints = build(src, &idx, PositionEncoding::Utf16, &prof, None);
        assert_eq!(hints.len(), 1);
        match &hints[0].label {
            InlayHintLabel::String(s) => {
                assert!(s.contains('…'), "expected ellipsis: {s}");
                assert!(s.chars().count() <= MAX_LITERAL_LEN + 4, "too long: {s}");
            }
            _ => panic!(),
        }
    }
}
