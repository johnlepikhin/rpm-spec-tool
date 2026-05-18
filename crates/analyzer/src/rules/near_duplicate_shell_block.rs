//! RPM536 `near-duplicate-shell-block` — flag 3+ consecutive shell
//! lines that share the same word-shape but differ in exactly one
//! token (e.g. three `install -D -m644 SERVICE_N.service …` calls).
//!
//! Such patterns usually fold into a `for`-loop over the varying token.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::shell_walk::{for_each_shell_body, render_shell_line};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM536",
    name: "near-duplicate-shell-block",
    description: "Three or more consecutive shell lines share the same word-shape and differ in \
                  one position — fold into a `for` loop over the varying token.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Minimum number of adjacent near-duplicate lines to fire.
const RUN: usize = 3;

/// Three or more consecutive shell lines share the same word-shape and differ in one position — fold into a `for` loop over the varying token.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct NearDuplicateShellBlock {
    diagnostics: Vec<Diagnostic>,
}

impl NearDuplicateShellBlock {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for NearDuplicateShellBlock {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut hits: Vec<Span> = Vec::new();
        for_each_shell_body(spec, |body, anchor| {
            let lines: Vec<Vec<String>> = body
                .lines
                .iter()
                .map(|l| {
                    render_shell_line(l)
                        .split_ascii_whitespace()
                        .map(|s| s.to_owned())
                        .collect()
                })
                .filter(|v: &Vec<String>| !v.is_empty() && !v[0].starts_with('#'))
                .collect();
            if lines.len() < RUN {
                return;
            }
            let mut i = 0;
            while i + RUN <= lines.len() {
                if all_differ_in_one_position(&lines[i..i + RUN]) {
                    hits.push(anchor);
                    i += RUN;
                } else {
                    i += 1;
                }
            }
        });
        for anchor in hits {
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    "consecutive lines share the same word-shape and differ in one position — \
                     consider folding into a `for` loop",
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "wrap the repeated invocation in `for x in …; do … $x …; done`",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

/// `true` when every line has the same token count AND there exists
/// exactly one column index where all lines have a value different
/// from the first line (or pairwise different across lines).
fn all_differ_in_one_position(lines: &[Vec<String>]) -> bool {
    if lines.len() < 2 {
        return false;
    }
    let n = lines[0].len();
    if !lines.iter().all(|l| l.len() == n) {
        return false;
    }
    if n < 2 {
        return false;
    }
    let mut diff_cols: Vec<usize> = Vec::new();
    for col in 0..n {
        let first = &lines[0][col];
        if !lines.iter().all(|l| &l[col] == first) {
            diff_cols.push(col);
        }
    }
    if diff_cols.len() != 1 {
        return false;
    }
    // Require the varying tokens to be DISTINCT across lines —
    // otherwise it's an idempotent no-op pattern, not a loop.
    let col = diff_cols[0];
    let distinct: std::collections::BTreeSet<&str> =
        lines.iter().map(|l| l[col].as_str()).collect();
    distinct.len() == lines.len()
}

impl Lint for NearDuplicateShellBlock {
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
        run_lint::<NearDuplicateShellBlock>(src)
    }

    #[test]
    fn flags_three_install_lines_with_varying_name() {
        // Single-column variation — three install lines differ only in
        // the source-file token.
        let src = "Name: x\n%install\n\
install -D -m644 svc1.service /usr/lib/systemd/system/\n\
install -D -m644 svc2.service /usr/lib/systemd/system/\n\
install -D -m644 svc3.service /usr/lib/systemd/system/\n";
        let diags = run(src);
        assert!(diags.iter().any(|d| d.lint_id == "RPM536"), "{diags:?}");
    }

    #[test]
    fn silent_for_distinct_commands() {
        let src = "Name: x\n%install\nls\necho hi\nmkdir -p target\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_below_threshold() {
        let src = "Name: x\n%install\n\
install -m644 a /tmp/a\ninstall -m644 b /tmp/b\n";
        assert!(run(src).is_empty());
    }
}
