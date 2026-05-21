//! Parser for the apt-rpm `base/release` text file.
//!
//! Debian-style `key: value` block followed by an `MD5Sum:` table
//! listing component metadata files. The backend only needs the
//! top-level fields for revision identity — the `MD5Sum:` table is
//! consumed for documentation but the actual integrity check is the
//! sha256 we compute over the whole file (revision_from).
//!
//! Example:
//!
//! ```text
//! Origin: ALT Linux Team
//! Label: p10
//! Suite: p10
//! Codename: 1779090488
//! Date: Mon, 18 May 2026 07:48:41 +0000
//! Architectures: x86_64 i586 aarch64 armh noarch
//! Components: checkinstall classic debuginfo gostcrypto
//! MD5Sum:
//!  2a302598dcead80c989aa1c2fb2b7219 240792 base/pkglist.checkinstall
//!  ...
//! ```
//!
//! Only the top-level fields are public — the MD5 table is parsed
//! into [`ReleaseFile::checksums`] for completeness but the backend
//! currently doesn't validate against it (we trust the sha256 of the
//! whole release file as the snapshot identity instead).

use super::error::AptRpmParseError;

/// Top-level parse result. `Codename` / `Suite` are optional because
/// some ALT mirrors omit them on the per-component `release.foo`
/// variants. The required fields are `Origin` (signals "this is
/// genuinely an ALT release file"), at least one architecture, and
/// at least one component — without those, downstream code can't
/// figure out what to fetch.
///
/// Most fields are currently consumed only by `fetch_revision`
/// (which calls `parse()` purely for validation — a real release
/// file is the snapshot identity, not its individual fields). The
/// fields stay on the public API for `repo show` (M3 PR7+) and
/// future tooling that wants to display human-readable summary of
/// the cached snapshot; suppress dead-code warnings until then.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ReleaseFile {
    pub origin: String,
    pub label: Option<String>,
    pub suite: Option<String>,
    pub codename: Option<String>,
    pub date: Option<String>,
    pub architectures: Vec<String>,
    pub components: Vec<String>,
    /// Files listed under `MD5Sum:` — each entry is
    /// `(md5_hex, size_bytes, relative_path)`. Sorted by path for
    /// determinism so callers iterating the list see stable output.
    pub checksums: Vec<ChecksumEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChecksumEntry {
    pub md5_hex: String,
    pub size: u64,
    pub path: String,
}

/// Parse `text` (the full contents of `base/release`).
///
/// # Errors
///
/// Returns [`AptRpmParseError::BadReleaseFile`] when the file is
/// empty, when `Origin:` is missing, or when no `Architectures:` /
/// `Components:` field provides at least one value.
pub fn parse(text: &str) -> Result<ReleaseFile, AptRpmParseError> {
    let mut origin: Option<String> = None;
    let mut label: Option<String> = None;
    let mut suite: Option<String> = None;
    let mut codename: Option<String> = None;
    let mut date: Option<String> = None;
    let mut architectures: Vec<String> = Vec::new();
    let mut components: Vec<String> = Vec::new();
    let mut checksums: Vec<ChecksumEntry> = Vec::new();

    let mut in_md5 = false;
    for raw_line in text.lines() {
        // MD5Sum entries are indented (per the Debian rules-file
        // tradition). Any line starting with whitespace inside the
        // MD5Sum block belongs to that table; once we hit a
        // non-indented line we exit MD5Sum.
        let leading_ws = raw_line.starts_with(' ') || raw_line.starts_with('\t');
        if in_md5 && leading_ws {
            if let Some(entry) = parse_checksum_line(raw_line.trim_start()) {
                checksums.push(entry);
            }
            continue;
        }
        in_md5 = false;

        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "Origin" => origin = Some(value.to_string()),
            "Label" => label = Some(value.to_string()),
            "Suite" => suite = Some(value.to_string()),
            "Codename" => codename = Some(value.to_string()),
            "Date" => date = Some(value.to_string()),
            "Architectures" => {
                architectures = value.split_whitespace().map(str::to_string).collect();
            }
            // Per-component `release.<component>` files use the
            // singular `Architecture: <single-arch>`. Treat it as a
            // 1-element architectures list so callers don't need
            // two different code paths.
            "Architecture" if !value.is_empty() => {
                architectures = vec![value.to_string()];
            }
            "Components" => {
                components = value.split_whitespace().map(str::to_string).collect();
            }
            "MD5Sum" => {
                in_md5 = true;
            }
            // Future ALT releases may add SHA256: / SHA1: blocks;
            // ignore unknown keys silently rather than fail (forward-
            // compat: a new optional field shouldn't break old tools).
            _ => {}
        }
    }

    let Some(origin) = origin else {
        return Err(AptRpmParseError::BadReleaseFile {
            detail: "`Origin:` field is missing".into(),
        });
    };
    if architectures.is_empty() {
        return Err(AptRpmParseError::BadReleaseFile {
            detail: "`Architectures:` field is missing or empty".into(),
        });
    }
    // Per-component `release.foo` variants don't always carry a
    // Components: line — they implicitly cover the single component
    // named in the filename. We don't fail on missing Components,
    // but the top-level `release` SHOULD have it; the caller can
    // distinguish via `components.is_empty()` if it cares.

    checksums.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(ReleaseFile {
        origin,
        label,
        suite,
        codename,
        date,
        architectures,
        components,
        checksums,
    })
}

fn parse_checksum_line(line: &str) -> Option<ChecksumEntry> {
    // Format: "<md5_hex> <size> <path>" with whitespace separator.
    let mut parts = line.split_whitespace();
    let md5 = parts.next()?;
    let size_str = parts.next()?;
    let path = parts.collect::<Vec<_>>().join(" ");
    if path.is_empty() {
        return None;
    }
    let size = size_str.parse::<u64>().ok()?;
    // md5 is 32 hex chars; cheap sanity check, drop entry if not.
    if md5.len() != 32 || !md5.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(ChecksumEntry {
        md5_hex: md5.to_string(),
        size,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Origin: ALT Linux Team\n\
                          Label: p10\n\
                          Suite: p10\n\
                          Codename: 1779090488\n\
                          Date: Mon, 18 May 2026 07:48:41 +0000\n\
                          Architectures: x86_64 i586 aarch64 armh noarch\n\
                          Components: checkinstall classic debuginfo gostcrypto\n\
                          Description: ALT Linux p10\n\
                          MD5Sum:\n\
                          \x202a302598dcead80c989aa1c2fb2b7219 240792 base/pkglist.checkinstall\n\
                          \x20fdaee213576b734d425111551ec0c720 128497214 base/pkglist.classic\n";

    #[test]
    fn parses_full_release() {
        let r = parse(SAMPLE).unwrap();
        assert_eq!(r.origin, "ALT Linux Team");
        assert_eq!(r.label.as_deref(), Some("p10"));
        assert_eq!(
            r.architectures,
            vec!["x86_64", "i586", "aarch64", "armh", "noarch"]
        );
        assert_eq!(
            r.components,
            vec!["checkinstall", "classic", "debuginfo", "gostcrypto"]
        );
        assert_eq!(r.checksums.len(), 2);
        // Sorted by path → checkinstall comes before classic.
        assert_eq!(r.checksums[0].path, "base/pkglist.checkinstall");
        assert_eq!(r.checksums[0].size, 240792);
        assert_eq!(r.checksums[1].path, "base/pkglist.classic");
    }

    #[test]
    fn rejects_missing_origin() {
        let s = "Architectures: x86_64\nComponents: classic\n";
        assert!(matches!(
            parse(s),
            Err(AptRpmParseError::BadReleaseFile { .. })
        ));
    }

    #[test]
    fn rejects_empty_arch() {
        let s = "Origin: x\nArchitectures:\nComponents: classic\n";
        assert!(matches!(
            parse(s),
            Err(AptRpmParseError::BadReleaseFile { .. })
        ));
    }

    #[test]
    fn per_component_release_without_components_ok() {
        // Per-component release.checkinstall doesn't list Components.
        let s = "Origin: ALT Linux Team\n\
                 Archive: ALT Linux p10\n\
                 Component: checkinstall\n\
                 Version: 1779090488\n\
                 Label: p10\n\
                 Architecture: x86_64\n\
                 NotAutomatic: false\n\
                 Architectures: x86_64\n";
        let r = parse(s).unwrap();
        assert_eq!(r.origin, "ALT Linux Team");
        assert!(r.components.is_empty());
    }

    #[test]
    fn skips_malformed_checksum_lines() {
        // First line: bad md5 length, second line: ok.
        let s = "Origin: x\nArchitectures: x86_64\nMD5Sum:\n\
                 \x20short 100 base/foo\n\
                 \x202a302598dcead80c989aa1c2fb2b7219 200 base/bar\n";
        let r = parse(s).unwrap();
        assert_eq!(r.checksums.len(), 1);
        assert_eq!(r.checksums[0].path, "base/bar");
    }
}
