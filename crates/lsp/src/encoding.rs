//! Convert byte offsets emitted by `rpm-spec-analyzer` into the
//! `(line, character)` positions LSP clients expect.
//!
//! LSP 3.17 default position encoding is `utf-16` — `character` counts
//! UTF-16 code units from the start of the line. We support `utf-8`
//! (`character` is the byte distance) too, because Neovim and some
//! newer clients negotiate it via the `positionEncoding` capability.
//! UTF-32 is intentionally not implemented; we'll fall back to UTF-16
//! when a client offers only UTF-32 (extremely rare).
//!
//! Spec files are typically ASCII; the conversion has a fast path that
//! skips the codepoint walk when the line contains no multibyte bytes.
//!
//! Reuses the line-index logic from
//! [`rpm_spec_analyzer::session`] in spirit (a `Vec<usize>` of line
//! starts) but materializes it locally so the LSP doesn't depend on
//! analyzer-private helpers.

use lsp_types::{Position, Range};

/// Position encoding negotiated with the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    /// `character` is the UTF-8 byte distance from the line start.
    /// Faster, but only supported by clients that advertise it.
    Utf8,
    /// `character` is the UTF-16 code-unit distance — LSP default.
    Utf16,
}

/// Pre-computed line index for a source text. Indexed by 0-based line
/// number; `starts[n]` is the byte offset of the first char of line n.
///
/// A trailing newline produces a final empty line entry. A source
/// without a trailing newline ends at the last non-empty line.
#[derive(Debug, Clone)]
pub struct LineIndex {
    starts: Vec<usize>,
    len: usize,
}

impl LineIndex {
    pub fn new(source: &str) -> Self {
        // Pre-size so the typical "one line ≈ 40 bytes" spec doesn't
        // reallocate. Off by a few is fine.
        let mut starts = Vec::with_capacity(source.len() / 40 + 1);
        starts.push(0);
        for (i, b) in source.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        Self {
            starts,
            len: source.len(),
        }
    }

    /// Convert a byte offset into a 0-based `(line, line_start_byte)`
    /// pair. Clamps to the source length.
    fn line_for_byte(&self, byte: usize) -> (u32, usize) {
        let clamped = byte.min(self.len);
        // `partition_point` finds the count of starts ≤ clamped, which
        // is the 1-based line number; subtract 1 for 0-based LSP.
        let one_based = self.starts.partition_point(|&s| s <= clamped);
        let line0 = one_based.saturating_sub(1);
        let line_start = self.starts[line0];
        (line0 as u32, line_start)
    }

    /// Convert a byte offset into an LSP [`Position`].
    pub fn position(&self, source: &str, byte: usize, enc: PositionEncoding) -> Position {
        let (line, line_start) = self.line_for_byte(byte);
        let clamped = byte.min(self.len);
        let line_slice = &source[line_start..clamped];
        let character = match enc {
            PositionEncoding::Utf8 => line_slice.len() as u32,
            PositionEncoding::Utf16 => utf16_len(line_slice),
        };
        Position { line, character }
    }

    /// Convert a byte range into an LSP [`Range`].
    pub fn range(
        &self,
        source: &str,
        start_byte: usize,
        end_byte: usize,
        enc: PositionEncoding,
    ) -> Range {
        Range {
            start: self.position(source, start_byte, enc),
            end: self.position(source, end_byte, enc),
        }
    }
}

/// Count the UTF-16 code units required to encode `s`. ASCII fast
/// path: each byte is one code unit, so we only do the codepoint walk
/// when a non-ASCII byte appears.
fn utf16_len(s: &str) -> u32 {
    if s.is_ascii() {
        return s.len() as u32;
    }
    let mut n: u32 = 0;
    for c in s.chars() {
        n += c.len_utf16() as u32;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_positions_round_trip() {
        let src = "abc\ndef\nghi";
        let idx = LineIndex::new(src);
        // First byte of line 0.
        let p = idx.position(src, 0, PositionEncoding::Utf16);
        assert_eq!(p, Position::new(0, 0));
        // 'd' starts line 1.
        let p = idx.position(src, 4, PositionEncoding::Utf16);
        assert_eq!(p, Position::new(1, 0));
        // Middle of line 2.
        let p = idx.position(src, 9, PositionEncoding::Utf16);
        assert_eq!(p, Position::new(2, 1));
        // Past end clamps.
        let p = idx.position(src, 9999, PositionEncoding::Utf16);
        assert_eq!(p, Position::new(2, 3));
    }

    #[test]
    fn utf8_encoding_matches_byte_offsets() {
        // Russian: each char is 2 bytes UTF-8.
        let src = "Привет\nworld";
        let idx = LineIndex::new(src);
        // Byte 12 is the '\n' position; UTF-8 column = byte distance from line start = 12.
        let p = idx.position(src, 12, PositionEncoding::Utf8);
        assert_eq!(p, Position::new(0, 12));
    }

    #[test]
    fn utf16_counts_code_units_not_bytes() {
        // Russian "Привет" = 6 chars, 12 bytes UTF-8, but 6 UTF-16 code units.
        let src = "Привет\nworld";
        let idx = LineIndex::new(src);
        let p = idx.position(src, 12, PositionEncoding::Utf16);
        assert_eq!(p, Position::new(0, 6));
    }

    #[test]
    fn utf16_surrogate_pair_takes_two_units() {
        // 😀 (U+1F600) is a single char, 4 bytes UTF-8, 2 UTF-16 code units.
        let src = "a😀b";
        let idx = LineIndex::new(src);
        // Byte after the emoji: position should be 1 + 2 = 3 UTF-16 units.
        let p = idx.position(src, 5, PositionEncoding::Utf16);
        assert_eq!(p, Position::new(0, 3));
    }

    #[test]
    fn range_spans_two_lines() {
        let src = "hello\nworld";
        let idx = LineIndex::new(src);
        let r = idx.range(src, 0, 11, PositionEncoding::Utf16);
        assert_eq!(r.start, Position::new(0, 0));
        assert_eq!(r.end, Position::new(1, 5));
    }

    #[test]
    fn empty_source() {
        let src = "";
        let idx = LineIndex::new(src);
        let p = idx.position(src, 0, PositionEncoding::Utf16);
        assert_eq!(p, Position::new(0, 0));
    }
}
