//! RPM020 `obsolete-tag` — flag preamble tags that modern distros forbid
//! or rpm itself fills in automatically.
//!
//! Auto-fix: drop the offending line. The parser stores a span covering
//! the entire preamble line on `PreambleItem.data`, so the replacement
//! is empty-string.

use rpm_spec::ast::{PreambleItem, Span, Tag};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::drop_span;
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM020",
    name: "obsolete-tag",
    description: "Preamble uses a tag that's deprecated or forbidden by modern packaging guidelines.",
    default_severity: Severity::Warn,
    category: LintCategory::Packaging,
};

/// Legacy tag names that arrive as `Tag::Other(_)` because the parser
/// preserves them verbatim. Matched case-insensitively because rpm
/// itself treats preamble tag names that way.
const OBSOLETE_OTHER_TAGS: &[(&str, &str)] = &[
    (
        "Copyright",
        "Copyright was renamed to License in rpm 4.0 (2000-09)",
    ),
    (
        "Serial",
        "Serial was replaced by Epoch in rpm 3.0 (1999-04)",
    ),
    (
        "PreReq",
        "PreReq is deprecated since rpm 4.4 (2005-09); use Requires",
    ),
    (
        "BuildPreReq",
        "BuildPreReq is deprecated since rpm 4.4 (2005-09); use BuildRequires",
    ),
];

#[derive(Debug, Default)]
pub struct ObsoleteTag {
    diagnostics: Vec<Diagnostic>,
}

impl ObsoleteTag {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Return the human-readable reason if `tag` is obsolete, `None` otherwise.
///
/// Each message names the rpm version that made the tag redundant (and
/// its release date) so the maintainer can quickly judge whether
/// dropping the tag is safe for the distributions they care about.
fn obsolete_reason(tag: &Tag) -> Option<&'static str> {
    match tag {
        Tag::BuildRoot => Some(
            "BuildRoot is set automatically by rpm ≥ 4.6 (released 2009-02); \
             every supported distribution ships a newer rpm",
        ),
        Tag::Packager => Some(
            "Packager is forbidden by the Fedora Packaging Guidelines \
             (the field is set by the build system, not the spec)",
        ),
        Tag::Vendor => Some(
            "Vendor is forbidden by the Fedora Packaging Guidelines \
             (the field is set by the build system, not the spec)",
        ),
        Tag::Other(name) => OBSOLETE_OTHER_TAGS
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, msg)| *msg),
        _ => None,
    }
}

impl<'ast> Visit<'ast> for ObsoleteTag {
    fn visit_preamble(&mut self, node: &'ast PreambleItem<Span>) {
        if let Some(reason) = obsolete_reason(&node.tag) {
            let diag = Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!("{reason}; remove this line"),
                node.data,
            )
            .with_suggestion(Suggestion::new(
                "drop the obsolete tag",
                vec![drop_span(node.data)],
                Applicability::MachineApplicable,
            ));
            self.diagnostics.push(diag);
        }
        visit::walk_preamble(self, node);
    }
}

impl Lint for ObsoleteTag {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }

    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = ObsoleteTag::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_buildroot_tag() {
        let diags = run("Name: x\nBuildRoot: %{_tmppath}/foo\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM020");
        assert!(diags[0].message.contains("BuildRoot"));
        assert!(!diags[0].suggestions.is_empty());
    }

    #[test]
    fn flags_packager_and_vendor() {
        let diags = run("Name: x\nPackager: who\nVendor: us\n");
        assert_eq!(diags.len(), 2);
        assert!(diags.iter().all(|d| d.lint_id == "RPM020"));
    }

    #[test]
    fn flags_legacy_copyright() {
        let diags = run("Name: x\nCopyright: MIT\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Copyright"));
    }

    #[test]
    fn flags_legacy_serial() {
        let diags = run("Name: x\nSerial: 1\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Serial"));
    }

    #[test]
    fn flags_legacy_prereq() {
        let diags = run("Name: x\nPreReq: bash\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("PreReq"));
    }

    #[test]
    fn flags_legacy_buildprereq() {
        let diags = run("Name: x\nBuildPreReq: gcc\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("BuildPreReq"));
    }

    #[test]
    fn matches_case_insensitively() {
        // rpm itself accepts preamble tag names case-insensitively, so
        // `copyright:` should still flag.
        let diags = run("Name: x\ncopyright: MIT\n");
        assert_eq!(diags.len(), 1, "lowercase 'copyright' should be flagged");
    }

    #[test]
    fn silent_when_modern_tags_only() {
        let diags = run("Name: x\nVersion: 1\nLicense: MIT\n");
        assert!(diags.is_empty(), "{diags:?}");
    }
}
