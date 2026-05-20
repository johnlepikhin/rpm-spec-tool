//! Smoke tests for the P0 walker. Constructs a tiny inline universe
//! (no fixture files) and exercises happy path, unsatisfiable, and
//! conflict scenarios.

use std::sync::Arc;

use rpm_spec_repo_core::{
    Capability, Dependency, NEVRA, Package, PkgChecksum, RepoIndex, RepoUniverse,
};
use rpm_spec_repo_resolver::{SolveRequest, Solution, solve};
use time::OffsetDateTime;

fn nevra(name: &str, version: &str, release: &str) -> NEVRA {
    NEVRA {
        name: Arc::from(name),
        epoch: 0,
        version: Arc::from(version),
        release: Arc::from(release),
        arch: Arc::from("x86_64"),
    }
}

fn cap(name: &str) -> Capability {
    Capability::unversioned(name)
}

fn pkg(
    repo_id: &str,
    name: &str,
    version: &str,
    release: &str,
    requires: Vec<Capability>,
    provides: Vec<Capability>,
    conflicts: Vec<Capability>,
) -> Package {
    Package {
        nevra: nevra(name, version, release),
        repo_id: Arc::from(repo_id),
        provides,
        requires,
        conflicts,
        obsoletes: Vec::new(),
        recommends: Vec::new(),
        suggests: Vec::new(),
        supplements: Vec::new(),
        enhances: Vec::new(),
        source_rpm: None,
        summary: Arc::from(""),
        size_installed: 1_024,
        checksum: PkgChecksum::Sha256(format!("{name}-{version}-{release}")),
        location: Arc::from(format!("Packages/{name}-{version}-{release}.x86_64.rpm")),
        files: Vec::new(),
    }
}

fn universe(packages: Vec<Package>) -> RepoUniverse {
    let repo_id: Arc<str> = Arc::from("test-repo");
    let index = RepoIndex {
        repo_id: repo_id.clone(),
        revision: "rev0".into(),
        fetched_at: OffsetDateTime::now_utc(),
        packages,
        advisories: Vec::new(),
    };
    RepoUniverse::from_indexes_for_tests("test-profile", vec![Arc::new(index)])
        .expect("build in-memory test universe")
}

#[test]
fn happy_path_three_package_universe() {
    // cmake requires bash + glibc transitively.
    let glibc = pkg(
        "test-repo",
        "glibc",
        "2.34",
        "1",
        Vec::new(),
        vec![cap("glibc")],
        Vec::new(),
    );
    let bash = pkg(
        "test-repo",
        "bash",
        "5.1.8",
        "9",
        vec![cap("glibc")],
        vec![cap("bash"), cap("/bin/sh")],
        Vec::new(),
    );
    let cmake = pkg(
        "test-repo",
        "cmake",
        "3.26.5",
        "1",
        vec![cap("bash"), cap("glibc")],
        vec![cap("cmake")],
        Vec::new(),
    );
    let uni = universe(vec![glibc, bash, cmake]);

    let req = vec![cap("cmake")];
    let sol = solve(SolveRequest {
        universe: &uni,
        requirements: &req,
        base_packages: &[],
        implicit_brs: &[],
    })
    .expect("solver db query");
    match sol {
        Solution::Ok(closure) => {
            let names: Vec<&str> = closure.packages.iter().map(|n| n.name.as_ref()).collect();
            assert!(names.contains(&"cmake"), "cmake missing: {names:?}");
            assert!(names.contains(&"bash"), "bash missing: {names:?}");
            assert!(names.contains(&"glibc"), "glibc missing: {names:?}");
            assert_eq!(closure.install_size_total, 1_024 * 3);
        }
        Solution::Unsatisfiable(core) => {
            panic!("expected Ok, got Unsatisfiable: {core:?}");
        }
    }
}

#[test]
fn unsatisfiable_when_provider_absent() {
    let uni = universe(vec![pkg(
        "test-repo",
        "bash",
        "5.1.8",
        "9",
        Vec::new(),
        vec![cap("bash")],
        Vec::new(),
    )]);

    let req = vec![Dependency::unversioned("nonexistent")];
    let sol = solve(SolveRequest {
        universe: &uni,
        requirements: &req,
        base_packages: &[],
        implicit_brs: &[],
    })
    .expect("solver db query");
    match sol {
        Solution::Unsatisfiable(core) => {
            assert_eq!(core.unsatisfied.len(), 1);
            assert_eq!(core.unsatisfied[0].dep.name.as_ref(), "nonexistent");
            // Top-level BR — no transitive ancestry.
            assert!(core.unsatisfied[0].provenance.is_from_spec());
        }
        Solution::Ok(c) => panic!("expected Unsatisfiable, got Ok({c:?})"),
    }
}

#[test]
fn file_path_alternative_provider_short_circuits() {
    // Regression for the `pkgconfig` vs `pkgconf-pkg-config` case.
    //
    // Two packages both own `/usr/bin/pkg-config` AND declare mutual
    // `Conflicts:`. One is pre-pinned via `base_packages`. The other
    // satisfies a file-path requirement that another package's
    // `Requires:` brings in.
    //
    // Bug before the `any_pinned_owns` fix: the solver picked the
    // second package for the file-path round, then `would_conflict`
    // tripped on the mutual `Conflicts:` and the solve came back
    // Unsatisfiable.
    //
    // Fix: `any_pinned_owns` short-circuits — the file is already
    // owned by something in the closure, so no second pin needed.
    let pkgconfig = pkg(
        "test-repo",
        "pkgconfig",
        "1.8.0",
        "1",
        Vec::new(),
        vec![cap("pkgconfig")],
        vec![cap("pkgconf-pkg-config")],
    );
    let mut pkgconfig = pkgconfig;
    pkgconfig.files = vec![Arc::from("/usr/bin/pkg-config")];

    let pkgconf = pkg(
        "test-repo",
        "pkgconf-pkg-config",
        "1.8.0",
        "1",
        Vec::new(),
        vec![cap("pkgconfig")],
        vec![cap("pkgconfig")],
    );
    let mut pkgconf = pkgconf;
    pkgconf.files = vec![Arc::from("/usr/bin/pkg-config")];

    let consumer = pkg(
        "test-repo",
        "consumer",
        "1.0",
        "1",
        vec![cap("/usr/bin/pkg-config")],
        vec![cap("consumer")],
        Vec::new(),
    );
    let uni = universe(vec![pkgconfig, pkgconf, consumer]);

    let base = vec![cap("pkgconfig")];
    let req = vec![cap("consumer")];
    let sol = solve(SolveRequest {
        universe: &uni,
        requirements: &req,
        base_packages: &base,
        implicit_brs: &[],
    })
    .expect("solver db query");
    match sol {
        Solution::Ok(closure) => {
            let names: Vec<&str> = closure.packages.iter().map(|n| n.name.as_ref()).collect();
            assert!(names.contains(&"pkgconfig"), "pkgconfig missing: {names:?}");
            assert!(names.contains(&"consumer"), "consumer missing: {names:?}");
            assert!(
                !names.contains(&"pkgconf-pkg-config"),
                "alternative provider must not be pinned: {names:?}",
            );
        }
        Solution::Unsatisfiable(core) => {
            panic!(
                "expected Ok (file path already owned by pinned pkgconfig), got Unsatisfiable: {core:?}"
            );
        }
    }
}

#[test]
fn conflict_chain_surfaces_when_two_packages_conflict() {
    // mta-alpha and mta-beta both claim provides:mta and conflict
    // with each other. Asking for both pulls a conflict.
    let alpha = pkg(
        "test-repo",
        "mta-alpha",
        "1.0",
        "1",
        Vec::new(),
        vec![cap("mta-alpha"), cap("mta")],
        vec![cap("mta-beta")],
    );
    let beta = pkg(
        "test-repo",
        "mta-beta",
        "1.0",
        "1",
        Vec::new(),
        vec![cap("mta-beta"), cap("mta")],
        vec![cap("mta-alpha")],
    );
    let uni = universe(vec![alpha, beta]);

    let req = vec![cap("mta-alpha"), cap("mta-beta")];
    let sol = solve(SolveRequest {
        universe: &uni,
        requirements: &req,
        base_packages: &[],
        implicit_brs: &[],
    })
    .expect("solver db query");
    match sol {
        Solution::Unsatisfiable(core) => {
            assert!(
                !core.conflict_chains.is_empty(),
                "expected conflict_chains non-empty, got {core:?}"
            );
        }
        Solution::Ok(c) => panic!("expected Unsatisfiable due to conflict, got Ok({c:?})"),
    }
}
