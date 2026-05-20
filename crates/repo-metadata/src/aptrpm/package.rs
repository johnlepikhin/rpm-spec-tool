//! Map a generic rpm-header [`Header`] (tag → typed value) to a
//! [`Package`] the resolver / lints consume.
//!
//! RPM-tag numbers come from `rpm/rpmtag.h`. The catalogue here is
//! the union of what the resolver actually reads — full coverage
//! per the iteration plan (NAME / VERSION / RELEASE / EPOCH / ARCH /
//! SOURCERPM / SUMMARY / SIZE plus provides / requires / conflicts /
//! obsoletes and the four weak-dep families).
//!
//! Each tag tuple (NAME / FLAGS / VERSION) gets zipped into a list
//! of [`Capability`] values. apt-rpm guarantees the three arrays
//! are the same length per tuple; we tolerate length mismatches by
//! truncating to the shortest (corrupt fixture > crash).

use std::sync::Arc;

use rpm_spec_repo_core::{
    CapVersion, Capability, Dependency, EVR, NEVRA, Package, PkgChecksum, RepoId,
};

use super::header::{Header, HeaderEntry};

// RPMTAG_* constants — kept inline because the resolver only needs
// the subset below, pulling in a `rpm-tags` crate just for these
// would be overkill.
const TAG_NAME: u32 = 1000;
const TAG_VERSION: u32 = 1001;
const TAG_RELEASE: u32 = 1002;
const TAG_EPOCH: u32 = 1003;
const TAG_SUMMARY: u32 = 1004;
const TAG_SIZE: u32 = 1009;
const TAG_ARCH: u32 = 1022;
const TAG_SOURCERPM: u32 = 1044;
const TAG_REQUIREFLAGS: u32 = 1048;
const TAG_REQUIRENAME: u32 = 1049;
const TAG_REQUIREVERSION: u32 = 1050;
const TAG_CONFLICTFLAGS: u32 = 1053;
const TAG_CONFLICTNAME: u32 = 1054;
const TAG_CONFLICTVERSION: u32 = 1055;
const TAG_OBSOLETENAME: u32 = 1090;
const TAG_OBSOLETEFLAGS: u32 = 1114;
const TAG_OBSOLETEVERSION: u32 = 1115;
const TAG_PROVIDENAME: u32 = 1047;
const TAG_PROVIDEFLAGS: u32 = 1112;
const TAG_PROVIDEVERSION: u32 = 1113;
const TAG_RECOMMENDNAME: u32 = 5046;
const TAG_RECOMMENDFLAGS: u32 = 5048;
const TAG_RECOMMENDVERSION: u32 = 5047;
const TAG_SUGGESTNAME: u32 = 5049;
const TAG_SUGGESTFLAGS: u32 = 5051;
const TAG_SUGGESTVERSION: u32 = 5050;
const TAG_SUPPLEMENTNAME: u32 = 5052;
const TAG_SUPPLEMENTFLAGS: u32 = 5054;
const TAG_SUPPLEMENTVERSION: u32 = 5053;
const TAG_ENHANCENAME: u32 = 5055;
const TAG_ENHANCEFLAGS: u32 = 5057;
const TAG_ENHANCEVERSION: u32 = 5056;

// RPMSENSE_* dep-flag bits (from rpm/rpmds.h). apt-rpm uses the
// same flag set as rpm-md — three comparison bits we map to
// `CapFlags`, plus two filter bits we skip entirely.
const RPMSENSE_LESS: u32 = 1 << 1;
const RPMSENSE_GREATER: u32 = 1 << 2;
const RPMSENSE_EQUAL: u32 = 1 << 3;
/// `Requires(missingok): foo` — soft requirement the resolver may
/// silently ignore if no provider exists. ALT's rpm-build-python3
/// uses this for namespace markers (`python3(...)` with an
/// intentionally-unsatisfiable `< 0` EVR); without skipping these
/// the buildroot solver reports hundreds of false UNSAT entries.
const RPMSENSE_MISSINGOK: u32 = 1 << 19;
/// `rpmlib(Feature)` — runtime capability provided by the rpm
/// binary itself, not by any installable package. Always-satisfied
/// in dnf/yum/apt-rpm by construction. Skipping at parse time
/// keeps the resolver's `Capability` namespace package-only.
const RPMSENSE_RPMLIB: u32 = 1 << 24;

/// Build a [`Package`] from a parsed header. The required tags
/// (NAME / VERSION / RELEASE / ARCH) must be present; missing them
/// returns `None` since the package would be unreferenceable from
/// the resolver. All other tags default to empty / "0" / etc.
#[must_use]
pub fn package_from_header(header: &Header, repo_id: &RepoId) -> Option<Package> {
    let name = string_tag(header, TAG_NAME)?;
    let version = string_tag(header, TAG_VERSION)?;
    let release = string_tag(header, TAG_RELEASE)?;
    let arch = string_tag(header, TAG_ARCH)?;
    let epoch = first_u32(header, TAG_EPOCH).unwrap_or(0);
    let summary = i18n_first(header, TAG_SUMMARY).unwrap_or_default();
    let size = first_u32(header, TAG_SIZE).unwrap_or(0) as u64;
    let source_rpm = string_tag(header, TAG_SOURCERPM).map(|s| Arc::from(s.as_str()));

    let nevra = NEVRA {
        name: Arc::from(name.as_str()),
        epoch,
        version: Arc::from(version.as_str()),
        release: Arc::from(release.as_str()),
        arch: Arc::from(arch.as_str()),
    };

    let provides = capability_triple(header, TAG_PROVIDENAME, TAG_PROVIDEFLAGS, TAG_PROVIDEVERSION);
    let requires = dependency_triple(header, TAG_REQUIRENAME, TAG_REQUIREFLAGS, TAG_REQUIREVERSION);
    let conflicts = dependency_triple(
        header,
        TAG_CONFLICTNAME,
        TAG_CONFLICTFLAGS,
        TAG_CONFLICTVERSION,
    );
    let obsoletes = dependency_triple(
        header,
        TAG_OBSOLETENAME,
        TAG_OBSOLETEFLAGS,
        TAG_OBSOLETEVERSION,
    );
    let recommends = dependency_triple(
        header,
        TAG_RECOMMENDNAME,
        TAG_RECOMMENDFLAGS,
        TAG_RECOMMENDVERSION,
    );
    let suggests = dependency_triple(
        header,
        TAG_SUGGESTNAME,
        TAG_SUGGESTFLAGS,
        TAG_SUGGESTVERSION,
    );
    let supplements = dependency_triple(
        header,
        TAG_SUPPLEMENTNAME,
        TAG_SUPPLEMENTFLAGS,
        TAG_SUPPLEMENTVERSION,
    );
    let enhances = dependency_triple(
        header,
        TAG_ENHANCENAME,
        TAG_ENHANCEFLAGS,
        TAG_ENHANCEVERSION,
    );

    Some(Package {
        nevra,
        repo_id: repo_id.clone(),
        provides,
        requires,
        conflicts,
        obsoletes,
        recommends,
        suggests,
        supplements,
        enhances,
        source_rpm,
        summary: Arc::from(summary.as_str()),
        size_installed: size,
        // apt-rpm pkglist entries don't carry a checksum (the .rpm
        // is the source of truth; pkglist is metadata only). The
        // resolver doesn't need one — `PkgChecksum::Other` with an
        // empty hex is a non-validating placeholder that the cache
        // layer accepts without trying to verify.
        checksum: PkgChecksum::Other {
            algo: "none".to_string(),
            hex: String::new(),
        },
        // location is the .rpm path relative to baseurl. pkglist
        // doesn't carry it directly; we synthesize the canonical
        // ALT layout `RPMS.classic/<name>-<v>-<r>.<arch>.rpm`.
        // This is good enough for `repo show` and any future
        // downloader; the file-owner index uses the `files` map.
        location: Arc::from(synth_location(header).as_str()),
        // Files come from `contents_index`, ingested separately by
        // the backend after pkglist parsing — leave empty here.
        files: Vec::new(),
    })
}

fn string_tag(header: &Header, tag: u32) -> Option<String> {
    match header.get(tag)? {
        HeaderEntry::String(s) => Some(s.clone()),
        // apt-rpm sometimes ships a single-element StringArray
        // where rpm-md would use a plain String. Be lenient.
        HeaderEntry::StringArray(v) | HeaderEntry::I18nString(v) => v.first().cloned(),
        _ => None,
    }
}

fn first_u32(header: &Header, tag: u32) -> Option<u32> {
    match header.get(tag)? {
        HeaderEntry::Int32(v) => v.first().copied(),
        _ => None,
    }
}

fn i18n_first(header: &Header, tag: u32) -> Option<String> {
    match header.get(tag)? {
        HeaderEntry::I18nString(v) | HeaderEntry::StringArray(v) => v.first().cloned(),
        HeaderEntry::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Wrap [`capability_triple`] for the require-side fields — each
/// produced `Capability` is moved into a `Dependency` so the
/// `Package.requires`/`conflicts`/... slots get the typed shape.
fn dependency_triple(
    header: &Header,
    name_tag: u32,
    flags_tag: u32,
    version_tag: u32,
) -> Vec<Dependency> {
    capability_triple(header, name_tag, flags_tag, version_tag)
        .into_iter()
        .map(Dependency::new)
        .collect()
}

fn capability_triple(
    header: &Header,
    name_tag: u32,
    flags_tag: u32,
    version_tag: u32,
) -> Vec<Capability> {
    let names = match header.get(name_tag) {
        Some(HeaderEntry::StringArray(v) | HeaderEntry::I18nString(v)) => v.as_slice(),
        _ => return Vec::new(),
    };
    let flags: Vec<u32> = match header.get(flags_tag) {
        Some(HeaderEntry::Int32(v)) => v.clone(),
        // Missing flags array = all unflagged (CapFlags::None).
        _ => vec![0; names.len()],
    };
    let versions: Vec<String> = match header.get(version_tag) {
        Some(HeaderEntry::StringArray(v) | HeaderEntry::I18nString(v)) => v.clone(),
        _ => vec![String::new(); names.len()],
    };
    // Truncate to the shortest length so a corrupt header (where
    // names / flags / versions have different lengths) yields
    // partial output rather than panicking.
    let len = names.len().min(flags.len()).min(versions.len());
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let name = names[i].as_str();
        if name.is_empty() {
            continue;
        }
        // Skip `rpmlib(...)` runtime caps and `(missingok)` soft
        // markers entirely — see the constant docs. Both classes
        // would otherwise produce false-positive UNSAT entries
        // since no package in the universe provides them.
        //
        // The `name.starts_with("rpmlib(")` belt-and-braces check
        // catches ALT pkglist entries that put rpmlib feature deps
        // into the table *without* setting `RPMSENSE_RPMLIB`
        // (observed in real p11 snapshots — `rpmlib(PosttransFiletriggers)`
        // ships with bit-flag 0). The cap name itself is the
        // authoritative signal of the rpmlib namespace.
        if flags[i] & (RPMSENSE_RPMLIB | RPMSENSE_MISSINGOK) != 0
            || name.starts_with("rpmlib(")
        {
            continue;
        }
        // `Requires: foo < 0` is a contradiction (versions are
        // non-negative) used by ALT's `rpm-build-python3` (and a
        // handful of other ALT tooling) as a namespace marker —
        // never a real requirement. Filter at parse time so the
        // resolver doesn't report perpetual UNSAT against these
        // intentionally-impossible constraints.
        if flags[i] & RPMSENSE_LESS != 0
            && (versions[i].trim() == "0" || versions[i].is_empty())
        {
            continue;
        }
        let version = decode_version(flags[i], &versions[i]);
        out.push(Capability {
            name: Arc::from(name),
            version,
        });
    }
    out
}

/// Decode `RPMSENSE_*` bits + the version string into a [`CapVersion`].
/// rpm models dep flags as a bit set: `LESS`, `GREATER`, `EQUAL` are
/// the three comparison bits (combinable as `LESS|EQUAL` → `<=`).
/// An entirely unflagged dep, or a versioned op with an unparseable
/// EVR, collapses to [`CapVersion::Unversioned`].
fn decode_version(flags: u32, version_raw: &str) -> CapVersion {
    let has_lt = flags & RPMSENSE_LESS != 0;
    let has_gt = flags & RPMSENSE_GREATER != 0;
    let has_eq = flags & RPMSENSE_EQUAL != 0;
    let evr = match parse_evr(version_raw) {
        Some(e) => e,
        // Op set but no EVR — collapse to Unversioned (matches dnf
        // tolerance and the old `decode_flags` early-return).
        None => return CapVersion::Unversioned,
    };
    match (has_lt, has_gt, has_eq) {
        (false, false, false) => CapVersion::Unversioned,
        (true, false, false) => CapVersion::Lt(evr),
        (false, true, false) => CapVersion::Gt(evr),
        (false, false, true) => CapVersion::Eq(evr),
        (true, false, true) => CapVersion::Le(evr),
        (false, true, true) => CapVersion::Ge(evr),
        // `LESS|GREATER` doesn't exist in rpm; treat as unversioned.
        _ => CapVersion::Unversioned,
    }
}

/// Parse an rpm wire EVR string `[E:]V[-R]` into an [`EVR`].
/// Returns `None` on an entirely empty input (which yields no
/// version constraint at the caller).
fn parse_evr(raw: &str) -> Option<EVR> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (epoch, rest) = match trimmed.split_once(':') {
        Some((e, rest)) => match e.parse::<u32>() {
            Ok(n) => (Some(n), rest),
            Err(_) => (None, trimmed),
        },
        None => (None, trimmed),
    };
    let (version, release) = match rest.split_once('-') {
        Some((v, r)) => (v, r),
        None => (rest, ""),
    };
    Some(EVR::new(epoch, version, release))
}

fn synth_location(header: &Header) -> String {
    let name = string_tag(header, TAG_NAME).unwrap_or_default();
    let ver = string_tag(header, TAG_VERSION).unwrap_or_default();
    let rel = string_tag(header, TAG_RELEASE).unwrap_or_default();
    let arch = string_tag(header, TAG_ARCH).unwrap_or_default();
    format!("RPMS.classic/{name}-{ver}-{rel}.{arch}.rpm")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aptrpm::header::HeaderEntry;

    fn make_header(entries: Vec<(u32, HeaderEntry)>) -> Header {
        Header { entries }
    }

    fn s(v: &str) -> HeaderEntry {
        HeaderEntry::String(v.to_string())
    }

    fn sa(v: &[&str]) -> HeaderEntry {
        HeaderEntry::StringArray(v.iter().map(|s| s.to_string()).collect())
    }

    fn i(v: &[u32]) -> HeaderEntry {
        HeaderEntry::Int32(v.to_vec())
    }

    #[test]
    fn minimal_package_resolves() {
        let h = make_header(vec![
            (TAG_NAME, s("bash")),
            (TAG_VERSION, s("5.1.8")),
            (TAG_RELEASE, s("alt1")),
            (TAG_ARCH, s("x86_64")),
        ]);
        let p = package_from_header(&h, &RepoId::from("classic")).unwrap();
        assert_eq!(p.nevra.name.as_ref(), "bash");
        assert_eq!(p.nevra.epoch, 0);
        assert_eq!(p.nevra.version.as_ref(), "5.1.8");
        assert_eq!(p.nevra.arch.as_ref(), "x86_64");
        assert!(p.provides.is_empty());
    }

    #[test]
    fn missing_required_tag_returns_none() {
        // No NAME → can't build a Package.
        let h = make_header(vec![(TAG_VERSION, s("1.0"))]);
        assert!(package_from_header(&h, &RepoId::from("classic")).is_none());
    }

    #[test]
    fn versioned_provides_decode() {
        let h = make_header(vec![
            (TAG_NAME, s("foo")),
            (TAG_VERSION, s("1.0")),
            (TAG_RELEASE, s("1")),
            (TAG_ARCH, s("x86_64")),
            (TAG_PROVIDENAME, sa(&["pkgconfig(foo)"])),
            (TAG_PROVIDEFLAGS, i(&[RPMSENSE_EQUAL])),
            (TAG_PROVIDEVERSION, sa(&["1.0-1"])),
        ]);
        let p = package_from_header(&h, &RepoId::from("classic")).unwrap();
        assert_eq!(p.provides.len(), 1);
        let cap = &p.provides[0];
        assert_eq!(cap.name.as_ref(), "pkgconfig(foo)");
        match &cap.version {
            CapVersion::Eq(evr) => {
                assert_eq!(evr.version, "1.0");
                assert_eq!(evr.release, "1");
            }
            other => panic!("expected Eq variant, got {other:?}"),
        }
    }

    #[test]
    fn flag_combinations_le_ge() {
        let h_le = make_header(vec![
            (TAG_NAME, s("a")),
            (TAG_VERSION, s("1")),
            (TAG_RELEASE, s("1")),
            (TAG_ARCH, s("x86_64")),
            (TAG_REQUIRENAME, sa(&["dep"])),
            (TAG_REQUIREFLAGS, i(&[RPMSENSE_LESS | RPMSENSE_EQUAL])),
            (TAG_REQUIREVERSION, sa(&["2.0"])),
        ]);
        let p = package_from_header(&h_le, &RepoId::from("c")).unwrap();
        let CapVersion::Le(evr_le) = p.requires[0].version() else {
            panic!("expected Le, got {:?}", p.requires[0].version());
        };
        assert_eq!(&*evr_le.version, "2.0");

        let h_ge = make_header(vec![
            (TAG_NAME, s("b")),
            (TAG_VERSION, s("1")),
            (TAG_RELEASE, s("1")),
            (TAG_ARCH, s("x86_64")),
            (TAG_REQUIRENAME, sa(&["dep"])),
            (TAG_REQUIREFLAGS, i(&[RPMSENSE_GREATER | RPMSENSE_EQUAL])),
            (TAG_REQUIREVERSION, sa(&["2.0"])),
        ]);
        let p = package_from_header(&h_ge, &RepoId::from("c")).unwrap();
        let CapVersion::Ge(evr_ge) = p.requires[0].version() else {
            panic!("expected Ge, got {:?}", p.requires[0].version());
        };
        assert_eq!(&*evr_ge.version, "2.0");
    }

    #[test]
    fn unversioned_dep_yields_none_evr() {
        let h = make_header(vec![
            (TAG_NAME, s("a")),
            (TAG_VERSION, s("1")),
            (TAG_RELEASE, s("1")),
            (TAG_ARCH, s("x86_64")),
            (TAG_REQUIRENAME, sa(&["bash"])),
            (TAG_REQUIREFLAGS, i(&[0])),
            (TAG_REQUIREVERSION, sa(&[""])),
        ]);
        let p = package_from_header(&h, &RepoId::from("c")).unwrap();
        assert_eq!(p.requires.len(), 1);
        assert!(p.requires[0].version().is_unversioned());
    }

    #[test]
    fn parse_evr_handles_epoch_and_release() {
        let evr = parse_evr("2:1.0-1.el9").unwrap();
        assert_eq!(evr.epoch, 2);
        assert_eq!(evr.version, "1.0");
        assert_eq!(evr.release, "1.el9");

        let evr = parse_evr("1.5").unwrap();
        assert_eq!(evr.epoch, 0);
        assert_eq!(evr.version, "1.5");
        assert_eq!(evr.release, "");

        assert!(parse_evr("").is_none());
    }

    #[test]
    fn weak_deps_decoded() {
        let h = make_header(vec![
            (TAG_NAME, s("a")),
            (TAG_VERSION, s("1")),
            (TAG_RELEASE, s("1")),
            (TAG_ARCH, s("x86_64")),
            (TAG_RECOMMENDNAME, sa(&["nice-to-have"])),
            (TAG_RECOMMENDFLAGS, i(&[0])),
            (TAG_RECOMMENDVERSION, sa(&[""])),
            (TAG_SUGGESTNAME, sa(&["maybe"])),
            (TAG_SUGGESTFLAGS, i(&[0])),
            (TAG_SUGGESTVERSION, sa(&[""])),
        ]);
        let p = package_from_header(&h, &RepoId::from("c")).unwrap();
        assert_eq!(p.recommends.len(), 1);
        assert_eq!(p.recommends[0].name().as_ref(), "nice-to-have");
        assert_eq!(p.suggests.len(), 1);
        assert_eq!(p.suggests[0].name().as_ref(), "maybe");
    }

    #[test]
    fn skips_rpmlib_requires() {
        // `Requires: rpmlib(PayloadIsXz)` carries RPMSENSE_RPMLIB.
        // The resolver has no provider for rpmlib(*) caps (rpm itself
        // does at runtime); without filtering these would surface as
        // false-positive UNSAT on every ALT/RHEL spec.
        let h = make_header(vec![
            (TAG_NAME, s("a")),
            (TAG_VERSION, s("1")),
            (TAG_RELEASE, s("1")),
            (TAG_ARCH, s("x86_64")),
            (
                TAG_REQUIRENAME,
                sa(&["rpmlib(PayloadIsXz)", "bash", "rpmlib(SetVersions)"]),
            ),
            (
                TAG_REQUIREFLAGS,
                i(&[
                    RPMSENSE_RPMLIB | RPMSENSE_LESS | RPMSENSE_EQUAL,
                    0,
                    RPMSENSE_RPMLIB,
                ]),
            ),
            (TAG_REQUIREVERSION, sa(&["5.2-1", "", ""])),
        ]);
        let p = package_from_header(&h, &RepoId::from("c")).unwrap();
        // Only the non-rpmlib `bash` survived.
        assert_eq!(p.requires.len(), 1, "got: {:?}", p.requires);
        assert_eq!(p.requires[0].name().as_ref(), "bash");
    }

    #[test]
    fn skips_missingok_marker_deps() {
        // ALT's rpm-build-python3 declares `Requires: python3(NAME) < 0`
        // with RPMSENSE_MISSINGOK as a namespace marker — apt-rpm
        // treats those as soft / always-OK-when-absent. Mirror that
        // by filtering at parse time so the resolver doesn't report
        // them as UNSAT.
        let h = make_header(vec![
            (TAG_NAME, s("rpm-build-python3")),
            (TAG_VERSION, s("0.1.29")),
            (TAG_RELEASE, s("alt1")),
            (TAG_ARCH, s("noarch")),
            (
                TAG_REQUIRENAME,
                sa(&["python3(py3dephell.py3req)", "python3-base"]),
            ),
            (
                TAG_REQUIREFLAGS,
                i(&[RPMSENSE_MISSINGOK | RPMSENSE_LESS, 0]),
            ),
            (TAG_REQUIREVERSION, sa(&["0", ""])),
        ]);
        let p = package_from_header(&h, &RepoId::from("c")).unwrap();
        // Only the real, non-missingok requirement survives.
        assert_eq!(p.requires.len(), 1, "got: {:?}", p.requires);
        assert_eq!(p.requires[0].name().as_ref(), "python3-base");
    }

    #[test]
    fn skips_contradiction_lt_zero_requires() {
        // ALT's `rpm-build-python3` declares `Requires: python3(NAME) < 0`
        // as a namespace marker. Without RPMSENSE_RPMLIB/MISSINGOK on
        // the entry, the bit-flag filter alone misses it; the
        // `flags == LESS && version in {"0", ""}` filter cleans up
        // the residue.
        let h = make_header(vec![
            (TAG_NAME, s("rpm-build-python3")),
            (TAG_VERSION, s("0.1.29")),
            (TAG_RELEASE, s("alt1")),
            (TAG_ARCH, s("noarch")),
            (
                TAG_REQUIRENAME,
                sa(&["python3(py3dephell.py3req)", "python3-base"]),
            ),
            (TAG_REQUIREFLAGS, i(&[RPMSENSE_LESS, 0])),
            (TAG_REQUIREVERSION, sa(&["0", ""])),
        ]);
        let p = package_from_header(&h, &RepoId::from("c")).unwrap();
        assert_eq!(p.requires.len(), 1, "got: {:?}", p.requires);
        assert_eq!(p.requires[0].name().as_ref(), "python3-base");
    }

    #[test]
    fn rpmlib_filter_also_applies_to_provides() {
        // A package declaring `Provides: rpmlib(Feature)` with
        // RPMSENSE_RPMLIB shouldn't show up in the universe either
        // (rpm itself owns the rpmlib namespace). The filter sits
        // in `capability_triple` so all three sides (provides /
        // requires / conflicts / obsoletes / weak deps) share it.
        let h = make_header(vec![
            (TAG_NAME, s("rpm")),
            (TAG_VERSION, s("4.20")),
            (TAG_RELEASE, s("1")),
            (TAG_ARCH, s("x86_64")),
            (
                TAG_PROVIDENAME,
                sa(&["rpm", "rpmlib(PayloadIsXz)"]),
            ),
            (
                TAG_PROVIDEFLAGS,
                i(&[RPMSENSE_EQUAL, RPMSENSE_RPMLIB | RPMSENSE_EQUAL]),
            ),
            (TAG_PROVIDEVERSION, sa(&["4.20-1", "5.2-1"])),
        ]);
        let p = package_from_header(&h, &RepoId::from("c")).unwrap();
        assert_eq!(p.provides.len(), 1, "got: {:?}", p.provides);
        assert_eq!(p.provides[0].name.as_ref(), "rpm");
    }

    #[test]
    fn triple_length_mismatch_truncates() {
        // names=2, flags=1, versions=2 → keep 1.
        let h = make_header(vec![
            (TAG_NAME, s("a")),
            (TAG_VERSION, s("1")),
            (TAG_RELEASE, s("1")),
            (TAG_ARCH, s("x86_64")),
            (TAG_REQUIRENAME, sa(&["a", "b"])),
            (TAG_REQUIREFLAGS, i(&[0])),
            (TAG_REQUIREVERSION, sa(&["", ""])),
        ]);
        let p = package_from_header(&h, &RepoId::from("c")).unwrap();
        assert_eq!(p.requires.len(), 1);
    }
}
