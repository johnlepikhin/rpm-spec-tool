//! Phase 5 modernization lints that flag deprecated shell-level
//! commands in `%build` / `%install` / `%check` / `%prep` /
//! `%verify` / `%sepolicy` bodies and in scriptlets/triggers.
//!
//! All three rules share a single implementation, [`WordScanLint`],
//! parameterised by:
//! - the metadata (id / name / severity),
//! - the list of needles to search for, each with an optional
//!   replacement that becomes a `MachineApplicable` edit when set.
//!
//! Adding a new rule of the same shape is one line in `registry.rs`
//! plus a metadata static here — no extra `impl` blocks.
//!
//! ## Rules in this file
//! - RPM060 `python-setup-test-deprecated` — `setup.py test`
//! - RPM061 `python-setup-install-deprecated` — `setup.py install`
//! - RPM062 `egrep-fgrep-deprecated` — `egrep` / `fgrep` (auto-fix)

use rpm_spec::ast::{FileTrigger, Scriptlet, Section, Span, Trigger};

use crate::diagnostic::{Applicability, Diagnostic, Edit, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::is_name_byte;
use crate::visit::{self, Visit};

// =====================================================================
// Per-rule metadata
// =====================================================================

pub static SETUP_TEST_METADATA: LintMetadata = LintMetadata {
    id: "RPM060",
    name: "python-setup-test-deprecated",
    description:
        "Replace `python setup.py test` with a modern test runner (pytest / tox / nox).",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

pub static SETUP_INSTALL_METADATA: LintMetadata = LintMetadata {
    id: "RPM061",
    name: "python-setup-install-deprecated",
    description:
        "Replace `python setup.py install` with `pip install` / `%py3_install` / PEP 517 builder.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

pub static EGREP_FGREP_METADATA: LintMetadata = LintMetadata {
    id: "RPM062",
    name: "egrep-fgrep-deprecated",
    description: "Use `grep -E` / `grep -F` instead of the deprecated `egrep` / `fgrep`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

// =====================================================================
// Needle tables
// =====================================================================

/// One word-scan target. `replacement: Some(...)` upgrades the
/// suggestion to `MachineApplicable` with an `Edit` that swaps the
/// matched bytes; `None` keeps it as a `Manual` advisory.
#[derive(Debug)]
pub struct Needle {
    pub text: &'static str,
    pub replacement: Option<&'static str>,
}

pub static SETUP_TEST_NEEDLES: &[Needle] =
    &[Needle { text: "setup.py test", replacement: None }];

pub static SETUP_INSTALL_NEEDLES: &[Needle] =
    &[Needle { text: "setup.py install", replacement: None }];

pub static EGREP_FGREP_NEEDLES: &[Needle] = &[
    Needle { text: "egrep", replacement: Some("grep -E") },
    Needle { text: "fgrep", replacement: Some("grep -F") },
];

// =====================================================================
// Unified rule implementation
// =====================================================================

/// Generic shell-body word-scan lint. Constructed with a metadata
/// pointer and a slice of needles; registered once per distinct rule
/// in [`crate::registry`].
#[derive(Debug)]
pub struct WordScanLint {
    meta: &'static LintMetadata,
    needles: &'static [Needle],
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl WordScanLint {
    pub fn new(meta: &'static LintMetadata, needles: &'static [Needle]) -> Self {
        Self { meta, needles, diagnostics: Vec::new(), source: None }
    }

    fn scan(&mut self, anchor: Span) {
        let Some(source) = self.source.as_deref() else { return };
        for needle in self.needles {
            scan_one(source, anchor, needle, self.meta, &mut self.diagnostics);
        }
    }
}

impl<'ast> Visit<'ast> for WordScanLint {
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        if let Some(anchor) = shell_body_anchor(node) {
            self.scan(anchor);
        }
        visit::walk_section(self, node);
    }
    fn visit_scriptlet(&mut self, node: &'ast Scriptlet<Span>) {
        self.scan(node.data);
        visit::walk_scriptlet(self, node);
    }
    fn visit_trigger(&mut self, node: &'ast Trigger<Span>) {
        self.scan(node.data);
        visit::walk_trigger(self, node);
    }
    fn visit_file_trigger(&mut self, node: &'ast FileTrigger<Span>) {
        self.scan(node.data);
        visit::walk_file_trigger(self, node);
    }
}

impl Lint for WordScanLint {
    fn metadata(&self) -> &'static LintMetadata {
        self.meta
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }
}

// =====================================================================
// Scan helpers
// =====================================================================

/// Anchor span for shell-bearing section variants. Returns `None` for
/// preamble-only / changelog-only sections that don't carry a body.
fn shell_body_anchor(node: &Section<Span>) -> Option<Span> {
    match node {
        Section::BuildScript { data, .. }
        | Section::Verify { data, .. }
        | Section::Sepolicy { data, .. } => Some(*data),
        _ => None,
    }
}

/// Scan the source slice covered by `anchor` for `needle.text` and
/// push one diagnostic per word-boundary-bounded occurrence.
fn scan_one(
    source: &str,
    anchor: Span,
    needle: &Needle,
    meta: &'static LintMetadata,
    out: &mut Vec<Diagnostic>,
) {
    let end = anchor.end_byte.min(source.len());
    let start = anchor.start_byte.min(end);
    let Some(slice) = source.get(start..end) else { return };

    let needle_len = needle.text.len();
    let mut idx = 0;
    while let Some(rel) = slice[idx..].find(needle.text) {
        let pos = idx + rel;
        if is_word_match(slice, pos, needle_len) {
            let abs_start = start + pos;
            let abs_end = abs_start + needle_len;
            let span = Span::from_bytes(abs_start, abs_end);
            let mut diag = Diagnostic::new(
                meta,
                Severity::Warn,
                format!("`{}` is deprecated", needle.text),
                span,
            );
            diag = if let Some(rep) = needle.replacement {
                diag.with_suggestion(Suggestion::new(
                    format!("replace `{}` with `{rep}`", needle.text),
                    vec![Edit::new(span, rep)],
                    Applicability::MachineApplicable,
                ))
            } else {
                diag.with_suggestion(Suggestion::new(
                    format!("rewrite away from `{}`", needle.text),
                    Vec::new(),
                    Applicability::Manual,
                ))
            };
            out.push(diag);
        }
        idx = pos + needle_len;
    }
}

/// `true` when the byte range `[pos, pos + len)` inside `slice` is
/// bounded by non-name bytes on both sides (or by the slice edges).
fn is_word_match(slice: &str, pos: usize, len: usize) -> bool {
    let bytes = slice.as_bytes();
    let prev_ok = match pos.checked_sub(1) {
        None => true,
        Some(p) => !is_name_byte(bytes[p]),
    };
    let end = pos + len;
    let next_ok = end >= bytes.len() || !is_name_byte(bytes[end]);
    prev_ok && next_ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run_lint(src: &str, meta: &'static LintMetadata, needles: &'static [Needle]) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = WordScanLint::new(meta, needles);
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ---- RPM060 -----------------------------------------------------

    #[test]
    fn flags_setup_py_test_in_check() {
        let src = "Name: x\n%check\npython setup.py test\n";
        let diags = run_lint(src, &SETUP_TEST_METADATA, SETUP_TEST_NEEDLES);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM060");
    }

    #[test]
    fn rpm060_silent_for_setup_py_build() {
        // `setup.py build` is a different subcommand — RPM060 only
        // fires on the `test` form.
        let src = "Name: x\n%build\npython setup.py build\n";
        assert!(run_lint(src, &SETUP_TEST_METADATA, SETUP_TEST_NEEDLES).is_empty());
    }

    // ---- RPM061 -----------------------------------------------------

    #[test]
    fn flags_setup_py_install_in_install() {
        let src = "Name: x\n%install\npython setup.py install\n";
        let diags = run_lint(src, &SETUP_INSTALL_METADATA, SETUP_INSTALL_NEEDLES);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM061");
    }

    // ---- RPM062 -----------------------------------------------------

    #[test]
    fn flags_egrep() {
        let src = "Name: x\n%build\nls | egrep foo\n";
        let diags = run_lint(src, &EGREP_FGREP_METADATA, EGREP_FGREP_NEEDLES);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM062");
        let span = diags[0].primary_span;
        assert_eq!(span.end_byte - span.start_byte, 5);
        assert_eq!(&src[span.start_byte..span.end_byte], "egrep");
    }

    #[test]
    fn flags_fgrep() {
        let src = "Name: x\n%build\nls | fgrep -v bar\n";
        let diags = run_lint(src, &EGREP_FGREP_METADATA, EGREP_FGREP_NEEDLES);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM062");
    }

    #[test]
    fn egrep_autofix_replaces_with_grep_e() {
        let src = "Name: x\n%build\nls | egrep foo\n";
        let diags = run_lint(src, &EGREP_FGREP_METADATA, EGREP_FGREP_NEEDLES);
        assert_eq!(diags.len(), 1);
        let sugg = &diags[0].suggestions[0];
        assert_eq!(sugg.edits.len(), 1);
        assert_eq!(sugg.edits[0].replacement, "grep -E");
    }

    #[test]
    fn silent_for_egrep_substring() {
        // `xegrep` and `egrepping` contain `egrep` as a substring but
        // are not the command being invoked. Word boundary rejects.
        // (Using `xegrep` rather than `regrep` because `regrep` is a
        // real recursive-grep wrapper on some distributions.)
        let src = "Name: x\n%build\nls | xegrep foo\necho egrepping\n";
        let diags = run_lint(src, &EGREP_FGREP_METADATA, EGREP_FGREP_NEEDLES);
        assert!(diags.is_empty(), "false positive on substring: {diags:?}");
    }

    #[test]
    fn silent_for_plain_grep() {
        let src = "Name: x\n%build\nls | grep foo\n";
        assert!(run_lint(src, &EGREP_FGREP_METADATA, EGREP_FGREP_NEEDLES).is_empty());
    }

    #[test]
    fn rpm062_fires_in_scriptlet() {
        // Make sure the visit_scriptlet path is wired correctly.
        let src = "Name: x\n%post\nls | egrep x\n";
        let diags = run_lint(src, &EGREP_FGREP_METADATA, EGREP_FGREP_NEEDLES);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
