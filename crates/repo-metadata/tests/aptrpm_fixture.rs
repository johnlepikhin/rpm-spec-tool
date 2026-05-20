//! Integration test for the apt-rpm backend against a real
//! `pkglist.checkinstall.xz` snapshot pulled from a public ALT
//! mirror. The fixture lives under
//! `tests/fixtures/repos/apt-rpm/tiny-alt/` and is checked into the
//! repo so CI doesn't need network access.
//!
//! End-to-end verification:
//! 1. release-file parser pulls the right top-level fields.
//! 2. xz-decompression of `pkglist.checkinstall.xz` yields a valid
//!    rpm-header chain.
//! 3. The chain parses into ≥1 [`rpm_spec_repo_core::Package`] with
//!    non-empty NAME / VERSION / RELEASE / ARCH.
//! 4. At least one package has provides / requires populated (the
//!    "all headers parsed correctly" smoke check — a corrupt
//!    parser tends to read all-empty arrays).

use std::fs;
use std::path::Path;

use rpm_spec_repo_core::RepoId;
use rpm_spec_repo_metadata::aptrpm;
use rpm_spec_repo_metadata::compression::decompress;

fn fixture_dir() -> &'static Path {
    // CARGO_MANIFEST_DIR is the crate root (`crates/repo-metadata/`);
    // the fixture lives at workspace-root/tests/fixtures/...
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
}

#[test]
fn release_file_parses() {
    let path = fixture_dir().join("tests/fixtures/repos/apt-rpm/tiny-alt/base/release");
    let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let r = aptrpm::parse_release_bytes(&bytes).expect("parse release file");
    assert_eq!(r.origin, "ALT Linux Team");
    assert_eq!(r.label.as_deref(), Some("p10"));
    // Per-component release files don't carry an Architectures: line
    // (they cover the single arch in the parent directory). The
    // checkinstall fixture is x86_64; the parser permits empty
    // architectures on per-component files — sanity-check by
    // requiring at least one entry.
    assert!(!r.architectures.is_empty(), "got: {:?}", r.architectures);
}

#[test]
fn pkglist_xz_parses_into_packages() {
    let path = fixture_dir()
        .join("tests/fixtures/repos/apt-rpm/tiny-alt/base/pkglist.classic.xz");
    let xz_bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let bytes =
        decompress("pkglist.classic.xz", &xz_bytes).expect("decompress fixture pkglist");

    // Magic check: rpm header v3 starts with `8e ad e8 01`. If the
    // decompressed bytes don't start with this, either xz produced
    // garbage (broken fixture) or the wrong file got committed.
    assert!(
        bytes.len() > 16,
        "decompressed pkglist suspiciously small: {}",
        bytes.len()
    );
    assert_eq!(
        &bytes[..4],
        &[0x8e, 0xad, 0xe8, 0x01],
        "decompressed pkglist doesn't start with rpm header magic"
    );

    let repo_id: RepoId = std::sync::Arc::from("classic");
    let packages = aptrpm::parse_pkglist_bytes(&bytes, &repo_id).expect("parse pkglist");

    assert!(
        !packages.is_empty(),
        "no packages parsed from checkinstall fixture"
    );
    // Verify every required field of every package is populated —
    // catches the "parser returned all-default Package" failure
    // mode where binary decoding silently yields blanks.
    for p in &packages {
        assert!(!p.nevra.name.is_empty(), "empty name: {p:?}");
        assert!(!p.nevra.version.is_empty(), "empty version: {p:?}");
        assert!(!p.nevra.release.is_empty(), "empty release: {p:?}");
        assert!(!p.nevra.arch.is_empty(), "empty arch: {p:?}");
        assert_eq!(p.repo_id, repo_id);
    }

    // At least one package should carry provides AND requires —
    // an apt-rpm binary that doesn't declare any of either would
    // be highly unusual on a real ALT mirror; if every parsed
    // package looks empty, the dep-tuple zipping is broken.
    let with_provides = packages.iter().filter(|p| !p.provides.is_empty()).count();
    let with_requires = packages.iter().filter(|p| !p.requires.is_empty()).count();
    assert!(
        with_provides > 0,
        "no parsed package had any provides — header zipping likely broken"
    );
    assert!(
        with_requires > 0,
        "no parsed package had any requires — header zipping likely broken"
    );
}
