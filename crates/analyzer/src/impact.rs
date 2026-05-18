//! Per-profile impact of a spec change between two revisions.
//!
//! Phase 13 closes the diff/expand/classes/impact quartet. Where
//! `matrix diff` compares effective spec views between two
//! **profiles** at one revision, `matrix impact` compares effective
//! spec views of one spec at two **revisions**, per profile. The PR
//! review use case: "this commit touches `foo.spec` — which profiles
//! are materially affected and which deps moved?".
//!
//! Mechanics:
//!
//! 1. Caller fetches `from_spec` and `to_spec` bytes (typically via
//!    `git show REV:path`; the CLI helper lives in
//!    `crates/cli/src/commands/matrix/impact.rs::git_show`).
//! 2. [`ImpactReport::compute`] parses both, computes per-profile
//!    [`ProfileSignature`](crate::ProfileSignature)s on each side,
//!    and reports the symmetric set diff per (profile, tag) pair.
//! 3. Output lists `added` / `removed` / `unchanged` dep names for
//!    every profile and every compared tag.
//!
//! Branch-aware via [`IndeterminatePolicy::Skip`] for symmetry with
//! `matrix diff`. Bcond overrides flow through unchanged.

use std::collections::BTreeSet;

use rpm_spec::ast::{Span, SpecFile, Tag, TagValue};
use rpm_spec_profile::ResolvedTargetSet;
use serde::Serialize;

use crate::bcond::BcondOverrides;
use crate::branch_aware::{IndeterminatePolicy, ProfileBranchSelection, walk_active_preamble};
use crate::branch_coverage::CoverageReport;
use crate::dep_walk::{for_each_dep_atom, render_text_with_macros};

/// Tags the impact report folds over. Same set as `matrix classes`
/// and `matrix diff` — keeps the semantic invariant that two
/// profiles equivalent under `matrix classes` see no
/// `matrix diff` deltas AND no `matrix impact` deltas at the same
/// revision pair.
///
/// Public so external consumers can correlate a [`TagImpact::tag_label`]
/// string back to its [`Tag`] enum value (the JSON wire shape uses the
/// label only). Order matches `ProfileImpact::tags` positionally.
pub const COMPARED_TAGS: &[(Tag, &str)] = &[
    (Tag::BuildRequires, "BuildRequires"),
    (Tag::Requires, "Requires"),
];

/// Set-diff between two dep buckets for one (profile, tag) pair.
///
/// Buckets are sorted alphabetically for stable output: a renamed
/// dep surfaces as one `removed` + one `added`, not as a churn.
#[derive(Debug, Clone, Default, Serialize)]
#[non_exhaustive]
pub struct ChangeSet {
    /// Deps present at `to` but not at `from`.
    pub added: Vec<String>,
    /// Deps present at `from` but not at `to`.
    pub removed: Vec<String>,
    /// Deps present on both sides — surfaced so operators can see
    /// "this profile has 12 BR, 2 new and 1 dropped, 9 unchanged"
    /// without re-computing the spec.
    pub unchanged: Vec<String>,
}

impl ChangeSet {
    /// `true` iff this changeset records no movement on either side.
    ///
    /// Intentionally ignores [`Self::unchanged`] — a profile with 12
    /// unchanged deps and 0 added/removed has had no impact even
    /// though `unchanged` is non-empty. The doc-comment is the
    /// contract; the underlying behaviour is pinned by
    /// `has_no_movement_ignores_unchanged` in the unit tests.
    #[must_use]
    pub fn has_no_movement(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// Per-(profile, tag) entry in the impact report.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct TagImpact {
    /// `"BuildRequires"`, `"Requires"`, …
    pub tag_label: &'static str,
    pub changes: ChangeSet,
}

/// Per-profile entry in the impact report.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ProfileImpact {
    pub profile_id: String,
    /// One entry per tag in [`COMPARED_TAGS`], in the same order.
    pub tags: Vec<TagImpact>,
}

impl ProfileImpact {
    /// `true` iff every tag's changeset is empty for this profile.
    /// CLI summaries highlight profiles where this is `false` —
    /// those are the platforms a PR actually moved.
    #[must_use]
    pub fn is_no_change(&self) -> bool {
        self.tags.iter().all(|t| t.changes.has_no_movement())
    }
}

/// Output of [`ImpactReport::compute`].
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ImpactReport {
    /// One row per profile in target-set declaration order, so
    /// renderers preserve column alignment.
    pub per_profile: Vec<ProfileImpact>,
}

impl ImpactReport {
    /// Compute the impact of changing `from_spec` to `to_spec` for
    /// every profile in `target_set`.
    ///
    /// `bcond_overrides` flows through to both `CoverageReport`
    /// computations so the same `--with`/`--without` policy applies
    /// on both sides — otherwise the diff would mix "spec changed"
    /// with "CLI flag changed", which is exactly the noise the
    /// command is meant to eliminate.
    #[must_use]
    pub fn compute(
        from_spec: &SpecFile<Span>,
        to_spec: &SpecFile<Span>,
        target_set: &ResolvedTargetSet,
        bcond_overrides: &BcondOverrides,
    ) -> Self {
        // One CoverageReport per side, shared across profiles. The
        // same selection-policy choice as `matrix diff`: Skip
        // indeterminate so a branch we can't statically resolve
        // contributes nothing to either side — under uncertainty,
        // err toward "no impact reported" rather than spurious
        // adds/removes.
        let from_cov = CoverageReport::compute(from_spec, target_set, bcond_overrides);
        let to_cov = CoverageReport::compute(to_spec, target_set, bcond_overrides);

        let per_profile = target_set
            .targets
            .iter()
            .map(|rt| {
                let from_sel = ProfileBranchSelection::compute(
                    &from_cov,
                    &rt.profile_id,
                    IndeterminatePolicy::Skip,
                );
                let to_sel = ProfileBranchSelection::compute(
                    &to_cov,
                    &rt.profile_id,
                    IndeterminatePolicy::Skip,
                );
                let from_buckets = collect_dep_names(from_spec, &from_sel);
                let to_buckets = collect_dep_names(to_spec, &to_sel);
                let tags = COMPARED_TAGS
                    .iter()
                    .enumerate()
                    .map(|(i, (_, label))| {
                        let f = &from_buckets[i];
                        let t = &to_buckets[i];
                        let added: Vec<String> = t.difference(f).cloned().collect();
                        let removed: Vec<String> = f.difference(t).cloned().collect();
                        let unchanged: Vec<String> = f.intersection(t).cloned().collect();
                        TagImpact {
                            tag_label: label,
                            changes: ChangeSet {
                                added,
                                removed,
                                unchanged,
                            },
                        }
                    })
                    .collect();
                ProfileImpact {
                    profile_id: rt.profile_id.clone(),
                    tags,
                }
            })
            .collect();

        Self { per_profile }
    }

    /// Number of profiles where the impact is non-empty. CLI
    /// summaries lead with this number — "PR affects 3 of 12
    /// profiles" is the most useful one-line headline.
    #[must_use]
    pub fn affected_profile_count(&self) -> usize {
        self.per_profile
            .iter()
            .filter(|p| !p.is_no_change())
            .count()
    }

    /// `true` iff every profile's changeset is empty across every
    /// tag. The PR materially changed nothing the analyzer can see
    /// (under the Skip indeterminate policy).
    #[must_use]
    pub fn is_no_change(&self) -> bool {
        self.per_profile.iter().all(ProfileImpact::is_no_change)
    }
}

/// Walk `spec` once collecting branch-projected dep sets per tag in
/// `COMPARED_TAGS`. Shared with `matrix diff` in spirit; isolated
/// here to avoid a dependency from the `diff` CLI module into the
/// `impact` module.
fn collect_dep_names(
    spec: &SpecFile<Span>,
    selection: &ProfileBranchSelection,
) -> Vec<BTreeSet<String>> {
    let mut buckets: Vec<BTreeSet<String>> =
        COMPARED_TAGS.iter().map(|_| BTreeSet::new()).collect();
    walk_active_preamble(spec, selection, |item| {
        if let Some(idx) = COMPARED_TAGS.iter().position(|(t, _)| t == &item.tag) {
            if let TagValue::Dep(dep) = &item.value {
                for_each_dep_atom(dep, |name| {
                    let rendered = render_text_with_macros(name);
                    let trimmed = rendered.trim();
                    if !trimmed.is_empty() {
                        buckets[idx].insert(trimmed.to_string());
                    }
                });
            }
        }
    });
    buckets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn target_set_with(profiles: &[&str]) -> ResolvedTargetSet {
        use rpm_spec_profile::{ProfileSection, ResolveOptions, TargetEntry, resolve_target_set};
        let section = ProfileSection::new(None, std::collections::BTreeMap::new());
        let target = TargetEntry::from_profiles(profiles.iter().map(|s| s.to_string()).collect());
        resolve_target_set(
            &section,
            "test",
            &target,
            std::path::Path::new("/tmp"),
            ResolveOptions::default(),
        )
        .expect("resolve")
    }

    fn report(from_src: &str, to_src: &str, profiles: &[&str]) -> ImpactReport {
        let from_parsed = parse(from_src);
        let to_parsed = parse(to_src);
        let ts = target_set_with(profiles);
        ImpactReport::compute(
            &from_parsed.spec,
            &to_parsed.spec,
            &ts,
            &BcondOverrides::default(),
        )
    }

    const BASE_SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: make

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn no_change_when_specs_identical() {
        let r = report(BASE_SPEC, BASE_SPEC, &["rhel-9-x86_64"]);
        assert!(r.is_no_change());
        assert_eq!(r.affected_profile_count(), 0);
        // All deps in "unchanged" bucket.
        let br = &r.per_profile[0].tags[0];
        assert_eq!(br.tag_label, "BuildRequires");
        assert!(br.changes.added.is_empty());
        assert!(br.changes.removed.is_empty());
        assert_eq!(br.changes.unchanged, vec!["gcc", "make"]);
    }

    #[test]
    fn has_no_movement_ignores_unchanged() {
        // Pins the doc contract: a ChangeSet with N unchanged deps but
        // 0 added/removed reports `has_no_movement == true`. Without
        // this test a well-meaning "is the changeset empty?" rename
        // would silently flip CI semantics (every PR that touches a
        // spec with stable deps would suddenly look "changed").
        let cs = ChangeSet {
            added: Vec::new(),
            removed: Vec::new(),
            unchanged: vec!["gcc".to_string(), "make".to_string()],
        };
        assert!(
            cs.has_no_movement(),
            "unchanged-only set must not register as movement"
        );
    }

    #[test]
    fn added_dep_surfaces_per_profile() {
        const TO: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: make
BuildRequires: cmake

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report(BASE_SPEC, TO, &["rhel-9-x86_64"]);
        assert!(!r.is_no_change());
        assert_eq!(r.affected_profile_count(), 1);
        let br = &r.per_profile[0].tags[0];
        assert_eq!(br.changes.added, vec!["cmake"]);
        assert!(br.changes.removed.is_empty());
    }

    #[test]
    fn removed_dep_surfaces_per_profile() {
        const TO: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report(BASE_SPEC, TO, &["rhel-9-x86_64"]);
        let br = &r.per_profile[0].tags[0];
        assert_eq!(br.changes.removed, vec!["make"]);
        assert!(br.changes.added.is_empty());
    }

    #[test]
    fn rhel_only_branch_split_affects_only_rhel() {
        // Add a RHEL-gated BR in the new revision. Affects only
        // rhel-* profiles; altlinux sees no change.
        const TO: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: make

%if 0%{?rhel}
BuildRequires: rhel-only
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report(BASE_SPEC, TO, &["rhel-9-x86_64", "altlinux-10-x86_64"]);
        // rhel-9-x86_64: rhel-only added.
        let rhel = r
            .per_profile
            .iter()
            .find(|p| p.profile_id == "rhel-9-x86_64")
            .expect("rhel profile present");
        assert!(!rhel.is_no_change());
        assert!(
            rhel.tags[0]
                .changes
                .added
                .contains(&"rhel-only".to_string())
        );
        // altlinux-10-x86_64: unaffected (the new BR is gated away).
        let alt = r
            .per_profile
            .iter()
            .find(|p| p.profile_id == "altlinux-10-x86_64")
            .expect("alt profile present");
        assert!(alt.is_no_change(), "alt must be unaffected; got {alt:?}");
        // Reported count is 1 of 2 profiles affected.
        assert_eq!(r.affected_profile_count(), 1);
    }

    #[test]
    fn empty_from_spec_reports_all_as_added() {
        // Edge: from-side is a spec that declares no deps (e.g.
        // the file was just added). Everything in to-spec is
        // "added" relative to a clean slate.
        const EMPTY: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report(EMPTY, BASE_SPEC, &["rhel-9-x86_64"]);
        let br = &r.per_profile[0].tags[0];
        assert_eq!(br.changes.added, vec!["gcc", "make"]);
        assert!(br.changes.removed.is_empty());
        assert!(br.changes.unchanged.is_empty());
    }

    #[test]
    fn affected_profile_count_excludes_unchanged_profiles() {
        // 3 profiles, only 1 has any change.
        const TO: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: make
%if 0%{?suse_version}
BuildRequires: suse-only
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report(
            BASE_SPEC,
            TO,
            &["rhel-9-x86_64", "altlinux-10-x86_64", "sles-15-x86_64"],
        );
        // Only sles-15-x86_64 should see suse-only added.
        assert_eq!(r.affected_profile_count(), 1);
    }
}
