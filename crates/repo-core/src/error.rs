//! Error variants partitioned by phase. Consumers can `match` on
//! variant to render user-friendly messages or react programmatically
//! (e.g. retry on `Http`, fail on `Verify`).

use std::io;

use thiserror::Error;

/// Source-location carrier for parse / decompress / verify failures.
///
/// Enum-of-three rather than struct-of-options so the **three
/// reachable shapes** are the only constructable ones (the struct
/// form had five illegal states the type system happily admitted —
/// `file=None, line=Some` made no sense). `Display` renders as
/// `[file:]line:col: detail`, eliding the prefix when missing.
///
/// `#[non_exhaustive]` reserves room for future shapes (e.g. byte-
/// offset for binary formats, span ranges) without breaking pattern
/// matchers in downstream code.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorLocation {
    /// Free-form detail, no source location. Used by the legacy
    /// bare-string call sites and by errors whose origin can't be
    /// pinned to a file (e.g. trait dispatch failures).
    Detail {
        /// Free-form description of what went wrong.
        detail: String,
    },
    /// File context but no line/col. Used by stream parsers
    /// (decompression, GPG verify) and by failures that name the
    /// data file but lack a positional anchor.
    InFile {
        /// Logical file name (e.g. `primary.xml`, `base/release`,
        /// `pkglist.classic.xz`). Not a filesystem path — value is
        /// for diagnostics and grep-ability only.
        file: String,
        /// Free-form description of what went wrong.
        detail: String,
    },
    /// Full file:line:col position. Used when the parser has a
    /// source span available (quick-xml byte offset translated to
    /// line/col, future apt-rpm header byte-offset translation).
    AtPos {
        /// Logical file name. See [`Self::InFile::file`].
        file: String,
        /// 1-based line number.
        line: u32,
        /// 1-based column number.
        col: u32,
        /// Free-form description of what went wrong.
        detail: String,
    },
}

impl ErrorLocation {
    /// File-less, line-less carrier — the bare-string case all the
    /// pre-iteration callers used.
    #[must_use]
    pub fn from_detail(detail: impl Into<String>) -> Self {
        Self::Detail {
            detail: detail.into(),
        }
    }

    /// Carrier with file context but no line/col. Use for stream
    /// parsers (decompression, GPG verify) that know the filename
    /// but not a source position.
    #[must_use]
    pub fn at_file(file: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::InFile {
            file: file.into(),
            detail: detail.into(),
        }
    }

    /// Full carrier with file:line:col. Use when the parser has a
    /// source span available.
    #[must_use]
    pub fn at(file: impl Into<String>, line: u32, col: u32, detail: impl Into<String>) -> Self {
        Self::AtPos {
            file: file.into(),
            line,
            col,
            detail: detail.into(),
        }
    }

    /// Accessor: the description string, regardless of variant.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Detail { detail } => detail,
            Self::InFile { detail, .. } => detail,
            Self::AtPos { detail, .. } => detail,
        }
    }

    /// Accessor: the file name when present.
    #[must_use]
    pub fn file(&self) -> Option<&str> {
        match self {
            Self::Detail { .. } => None,
            Self::InFile { file, .. } => Some(file),
            Self::AtPos { file, .. } => Some(file),
        }
    }
}

impl std::fmt::Display for ErrorLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Detail { detail } => f.write_str(detail),
            Self::InFile { file, detail } => write!(f, "{file}: {detail}"),
            Self::AtPos {
                file,
                line,
                col,
                detail,
            } => write!(f, "{file}:{line}:{col}: {detail}"),
        }
    }
}

/// Top-level repository error. Wraps phase-specific errors so the CLI
/// can attribute a failure to fetch, decompress, parse, verify, solve,
/// cache I/O, or config processing without lossy string conversion.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum RepoError {
    #[error("HTTP fetch failed: {0}")]
    Http(#[from] HttpError),

    #[error("decompression failed: {0}")]
    Decompress(ErrorLocation),

    #[error("XML parse failed: {0}")]
    Parse(ErrorLocation),

    #[error("GPG verification failed: {0}")]
    Verify(ErrorLocation),

    #[error("solver: {0}")]
    Solve(#[from] SolveError),

    #[error("cache I/O: {0}")]
    Cache(#[from] io::Error),

    #[error("serialisation failed: {0}")]
    Serialize(String),

    #[error("config: {0}")]
    Config(String),

    #[error("offline mode: cache miss for {repo} (run `rpm-spec-tool repo sync --allow-fetch`)")]
    OfflineCacheMiss { repo: String },

    #[error("unsupported repo kind: {0}")]
    UnsupportedKind(String),

    /// Free-form semantic database error: schema-version mismatch,
    /// missing required `meta` rows, malformed payloads — anything
    /// that isn't a raw rusqlite failure. The new [`Self::Sqlite`]
    /// variant carries the underlying rusqlite error directly so
    /// `anyhow`'s `.source()` chain stays intact.
    #[error("repo database: {0}")]
    Database(String),

    #[error("SQLite error: {source}")]
    Sqlite {
        #[source]
        source: rusqlite::Error,
    },
}

impl RepoError {
    /// Convenience constructor — `RepoError::Parse(ErrorLocation::from_detail(s))`
    /// for the bare-string case that 90% of existing parser sites need.
    #[must_use]
    pub fn parse_msg(detail: impl Into<String>) -> Self {
        Self::Parse(ErrorLocation::from_detail(detail))
    }

    /// File-context parse failure.
    #[must_use]
    pub fn parse_at_file(file: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Parse(ErrorLocation::at_file(file, detail))
    }

    /// Full file:line:col parse failure.
    #[must_use]
    pub fn parse_at(
        file: impl Into<String>,
        line: u32,
        col: u32,
        detail: impl Into<String>,
    ) -> Self {
        Self::Parse(ErrorLocation::at(file, line, col, detail))
    }

    /// File-context decompression failure.
    #[must_use]
    pub fn decompress_at_file(file: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Decompress(ErrorLocation::at_file(file, detail))
    }

    /// File-context verification failure.
    #[must_use]
    pub fn verify_at_file(file: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::Verify(ErrorLocation::at_file(file, detail))
    }
}

impl From<rusqlite::Error> for RepoError {
    fn from(source: rusqlite::Error) -> Self {
        Self::Sqlite { source }
    }
}

/// HTTP-layer error. `network` covers connect/read/timeout/TLS; `status`
/// covers any non-2xx (and non-304 for conditional GET) response; `policy`
/// covers refusal to fetch due to offline mode.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum HttpError {
    #[error("network error fetching {url}: {source}")]
    Network {
        url: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("HTTP {status} for {url}")]
    Status { url: String, status: u16 },

    #[error("refusing to fetch {url}: tool is in offline mode")]
    OfflinePolicy { url: String },

    #[error("invalid URL: {0}")]
    InvalidUrl(String),
}

/// Resolver-layer error. `Unsatisfiable` is returned when the walker
/// can't find a provider; `RichDep` flags rich-dependency boolean
/// expressions that the P0 walker doesn't try to resolve (escalation
/// to a SAT backend is gated behind `feature = "resolvo"` on the
/// resolver crate).
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum SolveError {
    #[error("unsatisfiable: {summary}")]
    Unsatisfiable { summary: String },

    #[error("rich dependency not solved by walker: {expr}")]
    RichDep { expr: String },

    #[error("solver internal error: {0}")]
    Internal(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_location_display_full() {
        let loc = ErrorLocation::at("primary.xml", 42, 7, "version: invalid utf-8");
        assert_eq!(loc.to_string(), "primary.xml:42:7: version: invalid utf-8");
    }

    #[test]
    fn error_location_display_file_only() {
        let loc = ErrorLocation::at_file("repomd.xml", "missing data type");
        assert_eq!(loc.to_string(), "repomd.xml: missing data type");
    }

    #[test]
    fn error_location_display_detail_only() {
        let loc = ErrorLocation::from_detail("solver: dead end");
        assert_eq!(loc.to_string(), "solver: dead end");
    }

    #[test]
    fn parse_constructors_round_trip_through_repo_error() {
        let e = RepoError::parse_msg("plain");
        assert!(e.to_string().contains("plain"));

        let e = RepoError::parse_at_file("filelists.xml", "bad attr");
        // Top-level Display wraps the inner Location's Display.
        assert!(e.to_string().contains("filelists.xml: bad attr"));

        let e = RepoError::parse_at("primary.xml", 100, 5, "EOF");
        let s = e.to_string();
        assert!(s.contains("primary.xml:100:5: EOF"), "got: {s}");
    }
}
