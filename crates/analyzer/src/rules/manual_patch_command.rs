//! RPM474 `manual-patch-command-to-patch-macro` — flag bare `patch`
//! invocations that read from `%{PATCH<N>}`.
//!
//! ```text
//! patch -p1 < %{PATCH0}
//! ```
//! should be expressed as `%patch -P 0 -p1`, which RPM tracks against
//! the declared `Patch:` tags and which downstream patch-management
//! tooling (`rpmdev-bumpspec --new-patch` etc.) understands.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::prep_model::find_prep_body_with_span;
use crate::rules::shell_walk::render_shell_line;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM474",
    name: "manual-patch-command-to-patch-macro",
    description: "`patch <flags> < %{PATCH<N>}` is the manual form; use `%patch -P <N> <flags>` \
                  so RPM tracks the application against the declared `Patch:` tag.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `patch <flags> < %{PATCH<N>}` is the manual form; use `%patch -P <N> <flags>` so RPM tracks the application against the declared `Patch:` tag.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ManualPatchCommand {
    diagnostics: Vec<Diagnostic>,
}

impl ManualPatchCommand {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ManualPatchCommand {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some((body, prep_span)) = find_prep_body_with_span(spec) else {
            return;
        };
        for line in &body.lines {
            let raw = render_shell_line(line);
            let trimmed = raw.trim();
            let Some(n) = match_patch_redirect(trimmed) else {
                continue;
            };
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "manual `patch < %{{PATCH{n}}}` invocation — use `%patch -P {n}` so RPM \
                         tracks the application"
                    ),
                    prep_span,
                )
                .with_suggestion(Suggestion::new(
                    format!("rewrite as `%patch -P {n} <flags>`"),
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

/// Match `patch [flags] < %{PATCH<N>}` (case-insensitive on the macro
/// name) and return `N`. The leading word must be `patch` (not part of
/// a longer command).
fn match_patch_redirect(line: &str) -> Option<u32> {
    let mut words = line.split_ascii_whitespace();
    let first = words.next()?;
    if first != "patch" && first != "/usr/bin/patch" {
        return None;
    }
    // The redirect form `< %{PATCHN}` may appear anywhere on the line.
    let lt = line.find('<')?;
    let after = line[lt + 1..].trim_start();
    let body = after.strip_prefix("%{")?.split('}').next()?;
    let name = body.trim();
    let rest = name
        .strip_prefix("PATCH")
        .or_else(|| name.strip_prefix("patch"))?;
    rest.parse::<u32>().ok()
}

impl Lint for ManualPatchCommand {
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
        run_lint::<ManualPatchCommand>(src)
    }

    #[test]
    fn flags_patch_redirect() {
        let src = "Name: x\nPatch0: foo.patch\n%prep\n%setup -q\npatch -p1 < %{PATCH0}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM474");
        assert!(diags[0].message.contains("-P 0"));
    }

    #[test]
    fn flags_higher_patch_number() {
        let src = "Name: x\nPatch12: foo.patch\n%prep\n%setup -q\npatch -p0 < %{PATCH12}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("PATCH12"));
    }

    #[test]
    fn silent_for_proper_patch_macro() {
        let src = "Name: x\nPatch0: foo.patch\n%prep\n%setup -q\n%patch -P 0 -p1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_unrelated_command() {
        let src = "Name: x\n%prep\n%setup -q\necho hello\n";
        assert!(run(src).is_empty());
    }
}
