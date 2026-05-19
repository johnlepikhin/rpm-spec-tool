//! Conversions between domain types and SQL column values.
//!
//! Kept in a dedicated module so the SQL access paths in `mod.rs` stay
//! readable. The `from_*` and `to_*` helpers assume well-formed rows;
//! malformed data surfaces as `RepoError::Cache` via the caller.

use std::sync::Arc;

use crate::error::RepoError;
use crate::evr::EVR;
use crate::package::{CapFlags, Capability, PkgChecksum};

/// Map [`CapFlags`] to the stable on-disk discriminator.
#[must_use]
pub fn cap_flags_to_str(f: CapFlags) -> &'static str {
    match f {
        CapFlags::None => "NONE",
        CapFlags::EQ => "EQ",
        CapFlags::LT => "LT",
        CapFlags::LE => "LE",
        CapFlags::GT => "GT",
        CapFlags::GE => "GE",
    }
}

/// Reverse of [`cap_flags_to_str`]. Unknown strings → `Err` because
/// silently coercing to `None` would mask a corrupt cache.
pub fn cap_flags_from_str(s: &str) -> Result<CapFlags, RepoError> {
    match s {
        "NONE" => Ok(CapFlags::None),
        "EQ" => Ok(CapFlags::EQ),
        "LT" => Ok(CapFlags::LT),
        "LE" => Ok(CapFlags::LE),
        "GT" => Ok(CapFlags::GT),
        "GE" => Ok(CapFlags::GE),
        other => Err(RepoError::Cache(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unknown cap flags discriminator: {other}"),
        ))),
    }
}

/// Decompose a [`Capability`] into the column tuple used by `caps`.
#[must_use]
pub fn capability_columns(
    cap: &Capability,
) -> (&str, &'static str, Option<u32>, Option<&str>, Option<&str>) {
    let (epoch, version, release) = match &cap.evr {
        Some(evr) => (Some(evr.epoch), Some(evr.version.as_str()), Some(evr.release.as_str())),
        None => (None, None, None),
    };
    (
        cap.name.as_ref(),
        cap_flags_to_str(cap.flags),
        epoch,
        version,
        release,
    )
}

/// Build a [`Capability`] from raw column values. Returns `Err` if the
/// flag string is unknown.
pub fn capability_from_columns(
    name: &str,
    flags_str: &str,
    epoch: Option<u32>,
    version: Option<&str>,
    release: Option<&str>,
) -> Result<Capability, RepoError> {
    let flags = cap_flags_from_str(flags_str)?;
    let evr = match (version, release) {
        (Some(v), Some(r)) => Some(EVR::new(epoch, v, r)),
        // Defensive: NULL in just one column is a corruption signal,
        // but we don't fail the whole load — emit `None` so the lookup
        // treats it as versionless and the user only sees a degraded
        // result, not a hard cache wipe.
        _ => None,
    };
    Ok(Capability {
        name: Arc::from(name),
        flags,
        evr,
    })
}

/// Split a [`PkgChecksum`] into `(algorithm, hex)` columns.
#[must_use]
pub fn checksum_columns(c: &PkgChecksum) -> (&str, &str) {
    match c {
        PkgChecksum::Sha256(h) => ("sha256", h.as_str()),
        PkgChecksum::Sha1(h) => ("sha1", h.as_str()),
        PkgChecksum::Other { algo, hex } => (algo.as_str(), hex.as_str()),
    }
}

/// Build a [`PkgChecksum`] from `(algorithm, hex)` columns. Unknown
/// algorithms are preserved as [`PkgChecksum::Other`] — same policy as
/// the parser.
#[must_use]
pub fn checksum_from_columns(algo: &str, hex: &str) -> PkgChecksum {
    match algo {
        "sha256" => PkgChecksum::Sha256(hex.to_string()),
        "sha1" => PkgChecksum::Sha1(hex.to_string()),
        _ => PkgChecksum::Other {
            algo: algo.to_string(),
            hex: hex.to_string(),
        },
    }
}
