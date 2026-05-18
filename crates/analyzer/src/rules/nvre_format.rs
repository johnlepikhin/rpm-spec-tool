//! RPM302 `invalid-name-version-release-epoch-format` — literal-only
//! validation of `Name`, `Version`, `Release`, `Epoch` against RPM's
//! lexical constraints.
//!
//! Catches:
//!
//! - `Name:` containing whitespace, `/`, or any other character RPM
//!   rejects in package names.
//! - `Version:` containing whitespace or `-` (the EVR separator).
//! - `Release:` containing whitespace or `-`.
//! - `Epoch:` literal `0` — meaningless, since the default already is
//!   zero. RPM keeps it but Fedora/openSUSE guidelines flag it as a
//!   code smell.
//!
//! Conservative on macros: a tag value that contains any `%foo` /
//! `%{foo}` reference is skipped — we can't tell what it expands to.
//! `Release: 1%{?dist}` is the standard idiom and stays silent.

use rpm_spec::ast::{Span, SpecFile, Tag, TagValue, Text};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::collect_top_level_preamble;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM302",
    name: "invalid-name-version-release-epoch-format",
    description: "Name/Version/Release contains characters RPM does not accept, or Epoch is \
                  literally `0` (the default — drop the tag instead).",
    default_severity: Severity::Deny,
    category: LintCategory::Correctness,
};

/// Name/Version/Release contains characters RPM does not accept, or Epoch is literally `0` (the default — drop the tag instead).
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct InvalidNvreFormat {
    diagnostics: Vec<Diagnostic>,
}

impl InvalidNvreFormat {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for InvalidNvreFormat {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for item in collect_top_level_preamble(spec) {
            match (&item.tag, &item.value) {
                (Tag::Name, TagValue::Text(t)) => {
                    if let Some(reason) = check_name(t) {
                        self.diagnostics
                            .push(diag(reason, item.data, Severity::Deny));
                    }
                }
                (Tag::Version, TagValue::Text(t)) => {
                    if let Some(reason) = check_version_or_release(t, "Version") {
                        self.diagnostics
                            .push(diag(reason, item.data, Severity::Deny));
                    }
                }
                (Tag::Release, TagValue::Text(t)) => {
                    if let Some(reason) = check_version_or_release(t, "Release") {
                        self.diagnostics
                            .push(diag(reason, item.data, Severity::Deny));
                    }
                }
                (Tag::Epoch, TagValue::Number(0)) => {
                    self.diagnostics.push(diag(
                        "`Epoch: 0` is the default — drop the tag instead of setting it explicitly"
                            .to_owned(),
                        item.data,
                        Severity::Warn,
                    ));
                }
                _ => {}
            }
        }
    }
}

fn check_name(t: &Text) -> Option<String> {
    let s = literal_or_skip(t)?;
    if s.is_empty() {
        return Some("`Name:` is empty".to_owned());
    }
    for c in s.chars() {
        // RPM accepts `[A-Za-z0-9._+-]` in names (with a few extras).
        // The set below is intentionally permissive (also allows `~`)
        // — the rule's job is to catch *obviously* invalid characters,
        // not police every distro convention.
        if !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | '~')) {
            return Some(format!(
                "`Name:` contains invalid character {c:?} (allowed: A-Z a-z 0-9 . _ + - ~)"
            ));
        }
    }
    None
}

fn check_version_or_release(t: &Text, tag_label: &str) -> Option<String> {
    let s = literal_or_skip(t)?;
    if s.is_empty() {
        return Some(format!("`{tag_label}:` is empty"));
    }
    if s.chars().any(char::is_whitespace) {
        return Some(format!("`{tag_label}:` contains whitespace"));
    }
    if s.contains('-') {
        // `-` is the EVR separator inside RPM dependency expressions.
        // Allowing it in Version/Release breaks `Requires: foo = V-R`.
        return Some(format!(
            "`{tag_label}:` contains `-`, which is the EVR separator and cannot appear here"
        ));
    }
    None
}

/// Return the trimmed literal text if `t` is fully literal; `None` when
/// any macro reference is present (we can't safely evaluate macros).
fn literal_or_skip(t: &Text) -> Option<String> {
    t.literal_str().map(|s| s.trim().to_owned())
}

fn diag(message: String, span: Span, severity: Severity) -> Diagnostic {
    Diagnostic::new(&METADATA, severity, message, span)
}

impl Lint for InvalidNvreFormat {
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
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<InvalidNvreFormat>(src)
    }

    #[test]
    fn flags_name_with_whitespace() {
        let diags = run("Name: hello world\nVersion: 1\n");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM302");
        assert!(diags[0].message.contains("Name"));
    }

    #[test]
    fn flags_name_with_slash() {
        let diags = run("Name: foo/bar\nVersion: 1\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("'/'") || diags[0].message.contains("\"/\""));
    }

    #[test]
    fn flags_version_with_hyphen() {
        let diags = run("Name: x\nVersion: 1-2\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Version"));
        assert!(diags[0].message.contains("`-`"));
    }

    #[test]
    fn flags_release_with_whitespace() {
        let diags = run("Name: x\nVersion: 1\nRelease: 1 alpha\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Release"));
    }

    #[test]
    fn flags_epoch_zero() {
        let diags = run("Name: x\nEpoch: 0\nVersion: 1\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Epoch"));
        assert_eq!(diags[0].severity, Severity::Warn);
    }

    #[test]
    fn silent_for_well_formed_nvre() {
        let src = "Name: hello\nEpoch: 2\nVersion: 1.2.3\nRelease: 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_release_with_dist_macro() {
        // The canonical Release form — `%{?dist}` is a macro, so the
        // whole value is skipped by `literal_or_skip`. No false flag.
        let src = "Name: hello\nVersion: 1\nRelease: 1%{?dist}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_version_with_macro() {
        // `Version: %{upstream_ver}` — skip rather than guess.
        let src = "Name: hello\nVersion: %{upstream_ver}\nRelease: 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn check_name_reports_empty() {
        // Pin the contract directly against `check_name`: an empty
        // `Text` (no segments) must produce the "is empty" reason.
        // Bypassing the parser keeps the test from depending on
        // whether `Name: ` is accepted as `Text::new()` or rejected
        // upstream.
        let empty = rpm_spec::ast::Text::new();
        let reason = check_name(&empty).expect("empty Name must be flagged");
        assert!(reason.contains("empty"), "reason: {reason}");
    }
}
