//! Streaming parser for primary.xml.
//!
//! Constructs one [`rpm_spec_repo_core::Package`] per `<package>`
//! element. The format is dnf-specific but well-documented in
//! `createrepo_c` and pulp; we cover the fields the resolver and
//! lints actually consume.

use std::sync::Arc;

use quick_xml::events::Event;
use quick_xml::Reader;

use rpm_spec_repo_core::{
    CapVersion, Capability, Dependency, EVR, NEVRA, Package, PkgChecksum, RepoError, RepoId,
};

/// Hard cap on the number of `<package>` entries the parser will
/// accept from a single primary.xml. 10× any real-world value
/// (Fedora Everything ships ~60k); the cap defends against hostile
/// or corrupt repos that would otherwise allocate gigabytes of
/// `Package` structs.
pub const MAX_PACKAGES_PER_REPO: usize = 1_000_000;

/// Parse primary.xml bytes into a vec of packages tagged with
/// `repo_id`.
pub fn parse(xml: &[u8], repo_id: RepoId) -> Result<Vec<Package>, RepoError> {
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut packages = Vec::new();
    let mut buf = Vec::new();

    // State for the in-flight package
    let mut in_package = false;
    let mut current = PackageBuilder::default();
    let mut dep_collector: Option<DepKind> = None;
    // Scratch buffer for the most recent `Event::Text` body. The
    // `Text` handler overwrites this unconditionally before the
    // `Field`-based dispatch reads it; the initial empty value never
    // surfaces in `current`.
    #[allow(unused_assignments)]
    let mut last_text = String::new();
    let mut last_field: Option<Field> = None;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| RepoError::parse_at_file("primary.xml", e.to_string()))?
        {
            Event::Start(e) => match e.name().as_ref() {
                b"package" => {
                    in_package = true;
                    current = PackageBuilder::default();
                }
                b"name" if in_package => last_field = Some(Field::Name),
                b"arch" if in_package => last_field = Some(Field::Arch),
                b"summary" if in_package => last_field = Some(Field::Summary),
                b"checksum" if in_package => {
                    // Parse the `type` attribute up-front (single pass), then capture
                    // the text body via `last_field = Some(Field::ChecksumHex)`.
                    for attr in e.attributes().with_checks(false).flatten() {
                        if attr.key.as_ref() == b"type" {
                            current.checksum_algo = std::str::from_utf8(&attr.value)
                                .map_err(|err| RepoError::parse_at_file("primary.xml", format!("checksum type: {err}")))?
                                .to_string();
                        }
                    }
                    last_field = Some(Field::ChecksumHex);
                }
                b"format" if in_package => last_field = None,
                b"rpm:sourcerpm" if in_package => last_field = Some(Field::SourceRpm),
                b"rpm:provides" if in_package => dep_collector = Some(DepKind::Provides),
                b"rpm:requires" if in_package => dep_collector = Some(DepKind::Requires),
                b"rpm:conflicts" if in_package => dep_collector = Some(DepKind::Conflicts),
                b"rpm:obsoletes" if in_package => dep_collector = Some(DepKind::Obsoletes),
                b"rpm:recommends" if in_package => dep_collector = Some(DepKind::Recommends),
                b"rpm:suggests" if in_package => dep_collector = Some(DepKind::Suggests),
                b"rpm:supplements" if in_package => dep_collector = Some(DepKind::Supplements),
                b"rpm:enhances" if in_package => dep_collector = Some(DepKind::Enhances),
                _ => {}
            },
            Event::Empty(e) if in_package => match e.name().as_ref() {
                b"version" => {
                    for attr in e.attributes().with_checks(false).flatten() {
                        let val = std::str::from_utf8(&attr.value)
                            .map_err(|e| RepoError::parse_at_file("primary.xml", format!("version: {e}")))?;
                        match attr.key.as_ref() {
                            // Missing/non-numeric epoch → 0. Matches rpm
                            // semantics (absent epoch == 0) and lines
                            // up with `NEVRA::epoch: u32`.
                            b"epoch" => current.epoch = val.parse::<u32>().unwrap_or(0),
                            b"ver" => current.version = val.to_string(),
                            b"rel" => current.release = val.to_string(),
                            _ => {}
                        }
                    }
                }
                b"size" => {
                    for attr in e.attributes().with_checks(false).flatten() {
                        if attr.key.as_ref() == b"installed" {
                            let s = std::str::from_utf8(&attr.value)
                                .map_err(|err| RepoError::parse_at_file("primary.xml", format!("size@installed for {}: {err}", current.name)))?;
                            current.size_installed = s.parse()
                                .map_err(|err| RepoError::parse_at_file("primary.xml", format!("size@installed for {} = {s:?}: {err}", current.name)))?;
                        }
                    }
                }
                b"location" => {
                    for attr in e.attributes().with_checks(false).flatten() {
                        if attr.key.as_ref() == b"href" {
                            current.location = std::str::from_utf8(&attr.value)
                                .map_err(|e| RepoError::parse_at_file("primary.xml", format!("location: {e}")))?
                                .to_string();
                        }
                    }
                }
                b"rpm:sourcerpm" => {}
                b"rpm:entry" => {
                    if let Some(kind) = dep_collector {
                        let cap = parse_entry(&e)?;
                        match kind {
                            DepKind::Provides => current.provides.push(cap),
                            DepKind::Requires => current.requires.push(cap),
                            DepKind::Conflicts => current.conflicts.push(cap),
                            DepKind::Obsoletes => current.obsoletes.push(cap),
                            DepKind::Recommends => current.recommends.push(cap),
                            DepKind::Suggests => current.suggests.push(cap),
                            DepKind::Supplements => current.supplements.push(cap),
                            DepKind::Enhances => current.enhances.push(cap),
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                let s = t
                    .unescape()
                    .map_err(|e| RepoError::parse_at_file("primary.xml", format!("text decode: {e}")))?;
                last_text = s.into_owned();
                if let Some(field) = last_field {
                    match field {
                        Field::Name => current.name = last_text.clone(),
                        Field::Arch => current.arch = last_text.clone(),
                        Field::Summary => current.summary = last_text.clone(),
                        Field::ChecksumHex => current.checksum_hex = last_text.clone(),
                        Field::SourceRpm => current.source_rpm = Some(last_text.clone()),
                    }
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"name" | b"arch" | b"summary" | b"checksum" | b"rpm:sourcerpm" => {
                    last_field = None;
                }
                b"rpm:provides"
                | b"rpm:requires"
                | b"rpm:conflicts"
                | b"rpm:obsoletes"
                | b"rpm:recommends"
                | b"rpm:suggests"
                | b"rpm:supplements"
                | b"rpm:enhances" => dep_collector = None,
                b"package" => {
                    if in_package {
                        packages.push(current.take().build(repo_id.clone())?);
                        if packages.len() > MAX_PACKAGES_PER_REPO {
                            return Err(RepoError::parse_at_file(
                                "primary.xml",
                                format!(
                                    "package count exceeded {MAX_PACKAGES_PER_REPO} \
                                     (likely hostile or corrupt repo)"
                                ),
                            ));
                        }
                    }
                    in_package = false;
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(packages)
}

#[derive(Debug, Clone, Copy)]
enum Field {
    Name,
    Arch,
    Summary,
    ChecksumHex,
    SourceRpm,
}

#[derive(Debug, Clone, Copy)]
enum DepKind {
    Provides,
    Requires,
    Conflicts,
    Obsoletes,
    Recommends,
    Suggests,
    Supplements,
    Enhances,
}

#[derive(Default, Debug)]
struct PackageBuilder {
    name: String,
    arch: String,
    epoch: u32,
    version: String,
    release: String,
    summary: String,
    size_installed: u64,
    checksum_algo: String,
    checksum_hex: String,
    location: String,
    source_rpm: Option<String>,
    provides: Vec<Capability>,
    requires: Vec<Dependency>,
    conflicts: Vec<Dependency>,
    obsoletes: Vec<Dependency>,
    recommends: Vec<Dependency>,
    suggests: Vec<Dependency>,
    supplements: Vec<Dependency>,
    enhances: Vec<Dependency>,
}

impl PackageBuilder {
    fn take(&mut self) -> Self {
        std::mem::take(self)
    }

    fn build(self, repo_id: RepoId) -> Result<Package, RepoError> {
        if self.name.is_empty() {
            return Err(RepoError::parse_at_file("primary.xml", "package without name"));
        }
        // Route through the validating constructor so a malformed
        // `<rpm:checksum>` (wrong length, non-hex bytes) doesn't
        // silently slip into the universe. Empty algorithm gets the
        // historic "unknown / empty hex" placeholder unchanged
        // because primary.xml DOES occasionally ship an entry with
        // no checksum block (older composers), and we want the
        // package to remain queryable for resolution.
        let checksum = if self.checksum_algo.is_empty() {
            PkgChecksum::Other {
                algo: "unknown".into(),
                hex: String::new(),
            }
        } else {
            PkgChecksum::try_new(&self.checksum_algo, &self.checksum_hex).map_err(|e| {
                // Wrap with `parse_at_file` so the diagnostic carries
                // the primary.xml file context alongside the NEVRA in
                // detail — matches the rest of this parser's call
                // sites and keeps grep-greppable output uniform.
                RepoError::parse_at_file(
                    "primary.xml",
                    format!(
                        "package `{}-{}-{}.{}` has invalid checksum: {e}",
                        self.name, self.version, self.release, self.arch
                    ),
                )
            })?
        };

        Ok(Package {
            nevra: NEVRA {
                name: Arc::from(self.name),
                epoch: self.epoch,
                version: Arc::from(self.version),
                release: Arc::from(self.release),
                arch: Arc::from(self.arch),
            },
            repo_id,
            provides: self.provides,
            requires: self.requires,
            conflicts: self.conflicts,
            obsoletes: self.obsoletes,
            recommends: self.recommends,
            suggests: self.suggests,
            supplements: self.supplements,
            enhances: self.enhances,
            source_rpm: self.source_rpm.map(Arc::from),
            summary: Arc::from(self.summary),
            size_installed: self.size_installed,
            checksum,
            location: Arc::from(self.location),
            files: Vec::new(),
        })
    }
}

fn parse_entry(e: &quick_xml::events::BytesStart) -> Result<Capability, RepoError> {
    let mut name = String::new();
    /// Local enum-of-strings — lets us validate flag + EVR in one pass
    /// before assembling the final `CapVersion`. The "unknown flags
    /// token degrades to Unversioned" policy is preserved verbatim.
    enum Op {
        None,
        Eq,
        Lt,
        Le,
        Gt,
        Ge,
    }
    let mut op = Op::None;
    let mut epoch: Option<u32> = None;
    let mut ver: Option<String> = None;
    let mut rel: Option<String> = None;

    for attr in e.attributes().with_checks(false).flatten() {
        let val = std::str::from_utf8(&attr.value)
            .map_err(|e| RepoError::parse_at_file("primary.xml", format!("entry attr: {e}")))?;
        match attr.key.as_ref() {
            b"name" => name = val.to_string(),
            b"flags" => {
                op = match val {
                    "EQ" => Op::Eq,
                    "LT" => Op::Lt,
                    "LE" => Op::Le,
                    "GT" => Op::Gt,
                    "GE" => Op::Ge,
                    _ => Op::None,
                };
            }
            b"epoch" => epoch = val.parse::<u32>().ok(),
            b"ver" => ver = Some(val.to_string()),
            b"rel" => rel = Some(val.to_string()),
            _ => {}
        }
    }

    // Build EVR only when version is present. Missing release on a
    // versioned entry is real (some `<rpm:entry>` ship only `ver=`);
    // empty-release wildcard semantics in `EVR::compare_rpm` handle
    // those at lookup time without losing the version constraint.
    let evr_opt = ver.map(|v| EVR::new(epoch, v, rel.unwrap_or_default()));
    let version = match (op, evr_opt) {
        (Op::None, _) => CapVersion::Unversioned,
        (Op::Eq, Some(e)) => CapVersion::Eq(e),
        (Op::Lt, Some(e)) => CapVersion::Lt(e),
        (Op::Le, Some(e)) => CapVersion::Le(e),
        (Op::Gt, Some(e)) => CapVersion::Gt(e),
        (Op::Ge, Some(e)) => CapVersion::Ge(e),
        // Versioned op without EVR is malformed; degrade to
        // Unversioned so the lookup at least keeps the package
        // queryable by name (matches dnf's tolerance).
        (_, None) => CapVersion::Unversioned,
    };

    Ok(Capability {
        name: Arc::from(name),
        version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/repos/rpm-md/tiny-fedora/repodata/primary.xml"
    ));

    #[test]
    fn parses_packages_with_provides_and_requires() {
        let packages = parse(FIXTURE.as_bytes(), RepoId::from("test")).unwrap();
        assert_eq!(packages.len(), 3, "expected 3 packages");

        let bash = packages
            .iter()
            .find(|p| p.nevra.name.as_ref() == "bash")
            .expect("bash present");
        assert_eq!(bash.nevra.version.as_ref(), "5.1.8");
        assert_eq!(bash.nevra.release.as_ref(), "9.el9");
        assert_eq!(bash.nevra.arch.as_ref(), "x86_64");
        assert_eq!(bash.provides.len(), 3, "bash provides bash + /bin/bash + /bin/sh");
        assert_eq!(bash.requires.len(), 1, "bash requires glibc");
        assert_eq!(bash.requires[0].name.as_ref(), "glibc");
        assert_eq!(bash.source_rpm.as_deref(), Some("bash-5.1.8-9.el9.src.rpm"));
        assert_eq!(bash.summary.as_ref(), "The GNU Bourne Again shell");
        assert_eq!(bash.size_installed, 6_500_000);
        assert_eq!(bash.location.as_ref(), "Packages/b/bash-5.1.8-9.el9.x86_64.rpm");
        assert!(
            // Fixture uses sha256("bash") = 37d2b…c6c2 (real digest,
            // not just a length-correct placeholder, so the test
            // doubles as an end-to-end check that hex normalisation
            // and casing pass through unchanged).
            matches!(
                &bash.checksum,
                PkgChecksum::Sha256(hex)
                    if hex == "37d2b12d5d9abc2a364ef9448767ee03938e383c0284193477dc7618f4b7c6c2"
            ),
            "expected bash sha256, got {:?}",
            bash.checksum,
        );

        let cmake = packages
            .iter()
            .find(|p| p.nevra.name.as_ref() == "cmake")
            .expect("cmake present");
        // cmake's Requires references plain `bash` (no version constraint)
        assert_eq!(cmake.requires.len(), 1);
        assert!(cmake.requires[0].version.is_unversioned());
    }
}
