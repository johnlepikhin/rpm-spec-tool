//! Shared test fixtures for the RPM-REPO-* rules.
//!
//! Provides a built-in `redos-7.3-x86_64` profile (the user's
//! preferred stable / predictable distro for repo-aware tests) and
//! a tiny in-memory `RepoUniverse` with a handful of hand-crafted
//! packages so each rule's unit test stays self-contained.

use std::sync::Arc;

use rpm_spec_profile::Profile;
use rpm_spec_repo_core::{
    CapFlags, Capability, NEVRA, Package, PkgChecksum, RepoIndex, RepoUniverse,
};
use time::OffsetDateTime;

pub fn redos_profile() -> Profile {
    let mut p = Profile::default();
    p.identity.name = "redos-7.3-x86_64".to_string();
    p.identity.family = Some(rpm_spec_profile::Family::Generic);
    p.arch.build_arch = Some("x86_64".to_string());
    p
}

/// Build a tiny but realistic universe: `bash`, `glibc`, two
/// versions of `cmake` (3.20 and 3.26), and one explicit `Provides:`
/// (`pkgconfig(libsystemd)` from `systemd-devel`). Covers all three
/// RPM-REPO-* outcomes:
///   * `bash`, `glibc`, `cmake` → `Satisfied`
///   * `cmake >= 3.28` → `VersionUnsatisfied`
///   * `missing-package`, `pkgconfig(does-not-exist)` → `NoProvider`
pub fn tiny_universe() -> Arc<RepoUniverse> {
    fn pkg(name: &str, version: &str, release: &str, provides: Vec<&str>) -> Package {
        Package {
            nevra: NEVRA {
                name: Arc::from(name),
                epoch: 0,
                version: Arc::from(version),
                release: Arc::from(release),
                arch: Arc::from("x86_64"),
            },
            repo_id: Arc::from("baseos"),
            provides: provides
                .into_iter()
                .map(|p| Capability {
                    name: Arc::from(p),
                    flags: CapFlags::None,
                    evr: None,
                })
                .collect(),
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
            checksum: PkgChecksum::Sha256(String::new()),
            location: Arc::from(""),
            files: Vec::new(),
        }
    }

    let packages = vec![
        pkg("bash", "5.1.8", "9.el9", vec!["bash", "/bin/bash"]),
        pkg("glibc", "2.34", "1.el9", vec!["glibc", "libc.so.6"]),
        pkg("cmake", "3.20.0", "1.el9", vec!["cmake"]),
        pkg("cmake", "3.26.5", "2.el9", vec!["cmake"]),
        pkg(
            "systemd-devel",
            "252",
            "1.el9",
            vec!["systemd-devel", "pkgconfig(libsystemd)"],
        ),
    ];

    let index = RepoIndex {
        repo_id: Arc::from("baseos"),
        revision: "deadbeef".to_string(),
        fetched_at: OffsetDateTime::now_utc(),
        packages,
        advisories: Vec::new(),
    };
    Arc::new(
        RepoUniverse::from_indexes_for_tests("redos-7.3-x86_64", vec![Arc::new(index)])
            .expect("build in-memory tiny universe"),
    )
}
