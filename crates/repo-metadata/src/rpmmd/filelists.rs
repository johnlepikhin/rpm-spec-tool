//! Streaming parser for filelists.xml. Merges file lists into the
//! Package vector already produced by primary.xml.

use std::collections::HashMap;
use std::sync::Arc;

use quick_xml::Reader;
use quick_xml::events::Event;

use rpm_spec_repo_core::{Package, RepoError};

/// Hard cap on `<file>` entries per `<package>`. Real-world
/// JS/Python frameworks (kibana ships node_modules, llvm ships
/// debuginfo, etc.) routinely exceed 100k; the cap mostly protects
/// against hostile filelists rather than capping legitimate data.
/// 1M is roughly 120 MB of `Arc<str>` per package — still bounded
/// and matches `MAX_PACKAGES_PER_REPO`'s ceiling.
pub const MAX_FILES_PER_PACKAGE: usize = 1_000_000;

/// Merge filelists into pre-parsed packages. Keyed by package
/// `pkgid` (== rpm checksum hex) when available; otherwise by name
/// (acceptable for fixture tests where there's no ambiguity).
pub fn merge(xml: &[u8], packages: &mut [Package]) -> Result<(), RepoError> {
    // Build O(1) lookup tables once instead of scanning `packages`
    // linearly per `</package>` close (was O(N^2): ~3.6B comparisons
    // on a 60k-package Fedora-class repo).
    let mut by_pkgid: HashMap<String, usize> = HashMap::with_capacity(packages.len());
    let mut by_name_idx: HashMap<String, usize> = HashMap::with_capacity(packages.len());
    for (idx, pkg) in packages.iter().enumerate() {
        let hex = match &pkg.checksum {
            rpm_spec_repo_core::PkgChecksum::Sha256(h)
            | rpm_spec_repo_core::PkgChecksum::Sha1(h) => h.clone(),
            rpm_spec_repo_core::PkgChecksum::Other { hex, .. } => hex.clone(),
        };
        if !hex.is_empty() {
            by_pkgid.insert(hex, idx);
        }
        by_name_idx.insert(pkg.nevra.name.to_string(), idx);
    }

    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut current_pkgid: Option<String> = None;
    let mut current_name: Option<String> = None;
    let mut collecting_file = false;
    let mut file_text = String::new();
    let mut files_for_pkg: Vec<Arc<str>> = Vec::new();

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| RepoError::parse_at_file("filelists.xml", format!("{e}")))?
        {
            Event::Start(e) if e.name().as_ref() == b"package" => {
                current_pkgid = None;
                current_name = None;
                files_for_pkg.clear();
                for attr in e.attributes().with_checks(false).flatten() {
                    let val = std::str::from_utf8(&attr.value).map_err(|e| {
                        RepoError::parse_at_file("filelists.xml", format!("attr: {e}"))
                    })?;
                    match attr.key.as_ref() {
                        b"pkgid" => current_pkgid = Some(val.to_string()),
                        b"name" => current_name = Some(val.to_string()),
                        _ => {}
                    }
                }
            }
            Event::Start(e) if e.name().as_ref() == b"file" => {
                collecting_file = true;
                file_text.clear();
            }
            Event::Text(t) if collecting_file => {
                file_text.push_str(&t.unescape().map_err(|e| {
                    RepoError::parse_at_file("filelists.xml", format!("text: {e}"))
                })?);
            }
            Event::End(e) if e.name().as_ref() == b"file" => {
                if collecting_file {
                    files_for_pkg.push(Arc::from(file_text.clone()));
                    if files_for_pkg.len() > MAX_FILES_PER_PACKAGE {
                        return Err(RepoError::parse_at_file(
                            "filelists.xml",
                            format!(
                                "package {name:?} has more than {MAX_FILES_PER_PACKAGE} files \
                                 (likely hostile or corrupt repo)",
                                name = current_name.as_deref().unwrap_or("<unknown>"),
                            ),
                        ));
                    }
                }
                collecting_file = false;
                file_text.clear();
            }
            Event::End(e) if e.name().as_ref() == b"package" => {
                let idx = current_pkgid
                    .as_deref()
                    .and_then(|id| by_pkgid.get(id).copied())
                    .or_else(|| {
                        current_name
                            .as_deref()
                            .and_then(|n| by_name_idx.get(n).copied())
                    });
                if let Some(i) = idx
                    && let Some(target) = packages.get_mut(i)
                {
                    target.files = std::mem::take(&mut files_for_pkg);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_repo_core::{NEVRA, Package, PkgChecksum, RepoId};
    use std::sync::Arc;

    fn pkg(name: &str, pkgid: &str) -> Package {
        Package {
            nevra: NEVRA {
                name: Arc::from(name),
                epoch: 0,
                version: Arc::from("1.0"),
                release: Arc::from("1"),
                arch: Arc::from("x86_64"),
            },
            repo_id: RepoId::from("test"),
            provides: Vec::new(),
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
            checksum: PkgChecksum::Sha256(pkgid.to_string()),
            location: Arc::from(""),
            files: Vec::new(),
        }
    }

    const TINY: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<filelists xmlns="http://linux.duke.edu/metadata/filelists" packages="1">
  <package pkgid="abc123" name="bash" arch="x86_64">
    <version epoch="0" ver="5.1.8" rel="9.el9"/>
    <file>/bin/bash</file>
    <file>/usr/share/man/man1/bash.1.gz</file>
  </package>
</filelists>
"#;

    #[test]
    fn merge_by_pkgid() {
        let mut packages = vec![pkg("bash", "abc123")];
        merge(TINY.as_bytes(), &mut packages).unwrap();
        assert_eq!(packages[0].files.len(), 2);
        assert_eq!(packages[0].files[0].as_ref(), "/bin/bash");
    }

    #[test]
    fn merge_by_name_fallback() {
        let mut packages = vec![pkg("bash", "other-checksum")];
        merge(TINY.as_bytes(), &mut packages).unwrap();
        assert_eq!(packages[0].files.len(), 2, "fallback to name match");
    }

    #[test]
    fn unknown_package_is_skipped_silently() {
        let mut packages = vec![pkg("not-bash", "different")];
        merge(TINY.as_bytes(), &mut packages).unwrap();
        assert!(packages[0].files.is_empty(), "no match: untouched");
    }
}
