//! Parse `repodata/repomd.xml` into a list of `(data_type, location)`
//! pointers. Drops checksum and timestamp metadata in M1 — `repo
//! sync` re-hashes anyway, and the revision is the sha of the file
//! itself rather than any sub-checksum.

use quick_xml::Reader;
use quick_xml::events::Event;

use rpm_spec_repo_core::RepoError;

/// Index of `<data type="X">` entries from `repomd.xml`.
#[derive(Debug, Clone, Default)]
pub struct Repomd {
    entries: Vec<(String, String)>, // (type, location.href)
}

impl Repomd {
    /// Resolve a data type (`primary`, `filelists`, `updateinfo`, ...)
    /// to its repo-relative `location.href`. `None` when the repo
    /// doesn't ship that data file.
    #[must_use]
    pub fn location_for(&self, kind: &str) -> Option<String> {
        self.entries
            .iter()
            .find(|(k, _)| k == kind)
            .map(|(_, loc)| loc.clone())
    }
}

/// Parse repomd.xml bytes.
pub fn parse(bytes: &[u8]) -> Result<Repomd, RepoError> {
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    let mut entries = Vec::new();
    let mut current_type: Option<String> = None;

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| RepoError::parse_at_file("repomd.xml", format!("{e}")))?
        {
            Event::Start(e) if e.name().as_ref() == b"data" => {
                for attr in e.attributes().with_checks(false).flatten() {
                    if attr.key.as_ref() == b"type" {
                        current_type = Some(
                            std::str::from_utf8(&attr.value)
                                .map_err(|e| {
                                    RepoError::parse_at_file("repomd.xml", format!("type: {e}"))
                                })?
                                .to_string(),
                        );
                    }
                }
            }
            #[allow(clippy::collapsible_match)]
            Event::Empty(e) if e.name().as_ref() == b"location" => {
                if let Some(ct) = current_type.clone() {
                    for attr in e.attributes().with_checks(false).flatten() {
                        if attr.key.as_ref() == b"href" {
                            let href = std::str::from_utf8(&attr.value)
                                .map_err(|e| {
                                    RepoError::parse_at_file("repomd.xml", format!("href: {e}"))
                                })?
                                .to_string();
                            if href.contains("://")
                                || href.contains("..")
                                || href.starts_with('/')
                                || href.chars().any(|c| c.is_control() || c == '\\')
                            {
                                return Err(RepoError::parse_at_file(
                                    "repomd.xml",
                                    format!("rejected suspicious href {href:?} for data type {ct}"),
                                ));
                            }
                            entries.push((ct.clone(), href));
                        }
                    }
                }
            }
            Event::End(e) if e.name().as_ref() == b"data" => {
                current_type = None;
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(Repomd { entries })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<repomd xmlns="http://linux.duke.edu/metadata/repo">
  <revision>1715000000</revision>
  <data type="primary">
    <location href="repodata/primary.xml.gz"/>
  </data>
  <data type="filelists">
    <location href="repodata/filelists.xml.gz"/>
  </data>
  <data type="updateinfo">
    <location href="repodata/updateinfo.xml.gz"/>
  </data>
</repomd>
"#;

    #[test]
    fn parses_data_locations() {
        let r = parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(
            r.location_for("primary").as_deref(),
            Some("repodata/primary.xml.gz")
        );
        assert_eq!(
            r.location_for("filelists").as_deref(),
            Some("repodata/filelists.xml.gz")
        );
        assert_eq!(
            r.location_for("updateinfo").as_deref(),
            Some("repodata/updateinfo.xml.gz")
        );
        assert!(r.location_for("missing").is_none());
    }

    /// Wrap a single `<data type="primary"><location href="..."/></data>` block
    /// in a minimal valid repomd.xml. Keeps the table-driven href-validation
    /// tests below readable.
    fn sample_with_href(href: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<repomd xmlns="http://linux.duke.edu/metadata/repo">
  <revision>1</revision>
  <data type="primary">
    <location href="{href}"/>
  </data>
</repomd>
"#
        )
    }

    fn assert_rejected(href: &str) {
        let xml = sample_with_href(href);
        let err = parse(xml.as_bytes()).expect_err(&format!("expected rejection for {href:?}"));
        match err {
            RepoError::Parse(loc) => assert!(
                loc.detail().contains("rejected suspicious href"),
                "unexpected message for {href:?}: {}",
                loc.detail()
            ),
            other => panic!("expected Parse error for {href:?}, got {other:?}"),
        }
    }

    #[test]
    fn rejects_scheme_in_href() {
        assert_rejected("http://attacker.com/x.xml.gz");
    }

    #[test]
    fn rejects_parent_traversal() {
        assert_rejected("../../../etc/passwd");
    }

    #[test]
    fn rejects_absolute_path() {
        assert_rejected("/etc/passwd");
    }

    #[test]
    fn rejects_control_char() {
        assert_rejected("repodata/primary.xml.gz\u{0a}");
    }

    #[test]
    fn rejects_backslash() {
        assert_rejected("repodata\\primary.xml.gz");
    }

    #[test]
    fn accepts_simple_relative() {
        let xml = sample_with_href("repodata/primary.xml.gz");
        let r = parse(xml.as_bytes()).expect("simple relative href must parse");
        assert_eq!(
            r.location_for("primary").as_deref(),
            Some("repodata/primary.xml.gz")
        );
    }
}
