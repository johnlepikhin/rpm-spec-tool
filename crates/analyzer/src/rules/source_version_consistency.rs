//! RPM304 `source-version-mismatch` — flag `SourceN:` URLs whose
//! hard-coded version differs from the `Version:` tag.
//!
//! Typical bug:
//!
//! ```rpm
//! Version: 1.4
//! Source0: https://example.org/foo/foo-1.3.tar.gz
//! ```
//!
//! After bumping `Version:` someone forgot to update `Source0:`. The
//! build still references `1.3`, downloads the wrong archive (or — with
//! lookaside caches — silently keeps the stale tarball), and the package
//! ships outdated upstream content.
//!
//! Heuristic — flag only when:
//!
//! 1. `Version:` is fully literal (no macros).
//! 2. The Source value does **not** reference `%{version}` / `%version`
//!    (that's the canonical, well-parameterised form).
//! 3. Some literal segment of the Source value contains a
//!    version-shaped token (`\d+(\.\d+)+`).
//! 4. At least one such token differs from `Version:` and no token
//!    matches it.
//!
//! That last clause keeps us silent on legitimate dual-version URLs
//! like `archive/v1.4/old-1.0-renamed.tar.gz` where both versions
//! appear and at least one of them lines up with `Version:`.

use rpm_spec::ast::{Span, SpecFile, Tag, TagValue, Text, TextSegment};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::collect_top_level_preamble;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM304",
    name: "source-version-mismatch",
    description: "A `SourceN:` URL contains a hard-coded version different from `Version:`. \
                  After a version bump this points at the old upstream archive.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct SourceVersionMismatch {
    diagnostics: Vec<Diagnostic>,
}

impl SourceVersionMismatch {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SourceVersionMismatch {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let items = collect_top_level_preamble(spec);
        let Some(version) = literal_version(&items) else {
            return;
        };

        for item in items {
            let Tag::Source(_) = item.tag else { continue };
            let TagValue::Text(t) = &item.value else {
                continue;
            };

            if references_version_macro(t) {
                continue;
            }

            let tokens: Vec<&str> = literal_version_tokens(t);
            if tokens.is_empty() {
                continue;
            }

            // Only flag when *no* token agrees with Version. A URL like
            // `archive/v%{version}/foo-1.0-renamed.tar.gz` happens to
            // satisfy clause 2 (uses `%{version}`) and is already
            // skipped above; the remaining ambiguous shapes — multiple
            // unrelated version tokens — get the benefit of the doubt.
            let any_match = tokens.iter().any(|tok| *tok == version);
            if any_match {
                continue;
            }

            let mismatched: Vec<&str> = tokens.into_iter().filter(|tok| *tok != version).collect();
            if mismatched.is_empty() {
                continue;
            }
            let listed = mismatched.join(", ");
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "Source URL hard-codes version `{listed}`, but `Version:` is `{version}` \
                     — replace the literal with `%{{version}}`",
                ),
                item.data,
            ));
        }
    }
}

fn literal_version(items: &[&rpm_spec::ast::PreambleItem<Span>]) -> Option<String> {
    for item in items {
        if let Tag::Version = item.tag
            && let TagValue::Text(t) = &item.value
            && let Some(s) = t.literal_str()
        {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    None
}

fn references_version_macro(t: &Text) -> bool {
    t.segments
        .iter()
        .any(|seg| matches!(seg, TextSegment::Macro(m) if m.name == "version"))
}

/// Pull every `\d+(\.\d+)+` token out of `t`'s literal segments. Macro
/// segments are conservatively treated as token boundaries.
fn literal_version_tokens(t: &Text) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for seg in &t.segments {
        if let TextSegment::Literal(s) = seg {
            extract_version_tokens(s, &mut out);
        }
    }
    out
}

/// Extract every `\d+(\.\d+)+` token from `s` and append into `out`.
///
/// Hand-rolled byte scanner because the workspace does not depend on
/// `regex` (the rest of the analyzer also avoids it for the same
/// reason — keep the dep graph small, runs once per `Source` value,
/// no catastrophic-backtracking surface). ASCII-only checks make
/// the byte indexing UTF-8-safe: `is_ascii_digit` and `b'.'` are both
/// single-byte codepoints, so `&s[start..i]` always lands on char
/// boundaries.
fn extract_version_tokens<'a>(s: &'a str, out: &mut Vec<&'a str>) {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            // Match `\d+(\.\d+)+`. A bare integer is excluded — too noisy.
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let mut has_dot = false;
            loop {
                if i + 1 < bytes.len() && bytes[i] == b'.' && bytes[i + 1].is_ascii_digit() {
                    has_dot = true;
                    i += 1; // consume dot
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            if has_dot {
                out.push(&s[start..i]);
            }
        } else {
            i += 1;
        }
    }
}

impl Lint for SourceVersionMismatch {
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
        let mut lint = SourceVersionMismatch::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_stale_version_in_url() {
        let src = "Name: foo\n\
Version: 1.4\n\
Source0: https://example.org/foo/foo-1.3.tar.gz\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM304");
        assert!(diags[0].message.contains("1.3"));
        assert!(diags[0].message.contains("1.4"));
    }

    #[test]
    fn silent_when_version_uses_macro() {
        let src = "Name: foo\n\
Version: 1.4\n\
Source0: https://example.org/foo/foo-%{version}.tar.gz\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_version_matches() {
        let src = "Name: foo\n\
Version: 1.4\n\
Source0: https://example.org/foo/foo-1.4.tar.gz\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_version_is_macro() {
        // Version itself is a macro — can't compare, skip the whole
        // check. Avoids guessing.
        let src = "Name: foo\n\
Version: %{upstream_ver}\n\
Source0: https://example.org/foo/foo-1.3.tar.gz\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_url_contains_no_version_token() {
        let src = "Name: foo\n\
Version: 1.4\n\
Source0: https://example.org/foo/foo.tar.gz\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_multiple_stale_sources() {
        let src = "Name: foo\n\
Version: 2.0\n\
Source0: https://example.org/foo-1.0.tar.gz\n\
Source1: https://example.org/foo-1.0.sig\n";
        let diags = run(src);
        assert_eq!(diags.len(), 2);
    }

    #[test]
    fn silent_when_one_token_matches_version() {
        // Dual-version URL where one token matches `Version:` — likely a
        // legitimate path layout. Stay conservative.
        let src = "Name: foo\n\
Version: 1.4\n\
Source0: https://example.org/archive/1.4/foo-1.0-renamed.tar.gz\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn ignores_bare_integers() {
        // `foo-12345.tar.gz` (no dot) is not a version-shaped token.
        let src = "Name: foo\n\
Version: 1.4\n\
Source0: https://example.org/foo-12345.tar.gz\n";
        assert!(run(src).is_empty());
    }
}
