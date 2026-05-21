//! Shared dependency-matching predicates.
//!
//! Both [`crate::lookup`] (single-shot queries) and [`crate::solver`]
//! (closure walker) need to answer "does this `Package` satisfy a
//! `Dependency`?" with identical semantics. Keeping the predicate
//! in one place prevents the two consumers from drifting.

use rpm_spec_repo_core::{Dependency, EVR, NEVRA, Package, ProviderRef, RepoError, RepoUniverse};

/// Priority assigned to repos not present in the priority map (e.g. when
/// the universe was rebuilt after the map was constructed). Sorted to the
/// bottom of the candidate list.
pub(crate) const REPO_PRIORITY_FALLBACK: i32 = 99;

/// True if `provider` satisfies the (`name`, `version`) shape of
/// `requirement`. Matches dnf semantics:
///
/// * direct package-name match → check EVR via [`evr_matches`];
/// * file-path requirement (`name` starts with `/`) → accept iff the
///   provider's filelist contains the path; rpm forbids version
///   constraints on file deps so we ignore `version` for this branch
///   (matches dnf which never publishes file deps as synthetic
///   versioned provides);
/// * `Provides:` name match → if the requirement is
///   [`CapVersion::Unversioned`], accept; otherwise both sides must
///   carry an EVR (a versionless `Provides:` does NOT satisfy a
///   versioned requirement).
pub fn provides_satisfies(provider: &Package, requirement: &Dependency) -> bool {
    if provider.nevra.name.as_ref() == requirement.name().as_ref() {
        return evr_matches(&provider.nevra.evr(), requirement);
    }
    if requirement.is_file_path() {
        for path in &provider.files {
            if path.as_ref() == requirement.name().as_ref() {
                return true;
            }
        }
    }
    for prov in &provider.provides {
        if prov.name.as_ref() != requirement.name().as_ref() {
            continue;
        }
        // No version constraint on the requirement → name match suffices.
        if requirement.version().is_unversioned() {
            return true;
        }
        // Versioned requirement: a versionless `Provides:` cannot
        // satisfy it (the package didn't declare a version, so we
        // can't compare). Move on to the next provider entry.
        if let Some(prov_evr) = prov.version.evr()
            && evr_matches(prov_evr, requirement)
        {
            return true;
        }
    }
    false
}

/// Check whether `provider_evr` satisfies the EVR constraint encoded
/// in `req`. An unversioned requirement is always satisfied; a
/// versioned one defers to [`CapVersion::matches`] over
/// [`EVR::compare_rpm`].
pub fn evr_matches(provider_evr: &EVR, req: &Dependency) -> bool {
    let Some(req_evr) = req.version().evr() else {
        // Unversioned — name match already verified by caller.
        return true;
    };
    req.version().matches(provider_evr.compare_rpm(req_evr))
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
    let mut candidates = universe.candidates_with_nevra(dep.name())?;
    if file_path_dep
        && let Some(owner) = universe.file_owner(dep.name())?
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
    //! vs Provides, versioned vs versionless, every `CapVersion` variant)
    //! to lock the contract before either consumer drifts.
    use std::cmp::Ordering;
    use std::sync::Arc;

    use rpm_spec_repo_core::{CapVersion, Capability, NEVRA, Package, PkgChecksum, RepoId};

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
            repo_id: RepoId::from("test-repo"),
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

    fn dep_unversioned(name: &str) -> Dependency {
        Dependency::unversioned(name)
    }

    fn dep(name: &str, version: CapVersion) -> Dependency {
        Dependency::new(Capability {
            name: Arc::from(name),
            version,
        })
    }

    /// Provides-side cap builder for the `Package.provides` field.
    fn provides_cap(name: &str) -> Capability {
        Capability::unversioned(name)
    }

    fn provides_cap_versioned(name: &str, version: CapVersion) -> Capability {
        Capability {
            name: Arc::from(name),
            version,
        }
    }

    fn evr(v: &str, r: &str) -> EVR {
        EVR::new(Some(0), v, r)
    }

    #[test]
    fn provides_satisfies_direct_name_match_no_flags() {
        let p = pkg("bash", "5.1.8", "9", vec![]);
        let req = dep_unversioned("bash");
        assert!(provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_satisfies_via_provides_versionless() {
        let p = pkg("bash", "5.1.8", "9", vec![provides_cap("/bin/sh")]);
        let req = dep_unversioned("/bin/sh");
        assert!(provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_satisfies_via_provides_versioned_matches_ge() {
        let p = pkg(
            "libfoo",
            "1.0",
            "1",
            vec![provides_cap_versioned(
                "libabc",
                CapVersion::Eq(evr("2.0", "1")),
            )],
        );
        let req = dep("libabc", CapVersion::Ge(evr("1.0", "1")));
        assert!(provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_satisfies_flags_none_always_matches_via_provides() {
        let p = pkg(
            "libfoo",
            "1.0",
            "1",
            vec![provides_cap_versioned(
                "virtual-cap",
                CapVersion::Eq(evr("9.9", "9")),
            )],
        );
        let req = dep_unversioned("virtual-cap");
        assert!(provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_versioned_requirement_against_versionless_provides_fails() {
        let p = pkg("libfoo", "1.0", "1", vec![provides_cap("virtual-cap")]);
        let req = dep("virtual-cap", CapVersion::Ge(evr("1.0", "1")));
        assert!(!provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_satisfies_file_path_requirement_via_files() {
        let mut p = pkg("glibc", "2.34", "1", vec![]);
        p.files = vec![Arc::from("/sbin/ldconfig")];
        let req = dep_unversioned("/sbin/ldconfig");
        assert!(provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_satisfies_file_path_no_match_when_path_absent() {
        let p = pkg("bash", "5.1.8", "9", vec![]);
        let req = dep_unversioned("/usr/bin/xsltproc");
        assert!(!provides_satisfies(&p, &req));
    }

    #[test]
    fn provides_direct_name_versioned_lt_failure() {
        let p = pkg("cmake", "3.26.5", "1", vec![]);
        let req = dep("cmake", CapVersion::Lt(evr("3.0.0", "1")));
        assert!(!provides_satisfies(&p, &req));
    }

    #[test]
    fn evr_matches_none_flag_always_true() {
        let e = EVR::new(Some(0), "1.0", "1");
        let req = dep_unversioned("anything");
        assert!(evr_matches(&e, &req));
    }

    #[test]
    fn evr_matches_all_flag_variants() {
        let pe = EVR::new(Some(0), "2.0", "1");
        // Eq
        assert!(evr_matches(&pe, &dep("x", CapVersion::Eq(evr("2.0", "1")))));
        assert!(!evr_matches(
            &pe,
            &dep("x", CapVersion::Eq(evr("2.1", "1")))
        ));
        // Lt
        assert!(evr_matches(&pe, &dep("x", CapVersion::Lt(evr("3.0", "1")))));
        assert!(!evr_matches(
            &pe,
            &dep("x", CapVersion::Lt(evr("2.0", "1")))
        ));
        // Le
        assert!(evr_matches(&pe, &dep("x", CapVersion::Le(evr("2.0", "1")))));
        assert!(evr_matches(&pe, &dep("x", CapVersion::Le(evr("3.0", "1")))));
        assert!(!evr_matches(
            &pe,
            &dep("x", CapVersion::Le(evr("1.0", "1")))
        ));
        // Gt
        assert!(evr_matches(&pe, &dep("x", CapVersion::Gt(evr("1.0", "1")))));
        assert!(!evr_matches(
            &pe,
            &dep("x", CapVersion::Gt(evr("2.0", "1")))
        ));
        // Ge
        assert!(evr_matches(&pe, &dep("x", CapVersion::Ge(evr("2.0", "1")))));
        assert!(evr_matches(&pe, &dep("x", CapVersion::Ge(evr("1.0", "1")))));
        assert!(!evr_matches(
            &pe,
            &dep("x", CapVersion::Ge(evr("3.0", "1")))
        ));
    }

    #[test]
    fn cap_version_matches_exhaustive() {
        // The `CapVersion::matches(Ordering)` predicate is the single
        // source of truth for "does this provider EVR satisfy this
        // require op". Pin every (op × Ordering) cell so a future
        // refactor can't silently flip a sign. (Replaces the old
        // `matches_op` shim — `version.matches(cmp)` directly.)
        use Ordering::{Equal, Greater, Less};
        let e = evr("1.0", "1");
        for c in [Less, Equal, Greater] {
            assert!(CapVersion::Unversioned.matches(c));
        }
        assert!(CapVersion::Eq(e.clone()).matches(Equal));
        assert!(!CapVersion::Eq(e.clone()).matches(Less));
        assert!(!CapVersion::Eq(e.clone()).matches(Greater));
        assert!(CapVersion::Lt(e.clone()).matches(Less));
        assert!(!CapVersion::Lt(e.clone()).matches(Equal));
        assert!(CapVersion::Le(e.clone()).matches(Less));
        assert!(CapVersion::Le(e.clone()).matches(Equal));
        assert!(!CapVersion::Le(e.clone()).matches(Greater));
        assert!(CapVersion::Gt(e.clone()).matches(Greater));
        assert!(!CapVersion::Gt(e.clone()).matches(Equal));
        assert!(CapVersion::Ge(e.clone()).matches(Greater));
        assert!(CapVersion::Ge(e.clone()).matches(Equal));
        assert!(!CapVersion::Ge(e).matches(Less));
    }
}
