//! RPM571 `stale-disabled-source-or-patch` — flag commented `SourceN:`
//! / `PatchN:` lines.
//!
//! Disabled source / patch entries left as comments tell reviewers
//! nothing about why they're disabled and clutter `diff` output on
//! every refresh. Either delete them or move them to the changelog.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM571",
    name: "stale-disabled-source-or-patch",
    description: "Commented `SourceN:` / `PatchN:` line — delete it or record the reason in the \
                  changelog.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Commented `SourceN:` / `PatchN:` line — delete it or record the reason in the changelog.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct StaleDisabledSourceOrPatch {
    diagnostics: Vec<Diagnostic>,
    source: std::sync::Arc<str>,
}

impl StaleDisabledSourceOrPatch {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for StaleDisabledSourceOrPatch {
    fn visit_spec(&mut self, _spec: &'ast SpecFile<Span>) {
        if self.source.is_empty() {
            return;
        }
        let preamble_end = find_preamble_end(&self.source);
        let mut cursor = 0usize;
        for line in self.source.lines() {
            let line_start = cursor;
            cursor += line.len() + 1;
            // Disabled-patch heuristic only makes sense in the preamble.
            // Commented `PatchN:` / `SourceN:` lines inside `%prep`,
            // `%description`, etc. are almost always explanatory notes,
            // not stale declarations.
            if line_start >= preamble_end {
                continue;
            }
            if is_commented_source_or_patch(line) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "commented `Source`/`Patch` declaration — drop the line or document the \
                         reason in the changelog",
                        Span::from_bytes(line_start, line_start + line.len()),
                    )
                    .with_suggestion(Suggestion::new(
                        "remove the disabled `SourceN:` / `PatchN:` line",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

/// Return the byte offset of the first `%`-section header (e.g.
/// `%prep`, `%description`). Anything at or after this point is
/// outside the preamble.
fn find_preamble_end(source: &str) -> usize {
    const SECTIONS: &[&str] = &[
        "%description",
        "%package",
        "%prep",
        "%build",
        "%install",
        "%check",
        "%files",
        "%changelog",
        "%pre",
        "%post",
        "%preun",
        "%postun",
        "%trigger",
        "%clean",
    ];
    let mut earliest = source.len();
    for sec in SECTIONS {
        let pat = format!("\n{sec}");
        if let Some(i) = source.find(&pat) {
            // +1 to land on the start of the section line itself.
            earliest = earliest.min(i + 1);
        }
        if source.starts_with(sec) {
            earliest = 0;
        }
    }
    earliest
}

fn is_commented_source_or_patch(line: &str) -> bool {
    let stripped = line.trim_start();
    let Some(rest) = stripped.strip_prefix('#') else {
        return false;
    };
    let body = rest.trim_start();
    let Some((head, _)) = body.split_once(':') else {
        return false;
    };
    // Head must be `Source` / `Patch` optionally followed by digits.
    let normalised = head.trim();
    if let Some(num) = normalised.strip_prefix("Source") {
        return num.is_empty() || num.chars().all(|c| c.is_ascii_digit());
    }
    if let Some(num) = normalised.strip_prefix("Patch") {
        return num.is_empty() || num.chars().all(|c| c.is_ascii_digit());
    }
    false
}

impl Lint for StaleDisabledSourceOrPatch {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: std::sync::Arc<str>) {
        self.source = source;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<StaleDisabledSourceOrPatch>(src)
    }

    #[test]
    fn flags_commented_patch() {
        let src = "Name: x\n#Patch12: old.patch\nVersion: 1\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM571");
    }

    #[test]
    fn flags_commented_source() {
        let src = "Name: x\n#Source3: old-tarball.tar.gz\nVersion: 1\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_normal_comment() {
        let src = "Name: x\n# Just a normal comment.\nVersion: 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_commented_patch_after_prep_section() {
        // A commented `PatchN:` inside `%prep` is almost always a note,
        // not a stale declaration. Only the preamble is in scope.
        let src = "Name: x\nPatch0: a.patch\n%prep\n# Patch12: legacy.patch\n%setup -q\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_uncommented_patch() {
        let src = "Name: x\nPatch12: foo.patch\nVersion: 1\n";
        assert!(run(src).is_empty());
    }
}
