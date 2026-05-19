//! Error variants partitioned by phase. Consumers can `match` on
//! variant to render user-friendly messages or react programmatically
//! (e.g. retry on `Http`, fail on `Verify`).

use std::io;

use thiserror::Error;

/// Top-level repository error. Wraps phase-specific errors so the CLI
/// can attribute a failure to fetch, decompress, parse, verify, solve,
/// cache I/O, or config processing without lossy string conversion.
#[derive(Error, Debug)]
#[non_exhaustive]
// TODO: structured carriers (file path, line/col) for Parse/Decompress/Verify.
// Right now we string-format the inner cause to keep M1's surface small.
pub enum RepoError {
    #[error("HTTP fetch failed: {0}")]
    Http(#[from] HttpError),

    #[error("decompression failed: {0}")]
    Decompress(String),

    #[error("XML parse failed: {0}")]
    Parse(String),

    #[error("GPG verification failed: {0}")]
    Verify(String),

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
