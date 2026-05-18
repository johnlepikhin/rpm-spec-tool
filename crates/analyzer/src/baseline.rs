//! Baseline mode for matrix runs.
//!
//! Records the [`MatrixSignature`]s that are already known to fire on a
//! given spec under a given target set, so that subsequent runs can
//! suppress them and surface only *new* regressions. The use case is
//! CI gating on a legacy spec where the existing warning corpus would
//! drown out any genuinely new finding.
//!
//! The format is a small JSON document — versioned, with one entry per
//! known finding. `lint_id` and `message` are kept alongside the
//! signature for human readability and as a soft tripwire: if the
//! signature hash algorithm ever changes (it shouldn't, but
//! `DefaultHasher` is not contractually stable across rustc versions),
//! the matching entries will quietly become "new" — the `message` /
//! `lint_id` columns help users notice that everything got duplicated.

use std::collections::HashSet;
use std::io::{Read, Write};

use serde::{Deserialize, Serialize};

use crate::matrix::{AggregatedDiagnostic, MatrixSignature, MatrixSignatureParseError};

/// JSON shape of a baseline file. Version 1 records the matrix
/// signatures known to fire together with minimal context. Future
/// versions can add fields (recorded date, recorded rustc version,
/// per-profile breakdown) without breaking older readers thanks to
/// `#[serde(default)]` on the new fields.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Baseline {
    /// File-format version. Currently always `1`. Bump when adding a
    /// breaking field (anything other than additive `#[serde(default)]`).
    pub baseline_version: u32,
    /// One entry per known finding. Order is preserved for
    /// reproducible diffs. Construction is restricted to
    /// [`Self::from_aggregated`] and [`Self::read`] so the "each
    /// `matrix_signature` parses cleanly" invariant
    /// [`Self::signature_set`] depends on cannot be violated by
    /// external mutation.
    entries: Vec<BaselineEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct BaselineEntry {
    /// Stable hex form of [`MatrixSignature`] (16 lowercase hex chars).
    pub matrix_signature: String,
    /// `RPM` lint id (e.g. `"RPM055"`). Informational.
    pub lint_id: String,
    /// Diagnostic message at recording time. Informational.
    pub message: String,
    /// Number of profiles in the recorded matrix that emitted this
    /// finding. Helps users notice if the affected set is suddenly
    /// growing without the diagnostic itself being new.
    pub affected_profile_count: usize,
}

impl Baseline {
    /// Current on-disk format version.
    pub const CURRENT_VERSION: u32 = 1;

    /// Hard cap (16 MiB) on bytes read by [`Self::read`]. Defends
    /// shared CI runners against an oversized baseline file (e.g. a
    /// crafted PR replacing the legit file) silently consuming
    /// memory. 16 MiB leaves room for tens of thousands of normal
    /// entries with worst-case-long messages.
    pub const MAX_READ_BYTES: u64 = 16 * 1024 * 1024;

    /// Build a baseline by snapshotting the aggregated findings of a
    /// completed matrix run. The recorded snapshot is shape-only —
    /// no spec contents or paths are persisted, so the file can be
    /// committed to a VCS without leaking source.
    pub fn from_aggregated(aggregated: &[AggregatedDiagnostic]) -> Self {
        let entries = aggregated
            .iter()
            .map(|ad| BaselineEntry {
                matrix_signature: ad.signature.to_string(),
                lint_id: ad.diagnostic.lint_id.to_string(),
                message: ad.diagnostic.message.clone(),
                affected_profile_count: ad.affected_profiles.len(),
            })
            .collect();
        Self {
            baseline_version: Self::CURRENT_VERSION,
            entries,
        }
    }

    /// Read a baseline document from any byte source (file, stdin).
    /// Every `matrix_signature` entry is validated against the hex
    /// `Display` contract of [`MatrixSignature`] up-front; a malformed
    /// entry surfaces as [`BaselineError::InvalidEntry`] rather than
    /// silently failing to match later (which would turn every
    /// finding into a "new" one and break CI gating).
    pub fn read<R: Read>(r: R) -> Result<Self, BaselineError> {
        let parsed: Self =
            serde_json::from_reader(r.take(Self::MAX_READ_BYTES)).map_err(BaselineError::Json)?;
        if parsed.baseline_version != Self::CURRENT_VERSION {
            return Err(BaselineError::UnsupportedVersion {
                found: parsed.baseline_version,
                supported: Self::CURRENT_VERSION,
            });
        }
        for (index, entry) in parsed.entries.iter().enumerate() {
            MatrixSignature::from_hex(&entry.matrix_signature).map_err(|source| {
                BaselineError::InvalidEntry {
                    index,
                    lint_id: entry.lint_id.clone(),
                    source,
                }
            })?;
        }
        Ok(parsed)
    }

    /// Pretty-print to any byte sink. Pretty form is intentional —
    /// the file is meant to be VCS-tracked and reviewed in PRs.
    pub fn write<W: Write>(&self, mut w: W) -> Result<(), BaselineError> {
        serde_json::to_writer_pretty(&mut w, self).map_err(BaselineError::Json)?;
        writeln!(w).map_err(BaselineError::Io)?;
        Ok(())
    }

    /// Build a typed [`MatrixSignature`] set for O(1) `contains`
    /// checks during filtering. The set is owned so callers can drop
    /// the [`Baseline`] immediately after building the index.
    ///
    /// Parsing is infallible by construction: every code path that
    /// produces a `Baseline` ([`Self::from_aggregated`],
    /// [`Self::read`]) guarantees each `matrix_signature` is a valid
    /// hex form of [`MatrixSignature`]. The unreachable arm panics
    /// rather than silently dropping an entry — that would defeat
    /// `--fail-on new` exactly when CI gating matters most.
    pub fn signature_set(&self) -> HashSet<MatrixSignature> {
        self.entries
            .iter()
            .map(|e| {
                MatrixSignature::from_hex(&e.matrix_signature).expect(
                    "BaselineEntry::matrix_signature is validated by Baseline::read \
                     and produced by MatrixSignature::Display elsewhere",
                )
            })
            .collect()
    }

    /// Read-only view of the entries. Provided for renderers and
    /// CLI summaries; mutation is prohibited so the
    /// `signature_set()` invariant holds.
    pub fn entries(&self) -> &[BaselineEntry] {
        &self.entries
    }

    /// Number of recorded findings. Convenience for status output.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if the baseline records no findings — typical for
    /// fresh projects before any deny-level rule has fired.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BaselineError {
    /// Either parse (on `read`) or serialisation (on `write`) failed
    /// inside `serde_json`. The shared variant matches what serde
    /// itself returns — both directions surface the same error type.
    #[error("baseline JSON error: {0}")]
    Json(#[source] serde_json::Error),
    /// Low-level IO failure when writing the trailing newline after
    /// the JSON body. Read-side IO errors surface inside [`Self::Json`]
    /// because `serde_json::from_reader` wraps them in `serde_json::Error`
    /// (see `serde_json::Error::is_io`).
    #[error("baseline IO error: {0}")]
    Io(#[source] std::io::Error),
    /// File-format version is recognised but not supported by this
    /// build. The message hints at the most common fix (`regenerate`)
    /// — strictly correct only when `found < supported`; the rarer
    /// `found > supported` case means the binary is older than the
    /// baseline and the user should upgrade the tool.
    #[error(
        "baseline file has version {found} but this build only supports version {supported} — \
         regenerate with `matrix baseline create` (or upgrade rpm-spec-tool if found > supported)"
    )]
    UnsupportedVersion { found: u32, supported: u32 },
    /// One entry in the baseline has a `matrix_signature` that is
    /// not 16 lowercase hex digits. Caught up-front in `read` so the
    /// silent-no-match failure mode can't escape into `--fail-on new`.
    #[error("baseline entry #{index} for lint `{lint_id}`: {source}")]
    InvalidEntry {
        index: usize,
        lint_id: String,
        #[source]
        source: MatrixSignatureParseError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Severity;
    use crate::matrix::MatrixSignature;
    use rpm_spec::ast::Span;

    fn diag(lint_id: &'static str, message: &str, start: usize, end: usize) -> crate::Diagnostic {
        crate::Diagnostic {
            lint_id,
            lint_name: lint_id,
            severity: Severity::Warn,
            message: message.to_string(),
            primary_span: Span {
                start_byte: start,
                end_byte: end,
                start_line: 1,
                start_column: 1,
                end_line: 1,
                end_column: 1,
            },
            labels: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    fn agg(d: crate::Diagnostic, profiles: &[&str]) -> AggregatedDiagnostic {
        AggregatedDiagnostic {
            signature: MatrixSignature::for_diagnostic(&d),
            diagnostic: d,
            affected_profiles: profiles.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    #[test]
    fn from_aggregated_records_one_entry_per_signature() {
        let entries = vec![
            agg(diag("RPM055", "msg", 1, 2), &["a", "b"]),
            agg(diag("RPM062", "other", 3, 4), &["a"]),
        ];
        let b = Baseline::from_aggregated(&entries);
        assert_eq!(b.baseline_version, Baseline::CURRENT_VERSION);
        assert_eq!(b.len(), 2);
        assert_eq!(b.entries()[0].lint_id, "RPM055");
        assert_eq!(b.entries()[0].affected_profile_count, 2);
    }

    #[test]
    fn roundtrip_through_json_preserves_signatures() {
        let entries = vec![agg(diag("RPM055", "msg", 1, 2), &["a"])];
        let original = Baseline::from_aggregated(&entries);
        let mut buf = Vec::new();
        original.write(&mut buf).unwrap();
        let parsed = Baseline::read(buf.as_slice()).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed.entries()[0].matrix_signature,
            original.entries()[0].matrix_signature
        );
    }

    #[test]
    fn read_rejects_unsupported_version() {
        // Synthetic future version; reader must refuse rather than
        // silently accept partial data.
        let raw = r#"{ "baseline_version": 99, "entries": [] }"#;
        let err = Baseline::read(raw.as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            BaselineError::UnsupportedVersion {
                found: 99,
                supported: 1
            }
        ));
    }

    #[test]
    fn signature_set_recognises_recorded_signature() {
        let d = diag("RPM055", "msg", 1, 2);
        let sig = MatrixSignature::for_diagnostic(&d);
        let b = Baseline::from_aggregated(&[agg(d, &["a"])]);
        assert!(b.signature_set().contains(&sig));
    }

    #[test]
    fn signature_set_returns_false_for_unrecorded_signature() {
        let recorded = diag("RPM055", "msg", 1, 2);
        let other = diag("RPM999", "different", 100, 200);
        let other_sig = MatrixSignature::for_diagnostic(&other);
        let b = Baseline::from_aggregated(&[agg(recorded, &["a"])]);
        assert!(!b.signature_set().contains(&other_sig));
    }

    #[test]
    fn read_rejects_invalid_signature_format() {
        // Corrupted/edited baseline must not slip through. Without
        // this check, the bad entry would simply never match any
        // real diagnostic — every finding would surface as new
        // under --fail-on=new, defeating CI gating silently.
        let raw = r#"{
            "baseline_version": 1,
            "entries": [
                {
                    "matrix_signature": "NOT-HEX-AT-ALL",
                    "lint_id": "RPM055",
                    "message": "msg",
                    "affected_profile_count": 1
                }
            ]
        }"#;
        let err = Baseline::read(raw.as_bytes()).unwrap_err();
        match err {
            BaselineError::InvalidEntry { index, lint_id, .. } => {
                assert_eq!(index, 0);
                assert_eq!(lint_id, "RPM055");
            }
            other => panic!("expected InvalidEntry, got {other:?}"),
        }
    }

    #[test]
    fn read_rejects_uppercase_hex_signature() {
        // Display contract is lowercase. Uppercase pre-empts a class
        // of "they look the same to humans, but HashSet comparison
        // fails" bugs.
        let raw = r#"{
            "baseline_version": 1,
            "entries": [
                {
                    "matrix_signature": "DEADBEEFDEADBEEF",
                    "lint_id": "RPM055",
                    "message": "msg",
                    "affected_profile_count": 1
                }
            ]
        }"#;
        assert!(matches!(
            Baseline::read(raw.as_bytes()),
            Err(BaselineError::InvalidEntry { .. })
        ));
    }

    #[test]
    fn read_rejects_unknown_field() {
        // deny_unknown_fields contract — typos in the file shouldn't
        // silently become a no-op.
        let raw = r#"{ "baseline_version": 1, "entries": [], "garbage": true }"#;
        let err = Baseline::read(raw.as_bytes()).unwrap_err();
        assert!(matches!(err, BaselineError::Json(_)));
    }

    #[test]
    fn signature_set_is_o1_lookup() {
        let entries = vec![
            agg(diag("RPM055", "msg", 1, 2), &["a"]),
            agg(diag("RPM062", "other", 3, 4), &["b"]),
        ];
        let b = Baseline::from_aggregated(&entries);
        let set = b.signature_set();
        assert_eq!(set.len(), 2);
        // Each typed signature recomputed from the on-disk hex form
        // must be present in the set.
        for entry in b.entries() {
            let sig = MatrixSignature::from_hex(&entry.matrix_signature).expect("valid");
            assert!(set.contains(&sig));
        }
    }

    #[test]
    fn empty_baseline_signature_set_is_empty() {
        // Fresh projects start with no recorded findings — the read
        // path must accept an empty entries array without error and
        // the resulting signature_set must be empty (not panic).
        let raw = r#"{ "baseline_version": 1, "entries": [] }"#;
        let baseline = Baseline::read(raw.as_bytes()).expect("empty baseline parses");
        assert!(baseline.is_empty());
        assert!(baseline.signature_set().is_empty());
    }
}
