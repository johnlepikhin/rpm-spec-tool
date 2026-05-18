//! RPM531 `redundant-mkdir-before-install-d` — flag `mkdir -p DIR`
//! lines that immediately precede an `install -D src DIR/file`
//! invocation. `install -D` creates the parent directory itself, so
//! the explicit `mkdir -p` is dead code.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::shell_walk::{for_each_shell_body, render_shell_line};
use crate::rules::util::extract_mkdir_p_target;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM531",
    name: "redundant-mkdir-before-install-d",
    description: "`mkdir -p DIR` followed by `install -D src DIR/file` — `install -D` already \
                  creates the parent directory; drop the `mkdir -p`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `mkdir -p DIR` followed by `install -D src DIR/file` — `install -D` already creates the parent directory; drop the `mkdir -p`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RedundantMkdirBeforeInstallD {
    diagnostics: Vec<Diagnostic>,
}

impl RedundantMkdirBeforeInstallD {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RedundantMkdirBeforeInstallD {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut hits: Vec<Span> = Vec::new();
        for_each_shell_body(spec, |body, anchor| {
            let mut prev_mkdir_target: Option<String> = None;
            for line in &body.lines {
                let raw = render_shell_line(line);
                let trimmed = raw.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if let Some(prev) = &prev_mkdir_target
                    && let Some(install_target) = extract_install_d_target(trimmed)
                    && install_target.starts_with(&format!("{prev}/"))
                {
                    hits.push(anchor);
                }
                prev_mkdir_target = extract_mkdir_p_target(trimmed);
            }
        });
        for anchor in hits {
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    "`mkdir -p` is redundant — the following `install -D` already creates the \
                     parent directory",
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "drop the `mkdir -p` line; `install -D` will create the parent on demand",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

fn extract_install_d_target(line: &str) -> Option<String> {
    let mut words = line.split_ascii_whitespace();
    let cmd = words.next()?;
    if cmd != "install" && cmd != "/usr/bin/install" {
        return None;
    }
    // `install -D [-m MODE] SRC DST`.
    let mut has_dash_d = false;
    let mut tokens: Vec<&str> = words.collect();
    if tokens.is_empty() {
        return None;
    }
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if t == "-D" {
            has_dash_d = true;
        } else if t == "-m" || t == "-o" || t == "-g" {
            // Skip flag + value.
            i += 2;
            continue;
        } else if t.starts_with('-') {
            // unknown flag — skip alone
        } else {
            // Found a non-flag token — start of SRC/DST list.
            break;
        }
        i += 1;
    }
    if !has_dash_d {
        return None;
    }
    // Remaining: SRC1 [SRC2 ...] DST.
    tokens.drain(0..i);
    if tokens.len() < 2 {
        return None;
    }
    Some(tokens.last().unwrap().to_string())
}

impl Lint for RedundantMkdirBeforeInstallD {
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
        run_lint::<RedundantMkdirBeforeInstallD>(src)
    }

    #[test]
    fn flags_mkdir_followed_by_install_d() {
        let src = "Name: x\n%install\nmkdir -p target\ninstall -D -m644 src target/file\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM531");
    }

    #[test]
    fn silent_when_install_targets_different_dir() {
        let src = "Name: x\n%install\nmkdir -p target\ninstall -D -m644 src other/file\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_install_lacks_dash_d() {
        let src = "Name: x\n%install\nmkdir -p target\ninstall -m644 src target/file\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_separated_by_other_command() {
        let src = "Name: x\n%install\nmkdir -p target\nls\ninstall -D -m644 src target/file\n";
        assert!(run(src).is_empty());
    }
}
