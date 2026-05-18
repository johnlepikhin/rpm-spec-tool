//! RPM532 `combine-install-dirs` — flag adjacent `install -d` (or
//! `mkdir -p`) lines that can be merged into one invocation.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::shell_walk::{for_each_shell_body, render_shell_line};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM532",
    name: "combine-install-dirs",
    description: "Adjacent `install -d` (or `mkdir -p`) lines — combine into one invocation \
                  with all directories.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Adjacent `install -d` (or `mkdir -p`) lines — combine into one invocation with all directories.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct CombineInstallDirs {
    diagnostics: Vec<Diagnostic>,
}

impl CombineInstallDirs {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for CombineInstallDirs {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut hits: Vec<Span> = Vec::new();
        for_each_shell_body(spec, |body, anchor| {
            let mut prev_kind: Option<MkdirKind> = None;
            for line in &body.lines {
                let raw = render_shell_line(line);
                let trimmed = raw.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let curr = classify(trimmed);
                if curr.is_some() && curr == prev_kind {
                    hits.push(anchor);
                }
                prev_kind = curr;
            }
        });
        for anchor in hits {
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    "adjacent directory-creation lines — combine into one `install -d` / \
                     `mkdir -p` invocation",
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "merge the consecutive lines into one with all targets on one command",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MkdirKind {
    InstallD,
    MkdirP,
}

fn classify(line: &str) -> Option<MkdirKind> {
    let mut words = line.split_ascii_whitespace();
    let cmd = words.next()?;
    let flag = words.next()?;
    let _arg = words.next()?;
    match (cmd, flag) {
        ("install" | "/usr/bin/install", "-d") => Some(MkdirKind::InstallD),
        ("mkdir" | "/bin/mkdir" | "/usr/bin/mkdir", "-p") => Some(MkdirKind::MkdirP),
        _ => None,
    }
}

impl Lint for CombineInstallDirs {
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
        run_lint::<CombineInstallDirs>(src)
    }

    #[test]
    fn flags_adjacent_install_d() {
        let src = "Name: x\n%install\ninstall -d a\ninstall -d b\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM532");
    }

    #[test]
    fn flags_adjacent_mkdir_p() {
        let src = "Name: x\n%install\nmkdir -p a\nmkdir -p b\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_mixed_install_and_mkdir() {
        // Different kinds — don't combine.
        let src = "Name: x\n%install\ninstall -d a\nmkdir -p b\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_separated() {
        let src = "Name: x\n%install\ninstall -d a\necho hi\ninstall -d b\n";
        assert!(run(src).is_empty());
    }
}
