//! RPM050 `hardcoded-paths` — flag literal absolute paths like
//! `/usr/bin` / `/etc` / `/var/log` that have a well-defined RPM macro
//! equivalent. Hardcoding them defeats rpm's path-relocation knobs
//! (`--prefix`, `_libdir` overrides, alternative install layouts).
//!
//! ## Scope
//!
//! We deliberately *do not* touch a few tag kinds where literal paths
//! are usually legitimate:
//! - `Source`, `Patch`, `URL` — upstream URLs / source paths.
//! - `Summary`, `License`, `Group` — free-form text, not paths.
//! - Dependency tags (`Requires`, `BuildRequires`, `Provides`, …) —
//!   absolute paths here are RPM's canonical file-based dependency
//!   idiom (`Requires: /usr/sbin/useradd` resolves through the file
//!   provider mechanism). Rewriting to `%{_sbindir}/useradd` does
//!   nothing useful; the proper fix (`Requires(pre): shadow-utils`)
//!   belongs to a separate rule.
//!
//! Everywhere else (`%files` entries, shell-script bodies) we suggest
//! the macro replacement.
//!
//! ## Span precision
//!
//! `TextSegment` doesn't carry per-segment spans, so we don't try to
//! anchor on the AST. Instead the rule scans the original source slice
//! covered by the enclosing anchor (preamble line, file entry, shell
//! body) and emits one diagnostic per occurrence with a precise
//! sub-span pointing at the matched path.

use rpm_spec::ast::{FileEntry, PreambleItem, Scriptlet, Section, Span, Tag, Trigger};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::match_path_prefix;
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM050",
    name: "hardcoded-paths",
    description: "Use the matching RPM macro instead of a hardcoded path (e.g. `%{_bindir}` for `/usr/bin`).",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct HardcodedPaths {
    diagnostics: Vec<Diagnostic>,
    /// Raw source bytes, set via [`Lint::set_source`] before each pass.
    /// Required because the rule scans the source slice covered by an
    /// anchor span to compute precise per-occurrence sub-spans.
    source: Option<String>,
}

impl HardcodedPaths {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scan the source slice covered by `anchor` and emit one
    /// diagnostic per matched hardcoded path.
    fn scan_anchor(&mut self, anchor: Span) {
        let Some(source) = &self.source else { return };
        let end = anchor.end_byte.min(source.len());
        let start = anchor.start_byte.min(end);
        // `source.get` returns `None` if either bound falls between
        // UTF-8 code-point boundaries — protect against malformed
        // spans rather than panicking inside a library call.
        let Some(slice) = source.get(start..end) else {
            return;
        };

        let mut idx = 0;
        while let Some(slash_offset) = slice[idx..].find('/') {
            let slash_pos = idx + slash_offset;
            if let Some((prefix_len, replacement)) = match_path_prefix(&slice[slash_pos..]) {
                let abs_start = start + slash_pos;
                let abs_end = abs_start + prefix_len;
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        format!("literal path found here — consider using `{replacement}` instead"),
                        Span::from_bytes(abs_start, abs_end),
                    )
                    .with_suggestion(Suggestion::new(
                        "replace the hardcoded path with the matching RPM macro",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
                idx = slash_pos + prefix_len;
            } else {
                idx = slash_pos + 1;
            }
        }
    }
}

fn is_safe_tag(tag: &Tag) -> bool {
    matches!(
        tag,
        // Free-form / URL / source tags — literal paths are expected.
        Tag::Source(_)
            | Tag::Patch(_)
            | Tag::URL
            | Tag::Summary
            | Tag::License
            | Tag::Group
            // Dependency tags — absolute paths are file-based deps,
            // the canonical RPM idiom. Rewriting them to a macro is
            // wrong: file deps resolve through rpm's file-provider
            // table, not the macro-expanded path.
            | Tag::Requires
            | Tag::BuildRequires
            | Tag::Provides
            | Tag::Conflicts
            | Tag::Obsoletes
            | Tag::Recommends
            | Tag::Suggests
            | Tag::Supplements
            | Tag::Enhances
            | Tag::BuildConflicts
            | Tag::OrderWithRequires
    )
}

impl<'ast> Visit<'ast> for HardcodedPaths {
    fn visit_preamble(&mut self, node: &'ast PreambleItem<Span>) {
        if !is_safe_tag(&node.tag) {
            self.scan_anchor(node.data);
        }
        visit::walk_preamble(self, node);
    }

    fn visit_file_entry(&mut self, node: &'ast FileEntry<Span>) {
        self.scan_anchor(node.data);
        visit::walk_file_entry(self, node);
    }

    fn visit_section(&mut self, node: &'ast Section<Span>) {
        if let Section::BuildScript { data, .. }
        | Section::Verify { data, .. }
        | Section::Sepolicy { data, .. } = node
        {
            self.scan_anchor(*data);
        }
        visit::walk_section(self, node);
    }

    fn visit_scriptlet(&mut self, node: &'ast Scriptlet<Span>) {
        self.scan_anchor(node.data);
        visit::walk_scriptlet(self, node);
    }

    fn visit_trigger(&mut self, node: &'ast Trigger<Span>) {
        self.scan_anchor(node.data);
        visit::walk_trigger(self, node);
    }
}

impl Lint for HardcodedPaths {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = HardcodedPaths::new();
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn silent_for_path_in_requires() {
        // File-based deps are RPM's canonical idiom; `Requires:`-like
        // tags are exempt.
        let diags = run("Name: x\nRequires: /usr/bin/python3\n");
        assert!(diags.is_empty(), "got {diags:?}");
    }

    #[test]
    fn silent_for_path_in_build_requires() {
        // BuildRequires uses the same file-based-dependency idiom.
        let diags = run("Name: x\nBuildRequires: /usr/bin/xsltproc\n");
        assert!(diags.is_empty(), "got {diags:?}");
    }

    #[test]
    fn silent_for_useradd_in_requires() {
        // The classic `Requires: /usr/sbin/useradd` case — flagging
        // it is wrong; the right fix (`Requires(pre): shadow-utils`)
        // is the job of a different rule.
        let diags = run("Name: x\nRequires: /usr/sbin/useradd\n");
        assert!(diags.is_empty(), "got {diags:?}");
    }

    #[test]
    fn silent_in_url_tag() {
        // URL is allowed to contain literal paths.
        let src = "Name: x\nURL: https://example.org/usr/bin\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_in_summary() {
        // Summary is free-form prose.
        let src = "Name: x\nSummary: Helper for /usr/bin tools\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_path_in_install_script() {
        let src = "Name: x\n%install\nmkdir -p /usr/lib/foo\n";
        let diags = run(src);
        assert!(!diags.is_empty());
    }

    #[test]
    fn flags_libdir_over_libdir64_first() {
        // `/usr/lib64` should match longer prefix first. Use a shell
        // script context — `Requires:`-like tags are exempt now.
        let src = "Name: x\n%install\ncp libfoo.so /usr/lib64/\n";
        let diags = run(src);
        assert!(!diags.is_empty());
        // Sanity: message mentions the longer-prefix replacement.
        assert!(diags[0].message.contains("%{_libdir}"));
    }

    #[test]
    fn silent_for_macro_only() {
        let src = "Name: x\n%install\ncp foo %{_bindir}/python3\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_short_path_not_in_table() {
        let src = "Name: x\n%install\ncp foo /opt/custom\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_prefix_substring() {
        // `/usr/binfoo` is not `/usr/bin` followed by `foo` — the name
        // character `f` continues the path segment. The boundary check
        // in `match_path_prefix` must reject this.
        let src = "Name: x\n%install\necho /usr/binfoo\n";
        let diags = run(src);
        assert!(
            diags.is_empty(),
            "false positive on prefix substring: {diags:?}"
        );
    }

    #[test]
    fn flags_path_terminated_by_whitespace() {
        // Real shell idiom: `if [ -d /usr/bin ]; then ...`. The byte
        // after `/usr/bin` is a space; the boundary check must accept
        // that as a path terminator and still emit the diagnostic.
        let src = "Name: x\n%install\nif [ -d /usr/bin ]; then :; fi\n";
        let diags = run(src);
        assert_eq!(
            diags.len(),
            1,
            "expected match on `/usr/bin ` (space-terminated): {diags:?}"
        );
    }

    #[test]
    fn per_occurrence_precise_spans_in_section() {
        // Two distinct hardcoded paths on different lines should
        // produce two diagnostics, each with a span pointing at its
        // own line — not at the whole section.
        let src = "Name: x\n%install\ncp a /usr/bin/foo\ncp b /usr/sbin/bar\n";
        let diags = run(src);
        assert_eq!(diags.len(), 2, "expected one diag per path: {diags:?}");
        // Spans must be distinct.
        assert_ne!(diags[0].primary_span, diags[1].primary_span);
        // Each span covers exactly the matched prefix length
        // (`/usr/bin` = 8 bytes, `/usr/sbin` = 9 bytes).
        let lens: Vec<usize> = diags
            .iter()
            .map(|d| d.primary_span.end_byte - d.primary_span.start_byte)
            .collect();
        assert!(lens.contains(&8), "got {lens:?}");
        assert!(lens.contains(&9), "got {lens:?}");
    }

    #[test]
    fn span_points_at_path_not_at_section() {
        // The span should be a few bytes (the path prefix), not the
        // entire section.
        let src = "Name: x\n%install\nmkdir -p /usr/lib/foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        let span = diags[0].primary_span;
        // `/usr/lib` = 8 bytes.
        assert_eq!(span.end_byte - span.start_byte, 8);
        // And the matched slice must actually be `/usr/lib`.
        assert_eq!(&src[span.start_byte..span.end_byte], "/usr/lib");
    }
}
