//! RPM535 `duplicate-shell-block` — flag 3+ contiguous shell lines
//! that appear identically in two or more shell-body sections.
//!
//! Often a `%post` and `%postun` scriptlet share a setup snippet
//! (`getent group …`, `update-alternatives --install …`); extracting
//! the snippet into a helper macro keeps both sides in sync.

use std::collections::{HashMap, HashSet};

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::shell_walk::{for_each_shell_body, render_shell_line};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM535",
    name: "duplicate-shell-block",
    description: "Identical 3+ line shell snippet appears in two or more shell-body sections — \
                  extract into a helper macro or function.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Minimum window size for a duplicate block to be worth extracting.
const WINDOW: usize = 3;

/// Identical 3+ line shell snippet appears in two or more shell-body sections — extract into a helper macro or function.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct DuplicateShellBlock {
    diagnostics: Vec<Diagnostic>,
}

impl DuplicateShellBlock {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for DuplicateShellBlock {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Per-section: ordered list of meaningful lines.
        let mut sections: Vec<(Span, Vec<String>)> = Vec::new();
        for_each_shell_body(spec, |body, anchor| {
            let lines: Vec<String> = body
                .lines
                .iter()
                .map(|l| render_shell_line(l).trim().to_owned())
                .filter(|s| !s.is_empty() && !s.starts_with('#'))
                .collect();
            sections.push((anchor, lines));
        });

        // For each section, compute every 3-line window's hash key.
        // Bucket by key; record (section_index, window_index, anchor).
        let mut buckets: HashMap<String, Vec<(usize, usize, Span)>> = HashMap::new();
        for (sec_idx, (anchor, lines)) in sections.iter().enumerate() {
            if lines.len() < WINDOW {
                continue;
            }
            for w_idx in 0..=lines.len() - WINDOW {
                let key = lines[w_idx..w_idx + WINDOW].join("\n");
                buckets
                    .entry(key)
                    .or_default()
                    .push((sec_idx, w_idx, *anchor));
            }
        }

        let mut reported: HashSet<usize> = HashSet::new();
        for (_key, hits) in buckets {
            // Need members from at least two DISTINCT sections.
            let distinct_sections: std::collections::BTreeSet<usize> =
                hits.iter().map(|(s, _, _)| *s).collect();
            if distinct_sections.len() < 2 {
                continue;
            }
            for (sec_idx, _w_idx, anchor) in hits {
                if !reported.insert(sec_idx) {
                    continue;
                }
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "this shell section shares a 3+ line snippet with another section — \
                         extract the shared block into a helper macro or function",
                        anchor,
                    )
                    .with_suggestion(Suggestion::new(
                        "factor the duplicate snippet into one `%global` macro and reference it \
                         from each section",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl Lint for DuplicateShellBlock {
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
        run_lint::<DuplicateShellBlock>(src)
    }

    #[test]
    fn flags_duplicate_block_across_scriptlets() {
        let src = "Name: x\n\
%post\n\
getent group foo >/dev/null\n\
groupadd -r foo\n\
useradd -r -g foo foo\n\
%postun\n\
getent group foo >/dev/null\n\
groupadd -r foo\n\
useradd -r -g foo foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 2, "{diags:?}");
        assert!(diags.iter().all(|d| d.lint_id == "RPM535"));
    }

    #[test]
    fn silent_for_unique_sections() {
        let src = "Name: x\n\
%post\necho post\n%postun\necho postun\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_short_common_run() {
        // Only 2 common lines — below the WINDOW threshold.
        let src = "Name: x\n\
%post\nline1\nline2\n%postun\nline1\nline2\n";
        assert!(run(src).is_empty());
    }
}
