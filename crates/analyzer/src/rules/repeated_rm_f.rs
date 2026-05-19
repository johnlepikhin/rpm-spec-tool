//! RPM534 `repeated-rm-f-combine` — flag adjacent `rm -f` lines in
//! shell-body sections; they can be folded into one `rm -f` line.

use rpm_spec::ast::{Scriptlet, Section, ShellBody, Span, SpecFile, SpecItem, Trigger};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::shell_walk::render_shell_line;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM534",
    name: "repeated-rm-f-combine",
    description: "Adjacent `rm -f` lines in a shell-body section — combine into one `rm -f` \
                  with all targets.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Adjacent `rm -f` lines in a shell-body section — combine into one `rm -f` with all targets.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RepeatedRmF {
    diagnostics: Vec<Diagnostic>,
}

impl RepeatedRmF {
    pub fn new() -> Self {
        Self::default()
    }

    fn scan_body(&mut self, body: &ShellBody<Span>, anchor: Span) {
        let mut prev_was_rm_f = false;
        for line in &body.lines {
            let raw = render_shell_line(line);
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let is_rm_f = is_rm_f_line(trimmed);
            if prev_was_rm_f && is_rm_f {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "adjacent `rm -f` lines — combine into one `rm -f a b c …`",
                        anchor,
                    )
                    .with_suggestion(Suggestion::new(
                        "merge the consecutive `rm -f` invocations into a single line",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
            prev_was_rm_f = is_rm_f;
        }
    }
}

fn is_rm_f_line(line: &str) -> bool {
    let mut words = line.split_ascii_whitespace();
    let Some(cmd) = words.next() else {
        return false;
    };
    if cmd != "rm" && cmd != "/bin/rm" && cmd != "/usr/bin/rm" {
        return false;
    }
    let Some(flag) = words.next() else {
        return false;
    };
    // Accept `-f`, `-rf`, `-fr`, `-rfv`, etc. The combine is safe as
    // long as both lines have the same flag set; for the MVP we
    // require BOTH to be exactly `-f`.
    if flag != "-f" {
        return false;
    }
    // Need at least one target.
    words.next().is_some()
}

impl<'ast> Visit<'ast> for RepeatedRmF {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for it in &spec.items {
            let SpecItem::Section(boxed) = it else {
                continue;
            };
            match boxed.as_ref() {
                Section::BuildScript { body, data, .. } => self.scan_body(body, *data),
                Section::Verify { body, data, .. } => self.scan_body(body, *data),
                Section::Scriptlet(Scriptlet { body, data, .. }) => self.scan_body(body, *data),
                Section::Trigger(Trigger { body, data, .. }) => self.scan_body(body, *data),
                _ => {}
            }
        }
    }
}

impl Lint for RepeatedRmF {
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
        run_lint::<RepeatedRmF>(src)
    }

    #[test]
    fn flags_adjacent_rm_f() {
        let src = "Name: x\n%install\nrm -f a\nrm -f b\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM534");
    }

    #[test]
    fn silent_for_single_rm() {
        let src = "Name: x\n%install\nrm -f a\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_non_rm_lines_between() {
        let src = "Name: x\n%install\nrm -f a\nls\nrm -f b\n";
        assert!(run(src).is_empty());
    }
}
