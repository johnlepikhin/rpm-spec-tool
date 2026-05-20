//! Binary RPM header-v3 parser, specialised for apt-rpm pkglist /
//! srclist files.
//!
//! `base/pkglist.classic` (post-xz-decompression) is a concatenation
//! of "bare" rpm headers — no lead, no signature header, no payload.
//! Each header starts with an 8-byte intro magic (`0x8e 0xad 0xe8 0x01`
//! plus 4 reserved bytes), then 4 BE bytes index count, 4 BE bytes
//! data length, then `count × 16` bytes of index entries pointing
//! into the `data_length` bytes of data store that follows. The next
//! header starts immediately after.
//!
//! This module owns the binary decoding only; the mapping from
//! generic `Header` (tag → value) to a [`Package`] lives in
//! [`super::package`] so the parser doesn't need to know which tags
//! we care about (and so tests can exercise the wire format without
//! pulling in the whole catalogue).
//!
//! All multi-byte values on the wire are big-endian. Strings are
//! NUL-terminated. The byte cursor is owned by the parser; callers
//! get back fully-materialised [`HeaderEntry`] values (no zero-copy
//! into the original buffer) so the buffer can be dropped after
//! parsing without keeping the headers' lifetime alive.

use crate::aptrpm::error::AptRpmParseError;

/// Magic bytes opening every rpm header v3 record. Stored big-endian:
/// `8e ad e8 01`.
pub(super) const HEADER_MAGIC: [u8; 4] = [0x8e, 0xad, 0xe8, 0x01];

/// Size of one [`IndexEntry`] on the wire (4 × u32 BE).
const INDEX_ENTRY_LEN: usize = 16;

/// Size of the intro block: magic(4) + reserved(4) + index_count(4) +
/// data_length(4).
const HEADER_INTRO_LEN: usize = 16;

/// One header parsed from the chain. Keyed by RPM tag number (the
/// well-known `RPMTAG_NAME=1000` etc. constants the consumer module
/// looks up). Multi-valued entries (e.g. `REQUIRENAME` for a package
/// with N runtime deps) live in the appropriate `Array` variants of
/// [`HeaderEntry`].
#[derive(Debug, Clone, Default)]
pub struct Header {
    pub entries: Vec<(u32, HeaderEntry)>,
}

impl Header {
    /// Find the first entry for a given tag. RPM headers never
    /// contain a tag twice (the format enforces unique tags per
    /// header), so "first" is also "only".
    #[must_use]
    pub fn get(&self, tag: u32) -> Option<&HeaderEntry> {
        self.entries.iter().find(|(t, _)| *t == tag).map(|(_, v)| v)
    }
}

/// One typed value pulled out of the data store. The wire format
/// uses a numeric type discriminator (1..9); we lift it into a Rust
/// enum at parse time so downstream code doesn't have to interpret
/// raw bytes.
///
/// Locale-tagged i18n strings ([`HeaderEntry::I18nString`]) merge
/// with regular [`HeaderEntry::String`] on the consumer side because
/// apt-rpm only ever writes a single "C" locale and the distinction
/// would be noise downstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderEntry {
    /// `RPM_CHAR_TYPE` (1) — single byte.
    Char(u8),
    /// `RPM_INT8_TYPE` (2) — single byte interpreted as i8/u8.
    Int8(u8),
    /// `RPM_INT16_TYPE` (3) — 16-bit big-endian.
    Int16(Vec<u16>),
    /// `RPM_INT32_TYPE` (4) — 32-bit big-endian. Array form because
    /// many tags (`REQUIREFLAGS`, etc.) hold one flag per dep.
    Int32(Vec<u32>),
    /// `RPM_INT64_TYPE` (5) — 64-bit big-endian.
    Int64(Vec<u64>),
    /// `RPM_STRING_TYPE` (6) — single NUL-terminated string.
    /// Always count=1 on the wire.
    String(String),
    /// `RPM_BIN_TYPE` (7) — opaque byte blob (count = blob length).
    Bin(Vec<u8>),
    /// `RPM_STRING_ARRAY_TYPE` (8) — `count` NUL-terminated strings
    /// laid out back-to-back in the data store.
    StringArray(Vec<String>),
    /// `RPM_I18N_STRING_TYPE` (9) — same wire layout as
    /// `StringArray` but the strings are locale-tagged variants of
    /// one logical message. We treat them as a string array; the
    /// consumer typically takes element [0] (the "C" locale).
    I18nString(Vec<String>),
}

/// Decode a single header from `bytes`, returning the parsed
/// [`Header`] plus the number of bytes consumed. Callers chain calls
/// in a `while remaining.is_empty() == false` loop to walk the whole
/// pkglist.
///
/// # Errors
///
/// Returns [`AptRpmParseError::TruncatedHeader`] when the buffer
/// ends mid-header, [`AptRpmParseError::BadMagic`] when the intro
/// magic doesn't match, [`AptRpmParseError::OffsetOutOfBounds`]
/// when an index entry points past the data store, and
/// [`AptRpmParseError::UnknownType`] for any wire-type discriminator
/// outside the documented 1..=9 range. All variants carry the byte
/// offset of the failure so corrupt fixtures are diagnosable.
pub fn parse_one(bytes: &[u8]) -> Result<(Header, usize), AptRpmParseError> {
    if bytes.len() < HEADER_INTRO_LEN {
        return Err(AptRpmParseError::TruncatedHeader { at: 0 });
    }
    if bytes[..4] != HEADER_MAGIC {
        return Err(AptRpmParseError::BadMagic {
            at: 0,
            found: [bytes[0], bytes[1], bytes[2], bytes[3]],
        });
    }
    // reserved bytes 4..8 are documented as version+padding; we
    // don't validate them — every header in the wild has them zeroed.
    let index_count = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    let data_length = u32::from_be_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;

    let index_start = HEADER_INTRO_LEN;
    let data_start = index_start + index_count * INDEX_ENTRY_LEN;
    let header_end = data_start + data_length;
    if bytes.len() < header_end {
        return Err(AptRpmParseError::TruncatedHeader { at: header_end });
    }
    let data = &bytes[data_start..header_end];

    let mut entries: Vec<(u32, HeaderEntry)> = Vec::with_capacity(index_count);
    for i in 0..index_count {
        let off = index_start + i * INDEX_ENTRY_LEN;
        let tag = u32::from_be_bytes([bytes[off], bytes[off + 1], bytes[off + 2], bytes[off + 3]]);
        let tag_type =
            u32::from_be_bytes([bytes[off + 4], bytes[off + 5], bytes[off + 6], bytes[off + 7]]);
        let data_offset =
            u32::from_be_bytes([bytes[off + 8], bytes[off + 9], bytes[off + 10], bytes[off + 11]])
                as usize;
        let count = u32::from_be_bytes([
            bytes[off + 12],
            bytes[off + 13],
            bytes[off + 14],
            bytes[off + 15],
        ]) as usize;
        let value = decode_entry(data, data_offset, tag_type, count, off)?;
        entries.push((tag, value));
    }
    Ok((Header { entries }, header_end))
}

fn decode_entry(
    data: &[u8],
    offset: usize,
    tag_type: u32,
    count: usize,
    index_entry_pos: usize,
) -> Result<HeaderEntry, AptRpmParseError> {
    // Helper: bounds-check then slice. Returns the OoB error variant
    // pre-filled with the offending index-entry position so the
    // caller doesn't have to thread that through.
    let need = |size: usize| -> Result<&[u8], AptRpmParseError> {
        let end = offset.saturating_add(size);
        if end > data.len() {
            return Err(AptRpmParseError::OffsetOutOfBounds {
                index_entry_at: index_entry_pos,
                offset,
                wanted: size,
                store_len: data.len(),
            });
        }
        Ok(&data[offset..end])
    };
    match tag_type {
        1 => {
            // CHAR. Count >= 1 expected; we model as single byte
            // (the consumer is rare and only ever single).
            let s = need(1)?;
            Ok(HeaderEntry::Char(s[0]))
        }
        2 => {
            let s = need(1)?;
            Ok(HeaderEntry::Int8(s[0]))
        }
        3 => {
            let s = need(2 * count)?;
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let b = &s[i * 2..i * 2 + 2];
                out.push(u16::from_be_bytes([b[0], b[1]]));
            }
            Ok(HeaderEntry::Int16(out))
        }
        4 => {
            let s = need(4 * count)?;
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let b = &s[i * 4..i * 4 + 4];
                out.push(u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
            }
            Ok(HeaderEntry::Int32(out))
        }
        5 => {
            let s = need(8 * count)?;
            let mut out = Vec::with_capacity(count);
            for i in 0..count {
                let b = &s[i * 8..i * 8 + 8];
                out.push(u64::from_be_bytes([
                    b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                ]));
            }
            Ok(HeaderEntry::Int64(out))
        }
        6 => {
            // STRING — count is always 1; data is NUL-terminated.
            // Walk until NUL to know the length.
            let s = read_cstring(data, offset, index_entry_pos)?;
            Ok(HeaderEntry::String(s))
        }
        7 => {
            // BIN — `count` is the blob length.
            let s = need(count)?;
            Ok(HeaderEntry::Bin(s.to_vec()))
        }
        8 | 9 => {
            // STRING_ARRAY / I18N_STRING — `count` NUL-terminated
            // strings concatenated. We walk one at a time, advancing
            // the cursor past each NUL.
            let mut strings = Vec::with_capacity(count);
            let mut cursor = offset;
            for _ in 0..count {
                let s = read_cstring(data, cursor, index_entry_pos)?;
                cursor += s.len() + 1; // +1 for the NUL we consumed
                strings.push(s);
            }
            Ok(if tag_type == 8 {
                HeaderEntry::StringArray(strings)
            } else {
                HeaderEntry::I18nString(strings)
            })
        }
        other => Err(AptRpmParseError::UnknownType {
            index_entry_at: index_entry_pos,
            tag_type: other,
        }),
    }
}

/// Read a NUL-terminated string starting at `offset` in `data`.
/// Lossy UTF-8 (rpm headers are nominally ASCII but real-world specs
/// occasionally have non-ASCII summaries; we don't want one bad spec
/// to abort the whole repo parse).
fn read_cstring(
    data: &[u8],
    offset: usize,
    index_entry_pos: usize,
) -> Result<String, AptRpmParseError> {
    if offset > data.len() {
        return Err(AptRpmParseError::OffsetOutOfBounds {
            index_entry_at: index_entry_pos,
            offset,
            wanted: 0,
            store_len: data.len(),
        });
    }
    let tail = &data[offset..];
    let nul_pos = tail
        .iter()
        .position(|&b| b == 0)
        .ok_or(AptRpmParseError::UnterminatedString {
            index_entry_at: index_entry_pos,
            offset,
        })?;
    Ok(String::from_utf8_lossy(&tail[..nul_pos]).into_owned())
}

/// Iterate every header in `bytes`, yielding the parsed [`Header`]
/// plus the consumed byte range. Stops cleanly at the end of the
/// buffer; surfaces parse failures by returning the error.
pub fn parse_chain(bytes: &[u8]) -> Result<Vec<Header>, AptRpmParseError> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let (header, consumed) = parse_one(&bytes[cursor..]).map_err(|e| e.with_base(cursor))?;
        out.push(header);
        cursor += consumed;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hand-craft a minimal one-entry header: NAME (tag 1000, type 6
    /// STRING) = "foo". Verify roundtrip.
    fn minimal_header() -> Vec<u8> {
        let mut buf = Vec::new();
        // Intro: magic + reserved + index_count=1 + data_length=4
        // ("foo\0").
        buf.extend_from_slice(&HEADER_MAGIC);
        buf.extend_from_slice(&[0, 0, 0, 0]); // reserved
        buf.extend_from_slice(&1_u32.to_be_bytes());
        buf.extend_from_slice(&4_u32.to_be_bytes());
        // Index entry: tag=1000, type=6 (STRING), offset=0, count=1.
        buf.extend_from_slice(&1000_u32.to_be_bytes());
        buf.extend_from_slice(&6_u32.to_be_bytes());
        buf.extend_from_slice(&0_u32.to_be_bytes());
        buf.extend_from_slice(&1_u32.to_be_bytes());
        // Data store: "foo\0".
        buf.extend_from_slice(b"foo\0");
        buf
    }

    #[test]
    fn parses_single_string_entry() {
        let buf = minimal_header();
        let (h, consumed) = parse_one(&buf).unwrap();
        assert_eq!(consumed, buf.len());
        assert_eq!(h.entries.len(), 1);
        let (tag, value) = &h.entries[0];
        assert_eq!(*tag, 1000);
        assert!(matches!(value, HeaderEntry::String(s) if s == "foo"));
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = minimal_header();
        buf[0] = 0xff;
        let err = parse_one(&buf).unwrap_err();
        assert!(matches!(err, AptRpmParseError::BadMagic { .. }));
    }

    #[test]
    fn rejects_truncated_intro() {
        let buf = vec![0u8; 8];
        assert!(matches!(
            parse_one(&buf),
            Err(AptRpmParseError::TruncatedHeader { .. })
        ));
    }

    #[test]
    fn rejects_truncated_data_store() {
        let mut buf = minimal_header();
        buf.truncate(buf.len() - 2); // chop off last 2 bytes of "foo\0"
        assert!(matches!(
            parse_one(&buf),
            Err(AptRpmParseError::TruncatedHeader { .. })
        ));
    }

    #[test]
    fn rejects_offset_past_store() {
        // Build a header whose index entry points past the data store.
        let mut buf = Vec::new();
        buf.extend_from_slice(&HEADER_MAGIC);
        buf.extend_from_slice(&[0, 0, 0, 0]);
        buf.extend_from_slice(&1_u32.to_be_bytes()); // index_count=1
        buf.extend_from_slice(&4_u32.to_be_bytes()); // data_length=4
        // Index entry: tag=1000, type=6, offset=99 (way past data
        // store), count=1.
        buf.extend_from_slice(&1000_u32.to_be_bytes());
        buf.extend_from_slice(&6_u32.to_be_bytes());
        buf.extend_from_slice(&99_u32.to_be_bytes());
        buf.extend_from_slice(&1_u32.to_be_bytes());
        // Data store padding.
        buf.extend_from_slice(b"foo\0");
        assert!(matches!(
            parse_one(&buf),
            Err(AptRpmParseError::OffsetOutOfBounds { .. })
        ));
    }

    #[test]
    fn parses_chain_of_two() {
        let mut chain = minimal_header();
        chain.extend(minimal_header());
        let parsed = parse_chain(&chain).unwrap();
        assert_eq!(parsed.len(), 2);
        for h in &parsed {
            let (tag, val) = &h.entries[0];
            assert_eq!(*tag, 1000);
            assert!(matches!(val, HeaderEntry::String(s) if s == "foo"));
        }
    }

    #[test]
    fn parses_int32_array() {
        // Header with one entry: tag=1009 (SIZE), type=4 INT32,
        // count=3 → 12 bytes of u32 BE.
        let mut buf = Vec::new();
        buf.extend_from_slice(&HEADER_MAGIC);
        buf.extend_from_slice(&[0, 0, 0, 0]);
        buf.extend_from_slice(&1_u32.to_be_bytes());
        buf.extend_from_slice(&12_u32.to_be_bytes());
        buf.extend_from_slice(&1009_u32.to_be_bytes());
        buf.extend_from_slice(&4_u32.to_be_bytes());
        buf.extend_from_slice(&0_u32.to_be_bytes());
        buf.extend_from_slice(&3_u32.to_be_bytes());
        for v in [10_u32, 20, 30] {
            buf.extend_from_slice(&v.to_be_bytes());
        }
        let (h, consumed) = parse_one(&buf).unwrap();
        assert_eq!(consumed, buf.len());
        assert!(matches!(&h.entries[0].1, HeaderEntry::Int32(v) if v == &vec![10, 20, 30]));
    }

    #[test]
    fn parses_string_array() {
        // Two strings: "foo", "bar".
        let mut buf = Vec::new();
        buf.extend_from_slice(&HEADER_MAGIC);
        buf.extend_from_slice(&[0, 0, 0, 0]);
        buf.extend_from_slice(&1_u32.to_be_bytes());
        buf.extend_from_slice(&8_u32.to_be_bytes()); // "foo\0bar\0" = 8 bytes
        buf.extend_from_slice(&1047_u32.to_be_bytes()); // PROVIDENAME
        buf.extend_from_slice(&8_u32.to_be_bytes()); // STRING_ARRAY
        buf.extend_from_slice(&0_u32.to_be_bytes());
        buf.extend_from_slice(&2_u32.to_be_bytes());
        buf.extend_from_slice(b"foo\0bar\0");
        let (h, _) = parse_one(&buf).unwrap();
        let HeaderEntry::StringArray(s) = &h.entries[0].1 else {
            panic!("expected StringArray");
        };
        assert_eq!(s, &vec!["foo".to_string(), "bar".to_string()]);
    }
}
