//! Cross-profile macro portability analysis.
//!
//! Given a parsed spec and a resolved target set, compute for every
//! macro name referenced by the spec which member profiles can
//! resolve the name. The output drives `matrix portability` — a
//! release-engineering view that surfaces which macros are platform-
//! specific without forcing the engineer to grep registries by hand.
//!
//! Phase 1 limitations (deliberate):
//!
//! * Reports presence/absence only, not whether values differ between
//!   profiles that all define a macro. A follow-up can compare
//!   `MacroRegistry::expand_to_literal` results to flag value drift.
//! * No usage location information — `MacroRef` carries no span at
//!   the AST level. A future enhancement can track the enclosing
//!   item's span via a scoped visitor.

use rpm_spec::ast::{Span, SpecFile};
use rpm_spec_profile::ResolvedTargetSet;

use crate::macro_usage::MacroUsageCollector;

/// Status of one macro across the target set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PortabilityStatus {
    /// Every member profile defines the macro.
    Portable,
    /// Some define it, others don't. The most actionable category —
    /// usually means the spec needs `%{?foo}` guards or a
    /// compatibility shim macro.
    Partial,
    /// No member profile defines the macro. Either the spec relies
    /// on a macro the user must define via `--define`, or it's a
    /// typo / removed-upstream macro.
    Missing,
}

impl PortabilityStatus {
    /// Lowercase kebab-style label used by both human and JSON
    /// output. Centralised so doc, code and tests stay aligned —
    /// adding a new variant fails to compile here until labelled,
    /// which is what the `#[non_exhaustive]` enum implies.
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Portable => "portable",
            Self::Partial => "partial",
            Self::Missing => "missing",
        }
    }
}

/// One row of the portability report: a macro name plus the profile
/// IDs that define / don't define it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PortabilityEntry {
    pub name: String,
    pub status: PortabilityStatus,
    /// Sorted profile IDs that define the macro.
    pub defined_in: Vec<String>,
    /// Sorted profile IDs that don't.
    pub missing_in: Vec<String>,
}

/// Whole-spec portability report.
///
/// `#[non_exhaustive]` — fields and methods may grow as Phase 2
/// introduces value-drift detection; construct via
/// [`Self::compute`] or [`Self::from_names`] only.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PortabilityReport {
    /// One entry per distinct macro name used in the spec, sorted
    /// by `(status_rank, name)` so the most actionable rows (Missing
    /// → Partial → Portable) come first.
    pub entries: Vec<PortabilityEntry>,
}

impl PortabilityReport {
    /// Compute the report. The macro registry of each profile is
    /// consulted by name; we do not attempt to expand values.
    pub fn compute(spec: &SpecFile<Span>, target_set: &ResolvedTargetSet) -> Self {
        let names = MacroUsageCollector::collect(spec);
        Self::from_names(&names, target_set)
    }

    /// Build the report from an already-collected name set. Useful
    /// when the caller already has the set or wants to merge across
    /// multiple sources.
    pub fn from_names(
        names: &std::collections::BTreeSet<String>,
        target_set: &ResolvedTargetSet,
    ) -> Self {
        // O(names × profiles) — both are bounded (typical specs use
        // <100 macros, typical target sets have <30 profiles). A
        // per-macro HashSet of profile defs would be a needless
        // intermediate.
        let mut entries: Vec<PortabilityEntry> = names
            .iter()
            .map(|name| {
                let mut defined_in = Vec::new();
                let mut missing_in = Vec::new();
                for rt in &target_set.targets {
                    if rt.profile.macros.get(name).is_some() {
                        defined_in.push(rt.profile_id.clone());
                    } else {
                        missing_in.push(rt.profile_id.clone());
                    }
                }
                let status = classify(defined_in.len(), missing_in.len());
                PortabilityEntry {
                    name: name.clone(),
                    status,
                    defined_in,
                    missing_in,
                }
            })
            .collect();

        // Surface the most actionable rows first.
        entries.sort_by(|a, b| {
            status_rank(a.status)
                .cmp(&status_rank(b.status))
                .then_with(|| a.name.cmp(&b.name))
        });

        Self { entries }
    }

    /// Distinct macro names recorded — equal to `self.entries.len()`
    /// by construction (`from_names` produces one entry per name).
    /// Convenience for status output.
    pub fn total_used(&self) -> usize {
        self.entries.len()
    }

    /// Number of rows whose status is [`PortabilityStatus::Missing`].
    pub fn missing_count(&self) -> usize {
        self.counts().missing
    }

    /// Number of rows whose status is [`PortabilityStatus::Partial`].
    pub fn partial_count(&self) -> usize {
        self.counts().partial
    }

    /// Number of rows whose status is [`PortabilityStatus::Portable`].
    pub fn portable_count(&self) -> usize {
        self.counts().portable
    }

    /// Single-pass tally of all three status counts. Cheaper than
    /// three separate `*_count()` calls when the renderer needs
    /// every total (which is the typical case).
    pub fn counts(&self) -> StatusCounts {
        let mut c = StatusCounts::default();
        for e in &self.entries {
            match e.status {
                PortabilityStatus::Missing => c.missing += 1,
                PortabilityStatus::Partial => c.partial += 1,
                PortabilityStatus::Portable => c.portable += 1,
            }
        }
        c
    }
}

/// Per-status totals returned by [`PortabilityReport::counts`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct StatusCounts {
    pub missing: usize,
    pub partial: usize,
    pub portable: usize,
}

fn classify(defined: usize, missing: usize) -> PortabilityStatus {
    match (defined, missing) {
        (_, 0) => PortabilityStatus::Portable,
        (0, _) => PortabilityStatus::Missing,
        (_, _) => PortabilityStatus::Partial,
    }
}

fn status_rank(s: PortabilityStatus) -> u8 {
    match s {
        PortabilityStatus::Missing => 0,
        PortabilityStatus::Partial => 1,
        PortabilityStatus::Portable => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_profile::{ProfileSection, ResolveOptions, TargetEntry, resolve_target_set};
    use std::path::Path;

    fn target_with(profiles: &[&str]) -> TargetEntry {
        TargetEntry::from_profiles(profiles.iter().map(|s| (*s).to_string()).collect())
    }

    fn small_target_set(profile_ids: &[&str]) -> ResolvedTargetSet {
        let section = ProfileSection::default();
        let target = target_with(profile_ids);
        resolve_target_set(
            &section,
            "T",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap()
    }

    fn names(items: &[&str]) -> std::collections::BTreeSet<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn portable_when_every_profile_defines_macro() {
        // `_bindir` is in the bundled showrc of every distro-aware
        // profile. `generic` has no showrc → it won't define it.
        // To get true "portable" we'd need a set of distros only;
        // here use just rhel-9-x86_64 + rhel-8-x86_64.
        let set = small_target_set(&["rhel-9-x86_64", "rhel-8-x86_64"]);
        let report = PortabilityReport::from_names(&names(&["_bindir"]), &set);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].status, PortabilityStatus::Portable);
        assert_eq!(report.entries[0].defined_in.len(), 2);
        assert!(report.entries[0].missing_in.is_empty());
    }

    #[test]
    fn missing_when_no_profile_defines_macro() {
        let set = small_target_set(&["generic", "rhel-9-x86_64"]);
        let report =
            PortabilityReport::from_names(&names(&["definitely_not_a_real_macro_xyz"]), &set);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].status, PortabilityStatus::Missing);
        assert!(report.entries[0].defined_in.is_empty());
        assert_eq!(report.entries[0].missing_in.len(), 2);
    }

    #[test]
    fn partial_when_subset_defines_macro() {
        // generic profile has no showrc bundle, so it won't define
        // most distro macros. rhel-9-x86_64 has the full bundle.
        let set = small_target_set(&["generic", "rhel-9-x86_64"]);
        let report = PortabilityReport::from_names(&names(&["_bindir"]), &set);
        assert_eq!(report.entries[0].status, PortabilityStatus::Partial);
        assert!(
            report.entries[0]
                .defined_in
                .contains(&"rhel-9-x86_64".to_string())
        );
        assert!(
            report.entries[0]
                .missing_in
                .contains(&"generic".to_string())
        );
    }

    #[test]
    fn entries_sorted_missing_first_then_partial_then_portable() {
        let set = small_target_set(&["generic", "rhel-9-x86_64"]);
        let report =
            PortabilityReport::from_names(&names(&["_bindir", "definitely_missing_xyz"]), &set);
        // _bindir → Partial, definitely_missing_xyz → Missing.
        // Sort order: Missing first.
        assert_eq!(report.entries[0].name, "definitely_missing_xyz");
        assert_eq!(report.entries[1].name, "_bindir");
    }

    #[test]
    fn status_counts_sum_to_total() {
        let set = small_target_set(&["generic", "rhel-9-x86_64"]);
        let report =
            PortabilityReport::from_names(&names(&["_bindir", "definitely_missing_xyz"]), &set);
        assert_eq!(
            report.missing_count() + report.partial_count() + report.portable_count(),
            report.total_used()
        );
    }

    #[test]
    fn empty_name_set_yields_empty_report() {
        let set = small_target_set(&["generic"]);
        let report = PortabilityReport::from_names(&std::collections::BTreeSet::new(), &set);
        assert!(report.entries.is_empty());
        assert_eq!(report.total_used(), 0);
    }

    #[test]
    fn classify_matches_documented_thresholds() {
        assert_eq!(classify(2, 0), PortabilityStatus::Portable);
        assert_eq!(classify(0, 2), PortabilityStatus::Missing);
        assert_eq!(classify(1, 1), PortabilityStatus::Partial);
    }
}
