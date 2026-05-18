//! Strongly-typed key for dependency-carrying preamble tags.
//!
//! Several rules bucket dependency atoms by the tag they appeared on
//! (RPM320/RPM321/RPM450/RPM451/RPM452/RPM453/RPM454/RPM574/RPM590/
//! RPM596/RPM597 and the `hardcoded_paths` / `macro_composition_*`
//! "safe-tag" probes). This enum centralises the bucket definition,
//! gives `match`-exhaustiveness, and makes the keys
//! `Copy + Eq + Hash + Ord`.

use rpm_spec::ast::Tag;

/// Strongly-typed key identifying a dependency-carrying preamble tag.
///
/// Several rules bucket dependency atoms by the tag they appeared on
/// (RPM320/RPM321/RPM450/RPM451/RPM452/RPM453/RPM454/RPM574/RPM590/
/// RPM596/RPM597 and the `hardcoded_paths` / `macro_composition_*`
/// "safe-tag" probes). Earlier versions inlined each tag-class match
/// at the call site, which was stringly-typed and easy to misspell;
/// this enum makes the buckets `Copy + Eq + Hash + Ord`, gives
/// `match`-exhaustiveness, and centralises the `Tag -> label` mapping
/// so a future rename (or new variant) is a single-line change.
///
/// [`label`] returns the same canonical string that the legacy
/// `tag_key` helpers used to return, so diagnostic messages render
/// unchanged.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub(crate) enum DepTagKey {
    Requires,
    BuildRequires,
    Provides,
    Conflicts,
    Obsoletes,
    Recommends,
    Suggests,
    Supplements,
    Enhances,
    BuildConflicts,
    OrderWithRequires,
}

impl DepTagKey {
    /// Every variant in declaration order. Table-driven consumers
    /// (e.g. RPM320's per-tag-class scan) iterate this slice instead
    /// of hand-rolling a `&[(fn, &str)]` table.
    pub(crate) const ALL: &'static [DepTagKey] = &[
        DepTagKey::Requires,
        DepTagKey::BuildRequires,
        DepTagKey::Provides,
        DepTagKey::Conflicts,
        DepTagKey::Obsoletes,
        DepTagKey::Recommends,
        DepTagKey::Suggests,
        DepTagKey::Supplements,
        DepTagKey::Enhances,
        DepTagKey::BuildConflicts,
        DepTagKey::OrderWithRequires,
    ];

    /// Canonical label as written in a spec file. Used both as the
    /// rendering for diagnostic messages and as a stable key for
    /// `BTreeMap` ordering across rule runs.
    pub(crate) fn label(self) -> &'static str {
        match self {
            DepTagKey::Requires => "Requires",
            DepTagKey::BuildRequires => "BuildRequires",
            DepTagKey::Provides => "Provides",
            DepTagKey::Conflicts => "Conflicts",
            DepTagKey::Obsoletes => "Obsoletes",
            DepTagKey::Recommends => "Recommends",
            DepTagKey::Suggests => "Suggests",
            DepTagKey::Supplements => "Supplements",
            DepTagKey::Enhances => "Enhances",
            DepTagKey::BuildConflicts => "BuildConflicts",
            DepTagKey::OrderWithRequires => "OrderWithRequires",
        }
    }

    /// Map a parsed [`Tag`] to its [`DepTagKey`]. Returns `None` for
    /// any tag that isn't a dependency-carrying preamble tag (Name,
    /// Version, Summary, ...).
    pub(crate) fn from_tag(t: &rpm_spec::ast::Tag) -> Option<Self> {
        match t {
            Tag::Requires => Some(DepTagKey::Requires),
            Tag::BuildRequires => Some(DepTagKey::BuildRequires),
            Tag::Provides => Some(DepTagKey::Provides),
            Tag::Conflicts => Some(DepTagKey::Conflicts),
            Tag::Obsoletes => Some(DepTagKey::Obsoletes),
            Tag::Recommends => Some(DepTagKey::Recommends),
            Tag::Suggests => Some(DepTagKey::Suggests),
            Tag::Supplements => Some(DepTagKey::Supplements),
            Tag::Enhances => Some(DepTagKey::Enhances),
            Tag::BuildConflicts => Some(DepTagKey::BuildConflicts),
            Tag::OrderWithRequires => Some(DepTagKey::OrderWithRequires),
            _ => None,
        }
    }

    /// `true` when `t` is the [`Tag`] variant this key represents.
    /// Provided so consumers can drop the hand-rolled
    /// `fn(&Tag) -> bool` matcher closures.
    pub(crate) fn matches_tag(self, t: &rpm_spec::ast::Tag) -> bool {
        Self::from_tag(t) == Some(self)
    }

    /// Canonical clustering priority used by RPM574
    /// (`preamble-tag-clustering`). Lower numbers come first in the
    /// canonical spec layout:
    ///
    /// 1. identity / metadata (`Name`, `Version`, `License`, …)
    /// 2. sources / patches (`Source*`, `Patch*`, `URL`, `Icon`)
    /// 3. build deps (`BuildRequires`, `BuildConflicts`)
    /// 4. runtime deps + ordering (`Requires`, `Recommends`, …,
    ///    `OrderWithRequires`)
    ///
    /// Variants that don't carry a priority (none exist among the
    /// current dep-tag set) would return `0` — but every current
    /// variant maps to a real bucket, so the table is exhaustive.
    pub(crate) fn cluster_priority(self) -> u8 {
        match self {
            DepTagKey::BuildRequires | DepTagKey::BuildConflicts => 3,
            DepTagKey::Requires
            | DepTagKey::Provides
            | DepTagKey::Conflicts
            | DepTagKey::Obsoletes
            | DepTagKey::Recommends
            | DepTagKey::Suggests
            | DepTagKey::Supplements
            | DepTagKey::Enhances
            | DepTagKey::OrderWithRequires => 4,
        }
    }

    /// Same as [`Self::from_tag`] but rejects "strong" deps — those
    /// whose presence/absence affects the package metadata rather than
    /// what's pulled into the install set: `Provides`, `Conflicts`,
    /// `Obsoletes`. Used by [`crate::rules::guarded_dependency_subsumption`]
    /// (RPM597), which only reasons about runtime/build pulls.
    pub(crate) fn from_tag_weak_only(t: &rpm_spec::ast::Tag) -> Option<Self> {
        match Self::from_tag(t)? {
            DepTagKey::Provides | DepTagKey::Conflicts | DepTagKey::Obsoletes => None,
            other => Some(other),
        }
    }

    /// Same as [`Self::from_tag`] but rejects `Provides` and
    /// `Obsoletes`. RPM treats the unversioned-vs-versioned axis on
    /// those two tags with different semantics from `Requires`-family
    /// tags (a `Provides:` is a *claim*, not a *pull*), so the
    /// constraint-subsumption rule (RPM596) excludes them while still
    /// looking at `Conflicts:`.
    pub(crate) fn from_tag_no_provides_obsoletes(t: &rpm_spec::ast::Tag) -> Option<Self> {
        match Self::from_tag(t)? {
            DepTagKey::Provides | DepTagKey::Obsoletes => None,
            other => Some(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dep_tag_key_label_round_trips() {
        // Sanity-check that every variant renders to the same string
        // that the legacy `tag_key()` helpers returned.
        assert_eq!(DepTagKey::Requires.label(), "Requires");
        assert_eq!(DepTagKey::BuildRequires.label(), "BuildRequires");
        assert_eq!(DepTagKey::Provides.label(), "Provides");
        assert_eq!(DepTagKey::Conflicts.label(), "Conflicts");
        assert_eq!(DepTagKey::Obsoletes.label(), "Obsoletes");
        assert_eq!(DepTagKey::Recommends.label(), "Recommends");
        assert_eq!(DepTagKey::Suggests.label(), "Suggests");
        assert_eq!(DepTagKey::Supplements.label(), "Supplements");
        assert_eq!(DepTagKey::Enhances.label(), "Enhances");
    }

    #[test]
    fn dep_tag_key_excludes_strong_deps_in_weak_only() {
        // `from_tag` accepts Provides; `from_tag_weak_only` rejects it.
        assert_eq!(
            DepTagKey::from_tag(&Tag::Provides),
            Some(DepTagKey::Provides)
        );
        assert_eq!(DepTagKey::from_tag_weak_only(&Tag::Provides), None);
        // Same for Conflicts and Obsoletes.
        assert_eq!(
            DepTagKey::from_tag(&Tag::Conflicts),
            Some(DepTagKey::Conflicts)
        );
        assert_eq!(DepTagKey::from_tag_weak_only(&Tag::Conflicts), None);
        assert_eq!(
            DepTagKey::from_tag(&Tag::Obsoletes),
            Some(DepTagKey::Obsoletes)
        );
        assert_eq!(DepTagKey::from_tag_weak_only(&Tag::Obsoletes), None);
        // Weak-only still accepts Requires/Recommends/etc.
        assert_eq!(
            DepTagKey::from_tag_weak_only(&Tag::Requires),
            Some(DepTagKey::Requires),
        );
        assert_eq!(
            DepTagKey::from_tag_weak_only(&Tag::Recommends),
            Some(DepTagKey::Recommends),
        );
    }

    #[test]
    fn dep_tag_key_no_provides_obsoletes_keeps_conflicts() {
        // RPM596's policy: drop Provides + Obsoletes, KEEP Conflicts.
        assert_eq!(
            DepTagKey::from_tag_no_provides_obsoletes(&Tag::Provides),
            None
        );
        assert_eq!(
            DepTagKey::from_tag_no_provides_obsoletes(&Tag::Obsoletes),
            None
        );
        assert_eq!(
            DepTagKey::from_tag_no_provides_obsoletes(&Tag::Conflicts),
            Some(DepTagKey::Conflicts),
        );
        assert_eq!(
            DepTagKey::from_tag_no_provides_obsoletes(&Tag::Requires),
            Some(DepTagKey::Requires),
        );
    }

    #[test]
    fn dep_tag_key_all_includes_build_conflicts_and_order_with_requires() {
        // `ALL` must enumerate the 11 dep-tag variants — earlier
        // versions exposed only 9 and four consumer files had to
        // inline-match the missing two.
        assert!(DepTagKey::ALL.contains(&DepTagKey::BuildConflicts));
        assert!(DepTagKey::ALL.contains(&DepTagKey::OrderWithRequires));
        assert_eq!(DepTagKey::ALL.len(), 11);
        // Every variant's label round-trips through `from_tag` so the
        // table is internally consistent (no typoed labels).
        for key in DepTagKey::ALL {
            assert!(!key.label().is_empty());
        }
    }

    #[test]
    fn dep_tag_key_matches_tag_round_trip() {
        // For every variant, `matches_tag` is `true` on the
        // corresponding `Tag` and `false` on a sibling — so consumers
        // can swap their hand-rolled `fn(&Tag) -> bool` matchers for
        // `key.matches_tag(t)` without changing semantics.
        assert!(DepTagKey::Requires.matches_tag(&Tag::Requires));
        assert!(!DepTagKey::Requires.matches_tag(&Tag::BuildRequires));
        assert!(DepTagKey::BuildConflicts.matches_tag(&Tag::BuildConflicts));
        assert!(!DepTagKey::BuildConflicts.matches_tag(&Tag::Conflicts));
        assert!(DepTagKey::OrderWithRequires.matches_tag(&Tag::OrderWithRequires));
        assert!(!DepTagKey::OrderWithRequires.matches_tag(&Tag::Requires));
        // Non-dep tag → no key matches.
        for key in DepTagKey::ALL {
            assert!(!key.matches_tag(&Tag::Name));
        }
    }
}
