//! Equivalence classes of profiles by effective dependency footprint.
//!
//! For a release matrix of N profiles many will produce an identical
//! set of `BuildRequires` and `Requires` once branch resolution is
//! done — the differences live entirely inside `%if` bodies that all
//! evaluate the same way on a sub-cluster. Release engineers want to
//! know: "is this 30-profile matrix really 30 distinct builds, or 5
//! distinct builds replicated across 30 arch×distro tuples?"
//!
//! Phase 11 surfaces this via `matrix classes`: every profile is
//! reduced to a [`ProfileSignature`] (sorted, branch-aware
//! BuildRequires + Requires set), profiles with identical signatures
//! collapse into one [`EquivalenceClass`], and the report yields a
//! "minimal representative build set" — one profile per class —
//! suitable for CI gating.
//!
//! Scope of the signature is intentionally narrow: just dependency
//! atoms (BR + Requires), reusing [`crate::dep_walk`] and
//! [`crate::branch_aware`]. Files / Provides / Conflicts /
//! Obsoletes are deferred — the artifact comparator (a future
//! phase) is the right place for post-build equivalence. Including
//! them here would silently fragment classes on differences operators
//! don't care about for "do I need to run this build?".
//!
//! ## Indeterminate policy
//!
//! The walker uses [`IndeterminatePolicy::Skip`] so a branch the
//! evaluator can't resolve on profile X is excluded from X's
//! signature on both sides. Two profiles where the same dep is
//! gated by an undecidable `%if` will share a signature (both saw
//! "no dep here"), which is the right outcome for "minimal build
//! set" — under uncertainty we err toward fewer distinct classes.

use std::collections::{BTreeMap, BTreeSet};

use rpm_spec::ast::{Span, SpecFile, Tag, TagValue};
use rpm_spec_profile::ResolvedTargetSet;
use serde::Serialize;

use crate::bcond::BcondOverrides;
use crate::branch_aware::{IndeterminatePolicy, ProfileBranchSelection, walk_active_preamble};
use crate::branch_coverage::CoverageReport;
use crate::dep_walk::{for_each_dep_atom, render_text_with_macros};

/// Tags the signature folds over, paired with their display labels.
/// Restricted to dep-bearing preamble entries that materially affect
/// "what gets installed on the build host or the target system".
/// Same tags as `matrix diff` — symmetric semantics: two profiles
/// equivalent here will also produce identical `matrix diff` buckets
/// (both `only_a` / `only_b` empty).
///
/// The pair shape keeps the tag → label mapping in one place; the
/// old design had a separate `tag_label()` match that was easy to
/// fall out of sync with the const.
const SIGNATURE_TAGS: &[(Tag, &str)] = &[
    (Tag::BuildRequires, "BuildRequires"),
    (Tag::Requires, "Requires"),
];

/// Sorted, branch-resolved dependency footprint of one profile.
///
/// `tag_buckets[i]` holds the deps for `SIGNATURE_TAGS[i]`. Buckets
/// are `BTreeSet<String>` for two reasons: order independence
/// (equality across runs needs sorted comparison anyway) and O(log n)
/// membership for `dep_walk::for_each_dep_atom`'s insertion.
///
/// The serialised form is a stable hex hash so JSON output stays
/// compact and diffable, while **in-memory equality is structural**:
/// `ClassesReport::compute` keys profile groups on the full
/// `ProfileSignature` rather than the hash. Hash collisions therefore
/// cannot silently merge distinct dep sets into one class — they
/// can only cause two genuinely-equivalent groups to collide on
/// the diagnostic hex string, which is harmless.
///
/// `Ord` is derived strictly as a `BTreeMap` key mechanism; the
/// induced order has no semantic meaning — do not rely on it for
/// presentation or comparison logic.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProfileSignature {
    /// Same order as `SIGNATURE_TAGS`; sorted within each bucket.
    pub tag_buckets: Vec<BTreeSet<String>>,
}

impl ProfileSignature {
    /// Compute the signature for `profile_id` against `spec`. The
    /// `coverage` argument is precomputed once per matrix run and
    /// shared across profiles — building one CoverageReport per
    /// profile would double the per-spec cost.
    #[must_use]
    pub fn compute(
        spec: &SpecFile<Span>,
        coverage: &CoverageReport,
        profile_id: &str,
    ) -> Self {
        let sel = ProfileBranchSelection::compute(
            coverage,
            profile_id,
            IndeterminatePolicy::Skip,
        );
        let mut buckets: Vec<BTreeSet<String>> =
            SIGNATURE_TAGS.iter().map(|_| BTreeSet::new()).collect();
        walk_active_preamble(spec, &sel, |item| {
            if let Some(idx) = SIGNATURE_TAGS
                .iter()
                .position(|(t, _)| t == &item.tag)
            {
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
        Self { tag_buckets: buckets }
    }

    /// Diagnostic 64-bit hash used by [`Self::hash_hex`]. Crate-internal
    /// because class membership is structural (via `Ord` on the
    /// signature), not hash-based — exposing the raw `u64` would
    /// invite downstream code to key persistent baselines on it and
    /// inherit the stdlib's SipHash-stability caveat. The hex
    /// `hash_hex()` form remains public for human/JSON diagnostics.
    #[must_use]
    pub(crate) fn hash(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for bucket in &self.tag_buckets {
            bucket.len().hash(&mut h);
            for dep in bucket {
                dep.hash(&mut h);
            }
        }
        h.finish()
    }

    /// Render the hash as a fixed-width hex string for diagnostic
    /// output. 16 lowercase hex chars (16-byte u64 in hex).
    #[must_use]
    pub fn hash_hex(&self) -> String {
        format!("{:016x}", self.hash())
    }
}

// ---------------------------------------------------------------------------
// ClassesReport
// ---------------------------------------------------------------------------

/// One equivalence class: every member profile produces the same
/// [`ProfileSignature`].
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct EquivalenceClass {
    /// Hex signature shared by every member.
    pub signature: String,
    /// Profile IDs in this class, sorted alphabetically. The first
    /// element is also stored separately as `representative` for
    /// convenience.
    pub members: Vec<String>,
    /// One profile from this class chosen as the representative
    /// build target. Stable across runs (first alphabetical).
    pub representative: String,
    /// Per-tag dep sets, sorted alphabetically. Ordering matches
    /// [`SIGNATURE_TAGS`]. Mostly diagnostic — the signature hash
    /// is what classes group on.
    pub deps_by_tag: Vec<DepBucket>,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct DepBucket {
    /// Display label of the tag (`"BuildRequires"`, `"Requires"`).
    /// In-memory field name matches the analyzer's `TagDiff` type
    /// in `matrix diff` for code consistency; on the JSON wire the
    /// field is serialised as `"tag"` to match `matrix diff`'s
    /// published shape (`TagDiffJson.tag`).
    #[serde(rename = "tag")]
    pub tag_label: &'static str,
    /// Sorted dep atoms.
    pub deps: Vec<String>,
}

/// Result of [`ClassesReport::compute`].
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ClassesReport {
    /// Equivalence classes sorted by descending member count, ties
    /// broken by representative ID. Stable across runs.
    pub classes: Vec<EquivalenceClass>,
}

impl ClassesReport {
    /// Compute the equivalence classes of every profile in
    /// `target_set` against `spec`. `bcond_overrides` flows through
    /// to the underlying [`CoverageReport`] so `--with FOO` honours
    /// the same semantics as `matrix coverage`.
    #[must_use]
    pub fn compute(
        spec: &SpecFile<Span>,
        target_set: &ResolvedTargetSet,
        bcond_overrides: &BcondOverrides,
    ) -> Self {
        let coverage = CoverageReport::compute(spec, target_set, bcond_overrides);

        // Group profile IDs by the FULL signature, not its u64 hash.
        // Keying on the structural value eliminates the silent
        // class-merge risk that a hash-keyed map would carry: two
        // distinct dep sets colliding on SipHash would otherwise be
        // bucketed together with the surviving `tag_buckets` from
        // whichever profile inserted first. Cost is `O(log N · |sig|)`
        // per insert; for N ≤ 30 and tens of deps per signature this
        // is sub-microsecond.
        let mut groups: BTreeMap<ProfileSignature, Vec<String>> = BTreeMap::new();
        for rt in &target_set.targets {
            let sig = ProfileSignature::compute(spec, &coverage, &rt.profile_id);
            groups
                .entry(sig)
                .and_modify(|members| members.push(rt.profile_id.clone()))
                .or_insert_with(|| vec![rt.profile_id.clone()]);
        }

        let mut classes: Vec<EquivalenceClass> = groups
            .into_iter()
            .map(|(sig, mut members)| {
                members.sort();
                let representative = members[0].clone();
                let signature = sig.hash_hex();
                // Consume the signature's buckets into the report —
                // they're no longer needed for grouping.
                let deps_by_tag = SIGNATURE_TAGS
                    .iter()
                    .zip(sig.tag_buckets)
                    .map(|((_tag, label), bucket)| DepBucket {
                        tag_label: label,
                        deps: bucket.into_iter().collect(),
                    })
                    .collect();
                EquivalenceClass {
                    signature,
                    members,
                    representative,
                    deps_by_tag,
                }
            })
            .collect();

        // Sort classes by descending member count, ties broken by
        // representative ID for stable output. Large classes first
        // surfaces "this is mostly one cluster with a few outliers"
        // at a glance.
        classes.sort_by(|a, b| {
            b.members.len()
                .cmp(&a.members.len())
                .then_with(|| a.representative.cmp(&b.representative))
        });

        Self { classes }
    }

    /// Number of distinct classes. The "minimal representative build
    /// set" has exactly this size.
    #[must_use]
    pub fn class_count(&self) -> usize {
        self.classes.len()
    }

    /// One representative per class, in the same order as `classes`.
    /// CI gating systems use this to drive their build matrix:
    /// instead of N full builds, run `representatives()`-many.
    pub fn representatives(&self) -> impl Iterator<Item = &str> {
        self.classes.iter().map(|c| c.representative.as_str())
    }
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

    fn report_for(spec_src: &str, profiles: &[&str]) -> ClassesReport {
        let parsed = parse(spec_src);
        let ts = target_set_with(profiles);
        ClassesReport::compute(&parsed.spec, &ts, &BcondOverrides::default())
    }

    const SHARED_SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: make
Requires: glibc

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn identical_profiles_collapse_into_one_class() {
        // No conditionals — every profile has the same BR+Requires.
        // 3 profiles → 1 class.
        let r = report_for(SHARED_SPEC, &["rhel-9-x86_64", "altlinux-10-x86_64", "sles-15-x86_64"]);
        assert_eq!(r.class_count(), 1, "no conditionals → all profiles equivalent");
        assert_eq!(r.classes[0].members.len(), 3);
    }

    #[test]
    fn rhel_gated_dep_splits_classes() {
        const SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
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
        // rhel-9 has gcc+rhel-only; alt has gcc only → 2 classes.
        let r = report_for(SPEC, &["rhel-9-x86_64", "altlinux-10-x86_64"]);
        assert_eq!(r.class_count(), 2);
        // Larger-or-equal class comes first; same size → alphabetical.
        // Both classes have 1 member, sorted by representative id:
        // altlinux-10-x86_64 before rhel-9-x86_64.
        assert_eq!(r.classes[0].representative, "altlinux-10-x86_64");
        assert_eq!(r.classes[1].representative, "rhel-9-x86_64");
    }

    #[test]
    fn class_signature_is_stable_across_runs() {
        // Two independent computations must produce identical
        // signatures and representatives.
        let r1 = report_for(SHARED_SPEC, &["rhel-9-x86_64", "altlinux-10-x86_64"]);
        let r2 = report_for(SHARED_SPEC, &["rhel-9-x86_64", "altlinux-10-x86_64"]);
        assert_eq!(r1.classes.len(), r2.classes.len());
        for (a, b) in r1.classes.iter().zip(r2.classes.iter()) {
            assert_eq!(a.signature, b.signature);
            assert_eq!(a.representative, b.representative);
        }
    }

    #[test]
    fn classes_sort_by_descending_member_count() {
        // Same RHEL-only conditional, 3 RHEL-family profiles + 1 alt.
        const SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
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
        let r = report_for(
            SPEC,
            &["rhel-9-x86_64", "rhel-9-aarch64", "rhel-8-x86_64", "altlinux-10-x86_64"],
        );
        // RHEL class has 3 members; alt class has 1. RHEL class
        // sorts first by descending count.
        assert_eq!(r.classes[0].members.len(), 3);
        assert_eq!(r.classes[1].members.len(), 1);
        // Representative of the 3-member RHEL class is the first
        // alphabetical RHEL id (rhel-8-x86_64).
        assert_eq!(r.classes[0].representative, "rhel-8-x86_64");
    }

    #[test]
    fn representatives_iterates_one_per_class() {
        let r = report_for(SHARED_SPEC, &["rhel-9-x86_64", "altlinux-10-x86_64"]);
        let reps: Vec<&str> = r.representatives().collect();
        assert_eq!(reps.len(), r.class_count());
    }

    #[test]
    fn deps_by_tag_preserves_signature_tag_order() {
        let r = report_for(SHARED_SPEC, &["rhel-9-x86_64"]);
        let class = &r.classes[0];
        assert_eq!(class.deps_by_tag.len(), 2);
        // SIGNATURE_TAGS is BuildRequires then Requires.
        assert_eq!(class.deps_by_tag[0].tag_label, "BuildRequires");
        assert_eq!(class.deps_by_tag[1].tag_label, "Requires");
        assert_eq!(class.deps_by_tag[0].deps, vec!["gcc", "make"]);
        assert_eq!(class.deps_by_tag[1].deps, vec!["glibc"]);
    }

    #[test]
    fn signature_equality_via_different_paths() {
        // The path-independence claim: two profiles with the same
        // effective dep set must produce the SAME signature even when
        // one declares deps unconditionally and another inside an
        // active `%if`. Without this, the doc's claim that
        // `matrix classes` and `matrix diff` agree on equivalence
        // would be wrong.
        const SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
%if 0%{?rhel}
BuildRequires: gcc
BuildRequires: make
%else
BuildRequires: gcc
BuildRequires: make
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        // Both branches of %if/%else contribute the same BR set →
        // rhel-9 and altlinux-10 must end up with the same signature.
        let parsed = parse(SPEC);
        let ts = target_set_with(&["rhel-9-x86_64", "altlinux-10-x86_64"]);
        let coverage = CoverageReport::compute(&parsed.spec, &ts, &BcondOverrides::default());
        let sig_rhel = ProfileSignature::compute(&parsed.spec, &coverage, "rhel-9-x86_64");
        let sig_alt = ProfileSignature::compute(&parsed.spec, &coverage, "altlinux-10-x86_64");
        assert_eq!(sig_rhel, sig_alt, "same dep set via different paths must hash equal");
        assert_eq!(sig_rhel.hash(), sig_alt.hash());
    }

    #[test]
    fn else_branch_classes_pin_correct_dep_contents() {
        // Reciprocal of `rhel_gated_dep_splits_classes`: not just
        // count but actual contents. Catches a regression where the
        // walker visits one arm of `%else` but the wrong one.
        const SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
%if 0%{?rhel}
BuildRequires: rhel-pkg
%else
BuildRequires: non-rhel-pkg
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report_for(SPEC, &["rhel-9-x86_64", "altlinux-10-x86_64"]);
        assert_eq!(r.class_count(), 2);
        // Look up each class by its representative and confirm the
        // BR bucket carries the right gating dep.
        let alt = r.classes.iter().find(|c| c.representative == "altlinux-10-x86_64")
            .expect("alt class present");
        let rhel = r.classes.iter().find(|c| c.representative == "rhel-9-x86_64")
            .expect("rhel class present");
        // BR is the first SIGNATURE_TAGS entry.
        assert_eq!(alt.deps_by_tag[0].deps, vec!["non-rhel-pkg"]);
        assert_eq!(rhel.deps_by_tag[0].deps, vec!["rhel-pkg"]);
    }

    #[test]
    fn class_count_matches_distinct_signatures() {
        // Two pairs collapsing into two classes: rhel-family share
        // BR via `%if 0%{?rhel}`, alt-family share via NOT being rhel.
        const SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
%if 0%{?rhel}
BuildRequires: rhel-pkg
%else
BuildRequires: non-rhel-pkg
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let r = report_for(
            SPEC,
            &["rhel-9-x86_64", "rhel-8-x86_64", "altlinux-10-x86_64", "sles-15-x86_64"],
        );
        assert_eq!(r.class_count(), 2);
    }
}
