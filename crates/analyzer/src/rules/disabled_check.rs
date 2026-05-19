//! RPM389 `disabled-check-section` — the `%check` section exists but
//! contains no executable shell statements.
//!
//! Whitespace-only or comment-only `%check` bodies are the way
//! maintainers silently disable an upstream test suite — usually as a
//! quick workaround for a flaky test, then forgotten. A disabled
//! `%check` masks regressions that would otherwise fail the build on
//! Mock/Koji/OBS and is one of the most common quality slips on long-
//! lived specs.
//!
//! The rule is conservative: it only fires when the section is
//! present (missing `%check` is a separate concern handled by other
//! rules) and the body has zero non-comment non-blank lines. A
//! single literal command — even `true` or
//! `:` — is enough to silence it; the maintainer who deliberately
//! wants a no-op `%check` can spell that out, which makes the
//! intention visible to reviewers.

use rpm_spec::ast::{BuildScriptKind, ShellBody, Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::shell::for_each_buildscript;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM389",
    name: "disabled-check-section",
    description: "`%check` is present but contains no executable statements — only blank lines \
                  and comments. Silently disabling the test suite masks regressions; either \
                  remove `%check` entirely or restore the test invocation.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Lint state for RPM389 `disabled-check-section`. Holds diagnostics
/// emitted while walking each `%check` body.
#[derive(Debug, Default)]
pub struct DisabledCheckSection {
    diagnostics: Vec<Diagnostic>,
}

impl DisabledCheckSection {
    /// Construct an empty lint instance with no buffered diagnostics.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for DisabledCheckSection {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for_each_buildscript(spec, |kind, body, span| {
            if kind != BuildScriptKind::Check {
                return;
            }
            if has_executable_line(body) {
                return;
            }
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                "`%check` is present but contains no executable statements; silently disabling \
                 tests hides regressions — drop the section or restore the test invocation",
                span,
            ));
        });
    }
}

/// `true` if any line in `body` carries shell content. Blank lines
/// and lines whose literal payload is empty (or starts with `#` after
/// trimming) are skipped; everything else counts.
fn has_executable_line(body: &ShellBody<Span>) -> bool {
    body.lines.iter().any(|line| {
        let Some(lit) = line.literal_str() else {
            // Macro-containing lines: assume they're executable. A
            // bare `%{nil}` is rare and a maintainer who wants to
            // disable tests with a macro can `--allow` the rule.
            return true;
        };
        let trimmed = lit.trim();
        !trimmed.is_empty() && !trimmed.starts_with('#')
    })
}

impl Lint for DisabledCheckSection {
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
        run_lint::<DisabledCheckSection>(src)
    }

    #[test]
    fn flags_empty_check_section() {
        let src = "Name: x\n%check\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM389");
    }

    #[test]
    fn flags_check_with_only_comments() {
        let src = "Name: x\n%check\n# tests broken on aarch64 — re-enable after upgrade\n# see issue #123\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_check_with_only_blanks_and_comments() {
        let src = "Name: x\n%check\n\n# disabled\n\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_when_check_has_make_test() {
        let src = "Name: x\n%check\nmake test\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_check_has_only_true() {
        // `true` is a real no-op statement — maintainer made the
        // intent explicit. Don't flag.
        let src = "Name: x\n%check\ntrue\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_check_section_missing() {
        // Missing `%check` is a separate concern (different rule).
        let src = "Name: x\n%build\nmake\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_other_empty_sections() {
        // Only `%check` is flagged; an empty `%clean` is not RPM389's
        // problem (and `%clean` itself is deprecated, RPM015).
        let src = "Name: x\n%clean\n# nothing to clean\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_check_has_macro_line() {
        // `%pytest` or any macro line is treated as executable.
        let src = "Name: x\n%check\n%pytest\n";
        assert!(run(src).is_empty());
    }
}
