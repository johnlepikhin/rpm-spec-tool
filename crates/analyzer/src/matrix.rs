//! Multi-profile (matrix) analyzer wrapper.
//!
//! Runs the existing single-profile analyzer ([`analyze_with_profile_at`])
//! against every profile in a [`ResolvedTargetSet`] and aggregates the
//! findings so the same spec-level issue surfaces once per signature
//! rather than once per profile.
//!
//! This is a thin wrapper layer — no lint rules are aware of it. Rules
//! continue to receive one [`Profile`](rpm_spec_profile::Profile) per
//! invocation, exactly as today. The matrix grouping happens after the
//! analyzer returns.

use std::collections::BTreeMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::Path;

use rpm_spec_profile::ResolvedTargetSet;

use crate::config::Config;
use crate::diagnostic::Diagnostic;
use crate::session::{ParseOutcome, analyze_with_profile_at};

/// Stable, opaque signature for cross-profile diagnostic aggregation.
///
/// Computed from `(lint_id, span byte range, message)`. The message is
/// hashed verbatim in Phase 1: rules whose findings should aggregate
/// across profiles must produce profile-stable text (no embedded macro
/// values, arch names, etc.). When a rule emits profile-specific text
/// for the same root cause, the finding will appear as N separate
/// aggregated entries with overlapping `affected_profiles` — visible
/// and easy to spot. The opposite failure mode (false merges) is
/// strictly avoided.
///
/// The hex `Display` representation is what JSON/SARIF output writes
/// as `matrix_signature`. Treat it as opaque on the consumer side —
/// the only invariants are *stable across runs of the same binary*
/// and *equal iff the findings are the same*.
///
/// **Stability caveat.** Phase 1 uses
/// [`std::collections::hash_map::DefaultHasher`] (currently SipHash
/// 1-3 with a fixed seed), which is *not* contractually stable
/// across stdlib / rustc upgrades. In practice signatures rarely
/// change, but plan for a one-time baseline refresh after a major
/// toolchain bump. See `doc/matrix.md` "Stability caveat".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MatrixSignature(u64);

impl MatrixSignature {
    /// Compute the signature for a diagnostic. Output renderers use
    /// this to correlate per-profile findings back to aggregated
    /// entries — needed because SARIF emits flat results and has to
    /// look up the matching `affected_profiles` per diagnostic.
    pub fn for_diagnostic(d: &Diagnostic) -> Self {
        signature(d)
    }

    /// Parse the hex `Display` form back into a [`MatrixSignature`].
    /// Strict: requires exactly [`SIGNATURE_HEX_LEN`] lowercase hex
    /// digits — the same shape `Display` emits. Used by the baseline
    /// loader to validate that on-disk entries are well-formed before
    /// they reach the comparison set.
    pub fn from_hex(s: &str) -> Result<Self, MatrixSignatureParseError> {
        let err = |reason| MatrixSignatureParseError {
            value: s.to_string(),
            reason,
        };
        if s.len() != SIGNATURE_HEX_LEN {
            return Err(err("expected 16 hex characters"));
        }
        // Byte-level scan: length was already pinned at 16 ASCII bytes
        // and we want to reject uppercase A–F. `bytes()` skips UTF-8
        // decoding `chars()` would do; `matches!` keeps the predicate
        // in one branch.
        if !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(err("expected lowercase hex digits only"));
        }
        // Unreachable in practice — 16 lowercase hex digits always
        // fit in u64. Keep the fallible call rather than `unwrap`
        // so a future widening of `SIGNATURE_HEX_LEN` surfaces as a
        // typed error rather than a panic.
        let value = u64::from_str_radix(s, 16).map_err(|_| err("could not parse as u64"))?;
        Ok(MatrixSignature(value))
    }
}

/// Error returned by [`MatrixSignature::from_hex`] for any input that
/// is not exactly 16 lowercase hex digits. Carries the offending value
/// so callers can surface it (e.g. `BaselineError::InvalidEntry`).
///
/// Fields are private; use [`Self::value`] / [`Self::reason`] for read
/// access. The struct is `#[non_exhaustive]` so a future build can add
/// e.g. `position: usize` without breaking dependents.
#[derive(Debug, thiserror::Error)]
#[error("invalid matrix signature `{value}`: {reason}")]
#[non_exhaustive]
pub struct MatrixSignatureParseError {
    value: String,
    reason: &'static str,
}

impl MatrixSignatureParseError {
    /// The offending input string, verbatim.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Static human-readable reason — one of: "expected 16 hex
    /// characters", "expected lowercase hex digits only", "could not
    /// parse as u64".
    pub fn reason(&self) -> &'static str {
        self.reason
    }
}

/// Width, in characters, of [`MatrixSignature`]'s hex `Display` form.
/// Tied to the backing `u64` width — `size_of::<u64>() * 2`. Tests
/// pin this contract; downstream consumers (SARIF/JSON parsers) can
/// rely on it to know the column width up-front.
pub const SIGNATURE_HEX_LEN: usize = std::mem::size_of::<u64>() * 2;

impl fmt::Display for MatrixSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:01$x}", self.0, SIGNATURE_HEX_LEN)
    }
}

/// Result of analyzing one spec under one member profile of a target
/// set. Mirrors [`analyze_with_profile_at`]'s return value plus the
/// profile identifier the matrix needs to attribute findings.
#[derive(Debug)]
#[non_exhaustive]
pub struct ProfileResult {
    /// Profile identifier from `[targets.<set>.profiles]` (or `--profiles`).
    pub profile_id: String,
    /// Parser outcome; `parse.parser_diagnostics.is_empty()` is the
    /// "parse ok" predicate.
    pub parse: ParseOutcome,
    /// Lint findings for this profile, after severity overrides.
    pub diagnostics: Vec<Diagnostic>,
}

/// One aggregated finding: a representative [`Diagnostic`] plus the
/// sorted list of profile IDs that produced it.
///
/// The representative is the first occurrence in declared profile
/// order — deterministic across runs, and matches what the user sees
/// when they scan the per-profile section of the report.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AggregatedDiagnostic {
    /// Hash key used for cross-profile aggregation; see [`MatrixSignature`].
    pub signature: MatrixSignature,
    /// First-seen diagnostic instance (representative).
    pub diagnostic: Diagnostic,
    /// Sorted, deduplicated list of profile IDs that emitted the same finding.
    pub affected_profiles: Vec<String>,
}

/// Output of a matrix run for one spec source.
#[derive(Debug)]
#[non_exhaustive]
pub struct MatrixResult {
    /// Per-profile parse + diagnostics, in declared profile order.
    pub per_profile: Vec<ProfileResult>,
    /// Aggregated findings, sorted by signature for stable output.
    pub aggregated: Vec<AggregatedDiagnostic>,
}

/// Run the analyzer against every profile in `target_set` and return
/// per-profile + aggregated findings.
///
/// Sequential — profile resolution and analyzer state are not yet
/// thread-safe. The expected profile count (≤30) keeps wall-clock
/// reasonable; parallel execution is deferred to a later phase.
pub fn run_matrix(
    source: &str,
    source_path: Option<&Path>,
    config: &Config,
    target_set: &ResolvedTargetSet,
) -> MatrixResult {
    let _span = tracing::info_span!(
        "run_matrix",
        target_set = %target_set.id,
        profiles = target_set.targets.len(),
    )
    .entered();

    let mut per_profile = Vec::with_capacity(target_set.targets.len());
    for rt in &target_set.targets {
        // analyze_with_profile_at consumes the profile, so we must
        // clone. Profile is a deep tree (BTreeMap of macros, layers,
        // license/group lists); cost is O(macros + layers) — for a
        // typical 700-macro showrc this is ~1k allocations per call,
        // and N-fold for a target set of N members. Acceptable for
        // an MVP CLI but documented honestly.
        let (parse, diagnostics) =
            analyze_with_profile_at(source, source_path, config, rt.profile.clone());
        tracing::debug!(
            profile = %rt.profile_id,
            diagnostics = diagnostics.len(),
            parse_ok = parse.parser_diagnostics.is_empty(),
            "matrix member analysed"
        );
        per_profile.push(ProfileResult {
            profile_id: rt.profile_id.clone(),
            parse,
            diagnostics,
        });
    }
    let aggregated = aggregate(&per_profile);
    MatrixResult {
        per_profile,
        aggregated,
    }
}

fn aggregate(per_profile: &[ProfileResult]) -> Vec<AggregatedDiagnostic> {
    // BTreeMap keyed on signature keeps output order deterministic
    // (sorted by hash value). Within one signature, we accumulate a
    // profile-id list and reuse the first-seen Diagnostic as the
    // representative — matches what the per-profile section shows.
    let mut grouped: BTreeMap<MatrixSignature, (Diagnostic, Vec<String>)> = BTreeMap::new();

    let input_diags: usize = per_profile.iter().map(|p| p.diagnostics.len()).sum();
    for pr in per_profile {
        for diag in &pr.diagnostics {
            let sig = signature(diag);
            grouped
                .entry(sig)
                .and_modify(|(_, profiles)| profiles.push(pr.profile_id.clone()))
                .or_insert_with(|| (diag.clone(), vec![pr.profile_id.clone()]));
        }
    }
    tracing::debug!(
        groups = grouped.len(),
        input_diags,
        "matrix aggregation done"
    );

    grouped
        .into_iter()
        .map(|(signature, (diagnostic, mut affected_profiles))| {
            // Dedup defends against a rule that emits the same
            // finding twice in one analyzer run (the analyzer's own
            // sort_and_dedup catches most, but matrix shouldn't
            // depend on that).
            affected_profiles.sort();
            affected_profiles.dedup();
            AggregatedDiagnostic {
                signature,
                diagnostic,
                affected_profiles,
            }
        })
        .collect()
}

fn signature(d: &Diagnostic) -> MatrixSignature {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    d.lint_id.hash(&mut h);
    d.primary_span.start_byte.hash(&mut h);
    d.primary_span.end_byte.hash(&mut h);
    d.message.hash(&mut h);
    MatrixSignature(h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::Severity;
    use rpm_spec::ast::Span;

    fn fake_diag(lint_id: &'static str, message: &str, start: usize, end: usize) -> Diagnostic {
        Diagnostic {
            lint_id,
            lint_name: lint_id, // not used in signature
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

    fn profile_result(profile_id: &str, diags: Vec<Diagnostic>) -> ProfileResult {
        ProfileResult {
            profile_id: profile_id.to_string(),
            // Parse outcome is irrelevant for aggregation tests;
            // build a minimal valid value via parsing an empty source.
            parse: crate::session::parse(""),
            diagnostics: diags,
        }
    }

    #[test]
    fn signature_is_stable_across_runs() {
        let a = fake_diag("RPM001", "missing changelog", 10, 20);
        let b = fake_diag("RPM001", "missing changelog", 10, 20);
        assert_eq!(signature(&a), signature(&b));
    }

    #[test]
    fn signature_differs_on_lint_id() {
        let a = fake_diag("RPM001", "same span", 10, 20);
        let b = fake_diag("RPM002", "same span", 10, 20);
        assert_ne!(signature(&a), signature(&b));
    }

    #[test]
    fn signature_differs_on_span() {
        let a = fake_diag("RPM001", "msg", 10, 20);
        let b = fake_diag("RPM001", "msg", 11, 21);
        assert_ne!(signature(&a), signature(&b));
    }

    #[test]
    fn signature_differs_on_message_text() {
        // Profile-dependent text in messages currently splits
        // findings — documented behaviour; rules that want
        // aggregation must keep messages profile-stable.
        let a = fake_diag("RPM001", "for /usr/lib64", 10, 20);
        let b = fake_diag("RPM001", "for /usr/lib", 10, 20);
        assert_ne!(signature(&a), signature(&b));
    }

    #[test]
    fn aggregate_groups_identical_finding_across_profiles() {
        let diag = fake_diag("RPM055", "summary ends with dot", 5, 30);
        let per_profile = vec![
            profile_result("rhel-9-x86_64", vec![diag.clone()]),
            profile_result("rhel-8-x86_64", vec![diag.clone()]),
            profile_result("altlinux-10-x86_64", vec![diag.clone()]),
        ];
        let agg = aggregate(&per_profile);
        assert_eq!(agg.len(), 1);
        // Sorted profile names — output stability.
        assert_eq!(
            agg[0].affected_profiles,
            vec!["altlinux-10-x86_64", "rhel-8-x86_64", "rhel-9-x86_64"]
        );
    }

    #[test]
    fn aggregate_keeps_distinct_findings_separate() {
        let a = fake_diag("RPM055", "summary ends with dot", 5, 30);
        let b = fake_diag("RPM062", "use grep -E instead of egrep", 100, 105);
        let per_profile = vec![
            profile_result("rhel-9-x86_64", vec![a.clone(), b.clone()]),
            profile_result("altlinux-10-x86_64", vec![a.clone()]),
        ];
        let agg = aggregate(&per_profile);
        assert_eq!(agg.len(), 2);
        // RPM055 is on both; RPM062 only on RHEL.
        let r55 = agg
            .iter()
            .find(|d| d.diagnostic.lint_id == "RPM055")
            .unwrap();
        let r62 = agg
            .iter()
            .find(|d| d.diagnostic.lint_id == "RPM062")
            .unwrap();
        assert_eq!(
            r55.affected_profiles,
            vec!["altlinux-10-x86_64", "rhel-9-x86_64"]
        );
        assert_eq!(r62.affected_profiles, vec!["rhel-9-x86_64"]);
    }

    #[test]
    fn aggregate_dedups_repeated_profile_emission() {
        // A misbehaving rule that emits the same diagnostic twice
        // for the same profile must not double-count it in
        // affected_profiles.
        let diag = fake_diag("RPM055", "summary ends with dot", 5, 30);
        let per_profile = vec![profile_result(
            "rhel-9-x86_64",
            vec![diag.clone(), diag.clone()],
        )];
        let agg = aggregate(&per_profile);
        assert_eq!(agg.len(), 1);
        assert_eq!(agg[0].affected_profiles, vec!["rhel-9-x86_64"]);
    }

    #[test]
    fn signature_display_is_16_hex_chars() {
        // SARIF/JSON consumers want a printable form; pin the
        // format so a refactor of MatrixSignature doesn't silently
        // change a public contract.
        let sig = signature(&fake_diag("RPM001", "msg", 0, 1));
        let s = sig.to_string();
        assert_eq!(s.len(), SIGNATURE_HEX_LEN);
        assert!(
            s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }
}
