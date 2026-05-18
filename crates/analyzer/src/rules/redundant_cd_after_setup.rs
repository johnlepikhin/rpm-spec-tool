//! RPM475 `redundant-cd-after-setup` — flag `cd %{name}-%{version}`
//! (or equivalent) immediately after `%setup` / `%autosetup`.
//!
//! `%setup` already changes into the unpacked source directory, so a
//! manual `cd` to the same path is dead code that confuses readers.

use rpm_spec::ast::{Span, SpecFile, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::prep_model::find_prep_body_with_span;
use crate::rules::shell_walk::render_shell_line;
use crate::rules::util::{MACRO_AUTOSETUP, MACRO_SETUP};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM475",
    name: "redundant-cd-after-setup",
    description: "`cd %{name}-%{version}` (or the same directory `%setup` already entered) \
                  immediately follows `%setup`/`%autosetup`; drop the redundant `cd`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `cd %{name}-%{version}` (or the same directory `%setup` already entered) immediately follows `%setup`/`%autosetup`; drop the redundant `cd`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RedundantCdAfterSetup {
    diagnostics: Vec<Diagnostic>,
}

impl RedundantCdAfterSetup {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RedundantCdAfterSetup {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some((body, prep_span)) = find_prep_body_with_span(spec) else {
            return;
        };
        let mut prev_was_setup = false;
        for line in &body.lines {
            let raw = render_shell_line(line);
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if prev_was_setup && is_default_dir_cd(trimmed) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "`cd %{name}-%{version}` right after `%setup`/`%autosetup` repeats the \
                         directory the setup macro already entered",
                        prep_span,
                    )
                    .with_suggestion(Suggestion::new(
                        "drop the redundant `cd` — `%setup` chdirs for you",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
            prev_was_setup = line_has_setup(line);
        }
    }
}

fn line_has_setup(line: &rpm_spec::ast::Text) -> bool {
    line.segments.iter().any(|s| match s {
        TextSegment::Macro(m) => m.name == MACRO_SETUP || m.name == MACRO_AUTOSETUP,
        _ => false,
    })
}

fn is_default_dir_cd(line: &str) -> bool {
    let after_cd = match line.strip_prefix("cd ") {
        Some(rest) => rest.trim(),
        None => return false,
    };
    matches!(
        after_cd,
        "%{name}-%{version}" | "%{srcdir}" | "%{name}_%{version}"
    )
}

impl Lint for RedundantCdAfterSetup {
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
        run_lint::<RedundantCdAfterSetup>(src)
    }

    #[test]
    fn flags_cd_name_version_after_setup() {
        let src = "Name: x\n%prep\n%setup -q\ncd %{name}-%{version}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM475");
    }

    #[test]
    fn flags_cd_after_autosetup() {
        let src = "Name: x\n%prep\n%autosetup\ncd %{name}-%{version}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_when_cd_target_is_subdir() {
        let src = "Name: x\n%prep\n%setup -q\ncd subdir\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_no_cd_after_setup() {
        let src = "Name: x\n%prep\n%setup -q\nls\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_cd_not_adjacent() {
        // Something between setup and cd — adjacency check fails.
        let src = "Name: x\n%prep\n%setup -q\nls\ncd %{name}-%{version}\n";
        assert!(run(src).is_empty());
    }
}
