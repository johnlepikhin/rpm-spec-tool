//! Error types for the apt-rpm parser family.
//!
//! Kept narrow on purpose: each variant carries the byte offset
//! (or, where applicable, the index-entry position) of the failure
//! so a corrupt `pkglist.classic.xz` fixture can be diagnosed
//! without re-running with `RUST_LOG=trace`. The top-level
//! [`RepoError`](rpm_spec_repo_core::RepoError) wraps these via
//! `From` when surfacing to callers.

use std::fmt;

use rpm_spec_repo_core::RepoError;

/// Failure modes of [`super::header::parse_one`] / `parse_chain`,
/// and the satellite parsers (`release`, `contents_index`,
/// `srclist`). Every variant carries enough context to point a
/// human at the broken byte in a hex dump.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AptRpmParseError {
    /// Buffer ended mid-header. `at` is the byte offset where the
    /// missing tail would have started (relative to the start of
    /// the buffer passed to `parse_chain`).
    TruncatedHeader { at: usize },
    /// Header intro didn't start with `8e ad e8 01`.
    BadMagic { at: usize, found: [u8; 4] },
    /// An index entry's `offset` field points past the end of the
    /// header's data store. `index_entry_at` is the byte offset of
    /// the index entry itself (so a hex viewer goes straight there);
    /// the other fields disambiguate which entry value field was
    /// out-of-bounds.
    OffsetOutOfBounds {
        index_entry_at: usize,
        offset: usize,
        wanted: usize,
        store_len: usize,
    },
    /// A NUL-terminated string entry was found without a NUL byte
    /// before the end of the data store. Usually means the data
    /// store length was wrong or the entry pointed at the wrong
    /// place.
    UnterminatedString {
        index_entry_at: usize,
        offset: usize,
    },
    /// `tag_type` outside the documented 1..=9 range. Could indicate
    /// a future rpm extension, a corrupt header, or an attempt to
    /// parse a non-header binary as if it were one.
    UnknownType { index_entry_at: usize, tag_type: u32 },
    /// `base/release` had no recognisable key=value line, or
    /// required fields were missing.
    BadReleaseFile { detail: String },
    /// A line in `contents_index` didn't split into a path and an
    /// owner. Carries the line number for triage.
    BadContentsIndexLine { line: usize, detail: String },
}

impl AptRpmParseError {
    /// Shift the `at` offset by `base`. Used by `parse_chain` to
    /// report failures in absolute pkglist coordinates rather than
    /// per-header relative ones — saves the operator from doing
    /// the arithmetic themselves.
    #[must_use]
    pub(crate) fn with_base(mut self, base: usize) -> Self {
        match &mut self {
            Self::TruncatedHeader { at } => *at += base,
            Self::BadMagic { at, .. } => *at += base,
            Self::OffsetOutOfBounds { index_entry_at, .. }
            | Self::UnterminatedString { index_entry_at, .. }
            | Self::UnknownType { index_entry_at, .. } => *index_entry_at += base,
            Self::BadReleaseFile { .. } | Self::BadContentsIndexLine { .. } => {}
        }
        self
    }
}

impl fmt::Display for AptRpmParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedHeader { at } => {
                write!(f, "truncated rpm header at byte offset {at}")
            }
            Self::BadMagic { at, found } => write!(
                f,
                "bad header magic at byte offset {at}: expected 8eade801, got {:02x}{:02x}{:02x}{:02x}",
                found[0], found[1], found[2], found[3]
            ),
            Self::OffsetOutOfBounds {
                index_entry_at,
                offset,
                wanted,
                store_len,
            } => write!(
                f,
                "index entry at byte {index_entry_at} points to offset {offset} \
                 (wanting {wanted} bytes) but data store is only {store_len} bytes"
            ),
            Self::UnterminatedString { index_entry_at, offset } => write!(
                f,
                "index entry at byte {index_entry_at} references unterminated string \
                 starting at data offset {offset}"
            ),
            Self::UnknownType { index_entry_at, tag_type } => write!(
                f,
                "index entry at byte {index_entry_at} has unknown tag type {tag_type} \
                 (expected 1..=9)"
            ),
            Self::BadReleaseFile { detail } => write!(f, "malformed `base/release`: {detail}"),
            Self::BadContentsIndexLine { line, detail } => {
                write!(f, "contents_index line {line}: {detail}")
            }
        }
    }
}

impl std::error::Error for AptRpmParseError {}

impl From<AptRpmParseError> for RepoError {
    fn from(e: AptRpmParseError) -> Self {
        Self::parse_msg(e.to_string())
    }
}
