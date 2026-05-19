//! Shared capability-matching predicates.
//!
//! Both [`crate::lookup`] (single-shot queries) and [`crate::solver`]
//! (closure walker) need to answer "does this `Package` satisfy a
//! requirement `Capability`?" with identical semantics. Keeping the
//! predicate in one place prevents the two consumers from drifting
//! (e.g. one using `compare_rpm`, the other `cmp` — both correct
//! today since EVR's `Ord` delegates to `compare_rpm`, but a single
//! source of truth removes the trap entirely).

use std::cmp::Ordering;

use rpm_spec_repo_core::{
    CapFlags, Capability, Dependency, EVR, NEVRA, Package, ProviderRef, RepoError, RepoUniverse,
};

/// Priority assigned to repos not present in the priority map (e.g. when
/// the universe was rebuilt after the map was constructed). Sorted to the
/// bottom of the candidate list.
pub(crate) const REPO_PRIORITY_FALLBACK: i32 = 99;

/// True if `provider` satisfies the (`name`, `flags`, `evr`) shape of
/// `requirement`. Matches dnf semantics:
///
/// * direct package-name match → check EVR;
/// * file-path requirement (`name` starts with `/`) → accept iff the
///   provider's filelist contains the path; rpm forbids version
///   constraints on file deps so we ignore `flags`/`evr` for this
///   branch (matches dnf which never publishes file deps as
///   synthetic versioned provides);
/// * `Provides:` name match → if the requirement is versionless,
///   accept; otherwise both sides must carry an EVR (a versionless
///   `Provides:` does NOT satisfy a versioned requirement).
pub fn provides_satisfies(provider: &Package, requirement: &Capability) -> bool {
    if provider.nevra.name.as_ref() == requirement.name.as_ref() {
        return evr_matches(&provider.nevra.evr(), requirement);
    }
    if requirement.is_file_path() {
        for path in &provider.files {
            if path.as_ref() == requirement.name.as_ref() {
                return true;
            }
        }
    }
    for prov in &provider.provides {
        if prov.name.as_ref() != requirement.name.as_ref() {
            continue;
        }
        // No version constraint on the requirement → name match suffices.
        if requirement.flags == CapFlags::None {
            return true;
        }
        // Versioned requirement: a versionless `Provides:` cannot
        // satisfy it (the package didn't declare a version, so we
        // can't compare). Move on to the next provider entry.
        if let (Some(prov_evr), Some(_)) = (&prov.evr, &requirement.evr)
            && evr_matches(prov_evr, requirement)
        {
            return true;
        }
    }
    false
}

/// Check whether `provider_evr` satisfies the EVR constraint encoded
/// in `req`. A malformed `req` carrying a versioned `flags` but no
/// EVR is treated as unmet (rather than silently accepted) — see the
/// note in solver.rs for the rationale.
pub fn evr_matches(provider_evr: &EVR, req: &Capability) -> bool {
    if req.flags == CapFlags::None {
        return true;
    }
    let Some(req_evr) = req.evr.as_ref() else {
        // Defensive: `flags=EQ` with `evr=None` is unsatisfiable.
        return false;
    };
    // EVR's `Ord` impl delegates to `compare_rpm` (rpmvercmp), so
    // calling `cmp` here is equivalent and keeps the predicate readable.
    matches_flag(provider_evr.cmp(req_evr), req.flags)
}

/// Map a three-way comparison result onto an RPM `CapFlags` predicate.
///
/// Thin alias for [`CapFlags::matches`] kept on this side for the
/// resolver's existing `matches_flag(cmp, flag)` call shape (the
/// canonical impl lives in `repo-core` so the DB-side `satisfies`
/// path can't drift from this one).
pub fn matches_flag(cmp: Ordering, flag: CapFlags) -> bool {
    flag.matches(cmp)
}

/// Gather every candidate that might satisfy `dep` across the
/// universe, plus a flag for "this is a file-path dep". Shared by
/// [`crate::lookup`] (single-shot) and [`crate::solver::pick_provider`]
/// (closure walk) so the candidate-set definition can't diverge — a
/// real bug class on previous iterations, e.g. when one consumer
/// remembered the file-owner fallback and the other didn't.
///
/// The order is: every package whose `name` or `Provides:` matches
/// `dep.name` (one indexed SQL roundtrip per repo via
/// `candidates_with_nevra`), with the optional file-owner appended
/// for file-path deps. Duplicates are filtered against
/// [`ProviderRef`] equality.
///
/// # Errors
///
/// Returns the per-repo DB error verbatim; both callers funnel it
/// straight up so lint code can surface it once and skip the rule.
pub(crate) fn gather_candidates(
    universe: &RepoUniverse,
    dep: &Dependency,
) -> Result<(Vec<(ProviderRef, NEVRA)>, bool), RepoError> {
    let file_path_dep = dep.is_file_path();
    let mut candidates = universe.candidates_with_nevra(&dep.name)?;
    if file_path_dep
        && let Some(owner) = universe.file_owner(&dep.name)?
        && !candidates.iter().any(|(p, _)| p == &owner)
        && let Some(nevra) = universe.resolve_nevra(&owner)?
    {
        candidates.push((owner, nevra));
    }
    Ok((candidates, file_path_dep))
}

#[cfg(test)]
mod tests {
    //! Unit coverage for the EVR / Provides matching semantics. These
    //! predicates are the single source of truth for both [`crate::lookup`]
    //! and [`crate::solver`], so the suite exercises each branch (own-name
    //! vs Provides, versioned vs versionless, every `CapFlags` variant)
    //! to lock the contract before either consumer drifts.
    use std::sync::Arc;

    use rpm_spec_repo_core::{CapFlags, Capability, NEVRA, Package, PkgChecksum};

    use super::*;

    fn nevra(name: &str, version: &str, release: &str) -> NEVRA {
        NEVRA {
            name: Arc::from(name),
            epoch: 0,
            version: Arc::from(version),
            release: Arc::from(release),
            arch: Arc::from("x86_64"),
        }
    }

    fn pkg(name: &str, version: &str, release: &str, provides: Vec<Capability>) -> Package {
        Package {
            nevra: nevra(name, version, release),
            repo_id: Arc::from("test-repo"),
            provides,
            requires: Vec::new(),
            conflicts: Vec::new(),
            obsoletes: Vec::new(),
            recommends: Vec::new(),
            suggests: Vec::new(),
            supplements: Vec::new(),
            enhances: Vec::new(),
            source_rpm: None,
            summary: Arc::from(""),
            size_installed: 0,
            checksum: PkgChecksum::Sha256(String::from("deadbeef")),
            location: Arc::from(""),
            files: Vec::new(),
        }
    }

    fn cap_unversioned(name: &str) -> Capability {
        Capability {
            name: Arc::from(name),
            flags: CapFlags::None,
            evr: None,
        }
    }

    fn cap_versioned(name: &str, flags: CapFlags, version: &str, release: &str) -> Capability {
        Capability {
            name: Arc::from(name),
            flags,
            evr: Some(EVR::new(Some(0), version, release)),
        }
    }

    #[test]
    fn provides_satisfies_direct_name_match_no_flags() {
        // Direct package-name hit with a versionless requirement.
        let p = pkg("bash", "5.1.8", "9", vec![]);
        let req = cap_unversioned("bash");
        assert!(provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_satisfies_via_provides_versionless() {
        // `/bin/sh` listed as a Provides on bash.
        let p = pkg("bash", "5.1.8", "9", vec![cap_unversioned("/bin/sh")]);
        let req = cap_unversioned("/bin/sh");
        assert!(provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_satisfies_via_provides_versioned_matches_ge() {
        // libfoo Provides: libabc = 2.0-1, Requires libabc >= 1.0-1 → match.
        let p = pkg(
            "libfoo",
            "1.0",
            "1",
            vec![cap_versioned("libabc", CapFlags::EQ, "2.0", "1")],
        );
        let req = cap_versioned("libabc", CapFlags::GE, "1.0", "1");
        assert!(provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_satisfies_flags_none_always_matches_via_provides() {
        // Requirement has no flags → name match through Provides suffices,
        // regardless of EVR on either side.
        let p = pkg(
            "libfoo",
            "1.0",
            "1",
            vec![cap_versioned("virtual-cap", CapFlags::EQ, "9.9", "9")],
        );
        let req = cap_unversioned("virtual-cap");
        assert!(provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_versioned_requirement_against_versionless_provides_fails() {
        // Versionless Provides cannot satisfy a versioned Requires.
        let p = pkg("libfoo", "1.0", "1", vec![cap_unversioned("virtual-cap")]);
        let req = cap_versioned("virtual-cap", CapFlags::GE, "1.0", "1");
        assert!(!provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_satisfies_file_path_requirement_via_files() {
        // `Requires: /sbin/ldconfig` matches the package whose filelist
        // contains the path (rpm-md models file deps via the per-package
        // filelist, not as synthetic Provides).
        let mut p = pkg("glibc", "2.34", "1", vec![]);
        p.files = vec![Arc::from("/sbin/ldconfig")];
        let req = cap_unversioned("/sbin/ldconfig");
        assert!(provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_satisfies_file_path_no_match_when_path_absent() {
        let p = pkg("bash", "5.1.8", "9", vec![]);
        let req = cap_unversioned("/usr/bin/xsltproc");
        assert!(!provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_direct_name_versioned_lt_failure() {
        // Direct package-name match but EVR is greater than the LT bound.
        let p = pkg("cmake", "3.26.5", "1", vec![]);
        let req = cap_versioned("cmake", CapFlags::LT, "3.0.0", "1");
        assert!(!provides_satisfies(&p, &req));
    }

    #[test]
    fn evr_matches_none_flag_always_true() {
        let evr = EVR::new(Some(0), "1.0", "1");
        let req = cap_unversioned("anything");
        assert!(evr_matches(&evr, &req));
    }

    #[test]
    fn evr_matches_eq_with_missing_evr_is_unsatisfiable() {
        // Defensive: flags=EQ + evr=None is malformed; treat as unmet.
        let evr = EVR::new(Some(0), "1.0", "1");
        let req = Capability {
            name: Arc::from("foo"),
            flags: CapFlags::EQ,
            evr: None,
        };
        assert!(!evr_matches(&evr, &req));
    }

    #[test]
    fn evr_matches_all_flag_variants() {
        let pe = EVR::new(Some(0), "2.0", "1");
        // EQ
        assert!(evr_matches(&pe, &cap_versioned("x", CapFlags::EQ, "2.0", "1")));
        assert!(!evr_matches(&pe, &cap_versioned("x", CapFlags::EQ, "2.1", "1")));
        // LT
        assert!(evr_matches(&pe, &cap_versioned("x", CapFlags::LT, "3.0", "1")));
        assert!(!evr_matches(&pe, &cap_versioned("x", CapFlags::LT, "2.0", "1")));
        // LE
        assert!(evr_matches(&pe, &cap_versioned("x", CapFlags::LE, "2.0", "1")));
        assert!(evr_matches(&pe, &cap_versioned("x", CapFlags::LE, "3.0", "1")));
        assert!(!evr_matches(&pe, &cap_versioned("x", CapFlags::LE, "1.0", "1")));
        // GT
        assert!(evr_matches(&pe, &cap_versioned("x", CapFlags::GT, "1.0", "1")));
        assert!(!evr_matches(&pe, &cap_versioned("x", CapFlags::GT, "2.0", "1")));
        // GE
        assert!(evr_matches(&pe, &cap_versioned("x", CapFlags::GE, "2.0", "1")));
        assert!(evr_matches(&pe, &cap_versioned("x", CapFlags::GE, "1.0", "1")));
        assert!(!evr_matches(&pe, &cap_versioned("x", CapFlags::GE, "3.0", "1")));
    }

    #[test]
    fn matches_flag_exhaustive() {
        use Ordering::{Equal, Greater, Less};
        // None: always true.
        for c in [Less, Equal, Greater] {
            assert!(matches_flag(c, CapFlags::None));
        }
        // EQ
        assert!(matches_flag(Equal, CapFlags::EQ));
        assert!(!matches_flag(Less, CapFlags::EQ));
        assert!(!matches_flag(Greater, CapFlags::EQ));
        // LT
        assert!(matches_flag(Less, CapFlags::LT));
        assert!(!matches_flag(Equal, CapFlags::LT));
        assert!(!matches_flag(Greater, CapFlags::LT));
        // LE
        assert!(matches_flag(Less, CapFlags::LE));
        assert!(matches_flag(Equal, CapFlags::LE));
        assert!(!matches_flag(Greater, CapFlags::LE));
        // GT
        assert!(matches_flag(Greater, CapFlags::GT));
        assert!(!matches_flag(Equal, CapFlags::GT));
        assert!(!matches_flag(Less, CapFlags::GT));
        // GE
        assert!(matches_flag(Greater, CapFlags::GE));
        assert!(matches_flag(Equal, CapFlags::GE));
        assert!(!matches_flag(Less, CapFlags::GE));
    }
}
