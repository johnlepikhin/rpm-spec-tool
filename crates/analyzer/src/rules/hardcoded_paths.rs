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
use rpm_spec_profile::Profile;

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{FALLBACK_PATH_TABLE, is_path_boundary};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM050",
    name: "hardcoded-paths",
    description: "Use the matching RPM macro instead of a hardcoded path (e.g. `%{_bindir}` for `/usr/bin`).",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug)]
pub struct HardcodedPaths {
    diagnostics: Vec<Diagnostic>,
    /// Raw source bytes, set via [`Lint::set_source`] before each pass.
    /// Required because the rule scans the source slice covered by an
    /// anchor span to compute precise per-occurrence sub-spans.
    source: Option<String>,
    /// `(literal_prefix, "%{macro}")` pairs scanned top-down. Defaults
    /// to [`FALLBACK_PATH_TABLE`]; replaced from `profile.macros` in
    /// [`Lint::set_profile`] so distro-specific paths (e.g. `_libdir`
    /// on e2k / aarch64 / non-Fedora layouts) suggest the correct
    /// macro instead of the x86_64 / RHEL default.
    path_table: Vec<(String, String)>,
}

impl Default for HardcodedPaths {
    fn default() -> Self {
        Self {
            diagnostics: Vec::new(),
            source: None,
            path_table: FALLBACK_PATH_TABLE
                .iter()
                .map(|(p, m)| ((*p).to_string(), (*m).to_string()))
                .collect(),
        }
    }
}

impl HardcodedPaths {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to find the longest matching prefix from `path_table` against
    /// the start of `text`, with the same path-boundary rule as the
    /// previous `util::match_path_prefix` helper.
    fn match_prefix<'a>(&'a self, text: &str) -> Option<(usize, &'a str)> {
        for (prefix, replacement) in &self.path_table {
            if let Some(rest) = text.strip_prefix(prefix.as_str())
                && is_path_boundary(rest)
            {
                return Some((prefix.len(), replacement.as_str()));
            }
        }
        None
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
            if let Some((prefix_len, replacement)) = self.match_prefix(&slice[slash_pos..]) {
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
    fn set_profile(&mut self, profile: &Profile) {
        // Profile-derived entries: for each well-known path macro, try
        // to expand to a literal absolute path; record the mapping
        // `<literal> → %{<macro>}`. Skip macros the profile didn't
        // define or couldn't expand cleanly.
        let mut table: Vec<(String, String)> = Vec::new();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for path_macro in PATH_MACROS {
            if let Some(literal) = profile.macros.expand_to_literal(path_macro, 8)
                && literal.starts_with('/')
                && seen.insert(literal.clone())
            {
                table.push((literal, format!("%{{{path_macro}}}")));
            }
        }
        // Append every fallback entry whose path the profile didn't
        // already cover. Preserves coverage of legacy/multi-arch
        // aliases (e.g. `/usr/lib → %{_libdir}` even when the profile
        // says `_libdir = /usr/lib64`).
        for (path, macro_expr) in FALLBACK_PATH_TABLE {
            if seen.insert((*path).to_string()) {
                table.push(((*path).to_string(), (*macro_expr).to_string()));
            }
        }
        // Longest prefix first, so `/usr/lib64` is checked before
        // `/usr/lib` and `/var/log` before `/var/lib`.
        table.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        self.path_table = table;
    }
}

/// Path-macro names that `set_profile` probes for in `profile.macros`.
/// Order doesn't matter — the merged table is sorted by prefix length
/// descending in `set_profile`.
const PATH_MACROS: &[&str] = &[
    "_bindir",
    "_sbindir",
    "_libdir",
    "_libexecdir",
    "_includedir",
    "_datadir",
    "_mandir",
    "_infodir",
    "_docdir",
    "_sysconfdir",
    "_localstatedir",
    "_sharedstatedir",
];

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

    /// `set_profile` pulls actual path values from `profile.macros` —
    /// so a profile defining `_libdir = /opt/myproj/lib` makes the rule
    /// suggest `%{_libdir}` for that path, not `/usr/lib64`.
    #[test]
    fn profile_redefines_libdir_path() {
        use rpm_spec_profile::{MacroEntry, Profile, Provenance};

        let src = "Name: x\n%install\ncp libfoo.so /opt/myproj/lib/\n";
        let outcome = parse(src);
        let mut lint = HardcodedPaths::new();
        lint.set_source(src);

        let mut profile = Profile::default();
        profile.macros.insert(
            "_libdir",
            MacroEntry::literal("/opt/myproj/lib", Provenance::Override),
        );
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        let diags = lint.take_diagnostics();

        assert_eq!(diags.len(), 1, "expected one diag; got {diags:?}");
        assert!(
            diags[0].message.contains("%{_libdir}"),
            "should suggest %{{_libdir}}; got {}",
            diags[0].message
        );
    }

    /// Profile expansion follows `%{...}` references — so RHEL's
    /// `_libdir = %{_prefix}/lib64` + `_prefix = /usr` resolves to
    /// `/usr/lib64` and the rule continues to flag it.
    #[test]
    fn profile_expands_macro_chain() {
        use rpm_spec_profile::{MacroEntry, MacroValue, Profile, Provenance};

        let src = "Name: x\n%install\ncp libfoo.so /usr/lib64/\n";
        let outcome = parse(src);
        let mut lint = HardcodedPaths::new();
        lint.set_source(src);

        let mut profile = Profile::default();
        profile
            .macros
            .insert("_prefix", MacroEntry::literal("/usr", Provenance::Override));
        // `MacroEntry` is `#[non_exhaustive]` — build via `literal()`
        // and mutate the `pub` fields to swap in a `Raw` value.
        let mut libdir = MacroEntry::literal("", Provenance::Override);
        libdir.value = MacroValue::Raw {
            body: "%{_prefix}/lib64".into(),
            multiline: false,
        };
        profile.macros.insert("_libdir", libdir);
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        let diags = lint.take_diagnostics();

        assert_eq!(diags.len(), 1, "got {diags:?}");
        assert!(diags[0].message.contains("%{_libdir}"));
    }

    /// When a profile's macro doesn't expand cleanly (e.g. a lua-bodied
    /// macro), the rule falls back to the hardcoded default so flagging
    /// still happens on standard FHS paths.
    #[test]
    fn profile_falls_back_when_macro_is_unresolvable() {
        use rpm_spec_profile::{MacroEntry, MacroValue, Profile, Provenance};

        let src = "Name: x\n%install\ncp foo /usr/bin/bar\n";
        let outcome = parse(src);
        let mut lint = HardcodedPaths::new();
        lint.set_source(src);

        let mut profile = Profile::default();
        let mut bindir = MacroEntry::literal("", Provenance::Override);
        // `_bindir` is a lua expression — unresolvable at lint time.
        // The fallback `/usr/bin` should still trigger the rule.
        bindir.value = MacroValue::Raw {
            body: "%{lua:print('/usr/bin')}".into(),
            multiline: false,
        };
        profile.macros.insert("_bindir", bindir);
        lint.set_profile(&profile);
        lint.visit_spec(&outcome.spec);
        let diags = lint.take_diagnostics();

        assert_eq!(
            diags.len(),
            1,
            "fallback to hardcoded /usr/bin; got {diags:?}"
        );
        assert!(diags[0].message.contains("%{_bindir}"));
    }
}
