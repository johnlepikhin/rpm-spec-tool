//! Parser for `base/contents_index` — apt-rpm's file → owner map.
//!
//! Text format, one entry per line: `<path>\t<package_name>\n`.
//! Real-world ALT mirrors ship this **uncompressed** (xz here would
//! be wasteful — paths compress well in xz but the file is read
//! sequentially exactly once by `repo sync`, so trading disk for
//! decompression CPU isn't a win).
//!
//! `<package_name>` is the package's **NAME** (no version, no arch),
//! matching what `pkglist` records in `TAG_NAME`. That makes the
//! merge into the [`Package::files`] vec a simple
//! `HashMap<name, Vec<path>>` build + per-package vec swap.
//!
//! The full p10/x86_64 contents_index is ~50–60 MB and ~2.5M lines.
//! We stream-parse: one allocation per line (the path), no full-file
//! Vec build, so memory stays bounded by the longest pathname.

use std::collections::HashMap;
use std::sync::Arc;

use super::error::AptRpmParseError;

/// Returned by [`parse`]. Keys are package NAME, values are the
/// owned paths in file-system order (the order is preserved exactly
/// as it appears in the source so downstream `Package.files` lists
/// are diff-stable across re-syncs of the same revision).
pub type FileMap = HashMap<Arc<str>, Vec<Arc<str>>>;

/// Parse a `contents_index` body into a per-package file map.
///
/// Lines are TAB-separated `path\tpackage`. Blank lines and lines
/// without a TAB are skipped silently — these don't occur in real
/// fixtures but ALT's tooling could emit them in a future format
/// revision and we don't want a no-op line to abort the whole parse.
///
/// A malformed line (e.g. empty path / empty package after the TAB)
/// returns [`AptRpmParseError::BadContentsIndexLine`] with the
/// 1-based line number.
///
/// # Errors
///
/// See above.
pub fn parse(text: &str) -> Result<FileMap, AptRpmParseError> {
    // Most ALT packages own dozens-to-hundreds of files; pre-size
    // the map so the typical case doesn't rehash 8× as it grows.
    let mut out: FileMap = HashMap::with_capacity(8192);
    for (idx, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim_end_matches(['\r']);
        if line.is_empty() {
            continue;
        }
        let Some((path, pkg)) = line.split_once('\t') else {
            // A line without TAB is degenerate (ALT format is
            // strictly TAB-separated). Skip silently — being
            // strict here would block a sync over a single
            // garbled byte in a multi-megabyte fixture.
            continue;
        };
        let path = path.trim();
        let pkg = pkg.trim();
        if path.is_empty() || pkg.is_empty() {
            return Err(AptRpmParseError::BadContentsIndexLine {
                line: idx + 1,
                detail: format!("empty path or owner on line: {line:?}"),
            });
        }
        out.entry(Arc::from(pkg)).or_default().push(Arc::from(path));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_pairs() {
        let text = "/bin/sh\tbash\n\
                    /usr/bin/perl\tperl-interpreter\n\
                    /usr/share/foo\tfoobar\n";
        let m = parse(text).unwrap();
        assert_eq!(m.len(), 3);
        assert_eq!(
            m[&Arc::<str>::from("bash")],
            vec![Arc::<str>::from("/bin/sh")]
        );
    }

    #[test]
    fn aggregates_per_package() {
        let text = "/usr/bin/foo\tfoo\n\
                    /usr/share/foo/data\tfoo\n\
                    /etc/foo.conf\tfoo\n";
        let m = parse(text).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[&Arc::<str>::from("foo")].len(), 3);
    }

    #[test]
    fn preserves_file_order_per_package() {
        let text = "/a\tpkg\n/b\tpkg\n/c\tpkg\n";
        let m = parse(text).unwrap();
        let files: Vec<&str> = m[&Arc::<str>::from("pkg")]
            .iter()
            .map(|s| s.as_ref())
            .collect();
        assert_eq!(files, vec!["/a", "/b", "/c"]);
    }

    #[test]
    fn rejects_empty_owner_on_otherwise_valid_line() {
        let text = "/path\t\n";
        assert!(matches!(
            parse(text),
            Err(AptRpmParseError::BadContentsIndexLine { line: 1, .. })
        ));
    }

    #[test]
    fn skips_lines_without_tab() {
        let text = "no tab here\n/real/path\tpkg\n";
        let m = parse(text).unwrap();
        assert_eq!(m.len(), 1);
        assert!(m.contains_key(&Arc::<str>::from("pkg")));
    }

    #[test]
    fn handles_crlf_lines() {
        let text = "/bin/sh\tbash\r\n/usr/bin/foo\tfoo\r\n";
        let m = parse(text).unwrap();
        assert_eq!(m.len(), 2);
    }
}
