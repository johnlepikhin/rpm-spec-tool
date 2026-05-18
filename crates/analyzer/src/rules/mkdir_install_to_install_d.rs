//! RPM530 `mkdir-install-to-install-d` — flag `mkdir -p DIR` +
//! `install -m… src DIR/file` (without `-D`) pairs that collapse to
//! a single `install -D -m… src DIR/file`.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::shell_walk::{for_each_shell_body, render_shell_line};
use crate::rules::util::extract_mkdir_p_target;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM530",
    name: "mkdir-install-to-install-d",
    description: "`mkdir -p DIR` followed by `install -m… src DIR/file` — fold into a single \
                  `install -D -m… src DIR/file`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `mkdir -p DIR` followed by `install -m… src DIR/file` — fold into a single `install -D -m… src DIR/file`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MkdirInstallToInstallD {
    diagnostics: Vec<Diagnostic>,
}

impl MkdirInstallToInstallD {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for MkdirInstallToInstallD {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut hits: Vec<Span> = Vec::new();
        for_each_shell_body(spec, |body, anchor| {
            let mut prev_mkdir: Option<String> = None;
            for line in &body.lines {
                let raw = render_shell_line(line);
                let trimmed = raw.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if let Some(prev) = &prev_mkdir
                    && let Some(install_target) = extract_install_target_no_dash_d(trimmed)
                    && install_target.starts_with(&format!("{prev}/"))
                {
                    hits.push(anchor);
                }
                prev_mkdir = extract_mkdir_p_target(trimmed);
            }
        });
        for anchor in hits {
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    "`mkdir -p DIR` + `install -m… src DIR/file` — collapse to a single \
                     `install -D -m… src DIR/file`",
                    anchor,
                )
                .with_suggestion(Suggestion::new(
                    "drop the `mkdir -p` and add `-D` to the `install` line",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

/// `install [-m MODE] SRC DST` — returns DST. Skips when `-D` is
/// already present (that's RPM531's case).
fn extract_install_target_no_dash_d(line: &str) -> Option<String> {
    let mut words = line.split_ascii_whitespace();
    let cmd = words.next()?;
    if cmd != "install" && cmd != "/usr/bin/install" {
        return None;
    }
    let mut tokens: Vec<&str> = words.collect();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if t == "-D" {
            return None;
        } else if t == "-m" || t == "-o" || t == "-g" {
            i += 2;
            continue;
        } else if t.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    tokens.drain(0..i);
    if tokens.len() < 2 {
        return None;
    }
    Some(tokens.last().unwrap().to_string())
}

impl Lint for MkdirInstallToInstallD {
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
        run_lint::<MkdirInstallToInstallD>(src)
    }

    #[test]
    fn flags_mkdir_then_install_no_dash_d() {
        let src = "Name: x\n%install\nmkdir -p target\ninstall -m755 src target/file\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM530");
    }

    #[test]
    fn silent_when_install_already_has_dash_d() {
        // RPM531's territory.
        let src = "Name: x\n%install\nmkdir -p target\ninstall -D -m755 src target/file\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_target_mismatch() {
        let src = "Name: x\n%install\nmkdir -p target\ninstall -m755 src other/file\n";
        assert!(run(src).is_empty());
    }
}
