//! RPM473 `manual-tar-cd-to-setup` — flag manual `tar xf %{SOURCEN}`
//! invocations in `%prep`.
//!
//! `%setup` is the canonical way to extract sources: it picks the
//! right tar flags for the archive type, sets up `$RPM_BUILD_DIR`,
//! and integrates with `%autosetup` / `%autopatch`. Hand-rolled `tar`
//! commands miss those affordances.

use rpm_spec::ast::{Span, SpecFile, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::prep_model::find_prep_body_with_span;
use crate::rules::shell_walk::render_shell_line;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM473",
    name: "manual-tar-cd-to-setup",
    description: "Manual `tar xf %{SOURCE<N>}` extraction in `%prep` — prefer `%setup` so RPM \
                  picks the right tar flags and integrates with `%autosetup` / `%autopatch`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Manual `tar xf %{SOURCE<N>}` extraction in `%prep` — prefer `%setup` so RPM picks the right tar flags and integrates with `%autosetup` / `%autopatch`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ManualTarCdToSetup {
    diagnostics: Vec<Diagnostic>,
}

impl ManualTarCdToSetup {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ManualTarCdToSetup {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some((body, prep_span)) = find_prep_body_with_span(spec) else {
            return;
        };
        // RPM473 targets the "pure manual prep" case: no `%setup` /
        // `%autosetup` in the body at all. When the spec already uses
        // setup, additional `tar xf %{SOURCEN}` for N >= 1 is RPM476's
        // territory.
        let has_setup = body.lines.iter().any(|line| {
            line.segments.iter().any(|s| {
                matches!(
                    s,
                    TextSegment::Macro(m) if m.name == crate::rules::util::MACRO_SETUP
                        || m.name == crate::rules::util::MACRO_AUTOSETUP
                )
            })
        });
        if has_setup {
            return;
        }
        for line in &body.lines {
            let raw = render_shell_line(line);
            let trimmed = raw.trim();
            let Some(n) = match_tar_source(trimmed) else {
                continue;
            };
            let n_label = match n {
                Some(n) => format!("%{{SOURCE{n}}}"),
                None => "%{SOURCE…}".to_string(),
            };
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "manual `tar` extraction of {n_label} in `%prep` — use `%setup` so RPM \
                         handles the tarball uniformly"
                    ),
                    prep_span,
                )
                .with_suggestion(Suggestion::new(
                    "replace the manual `tar` + `cd` pair with `%setup -q -n <topdir>`",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

/// Match a `tar` line that references `%{SOURCE<N>}`. Returns the
/// number or `None` for the bare `%{SOURCE}` form. Returns the outer
/// `Option<Option<u32>>` as `None` when the line isn't a tar
/// extraction at all.
fn match_tar_source(line: &str) -> Option<Option<u32>> {
    let mut words = line.split_ascii_whitespace();
    let first = words.next()?;
    if first != "tar" && first != "/usr/bin/tar" && first != "/bin/tar" {
        return None;
    }
    // Look for a `%{SOURCE<N>}` token anywhere in the line.
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(rest) = line.get(i..)
            && let Some(after_open) = rest.strip_prefix("%{")
            && let Some(close_idx) = after_open.find('}')
        {
            let inner = &after_open[..close_idx];
            let trimmed = inner.trim();
            if let Some(num_part) = trimmed
                .strip_prefix("SOURCE")
                .or_else(|| trimmed.strip_prefix("source"))
            {
                return Some(num_part.parse::<u32>().ok());
            }
            i += 2 + close_idx + 1;
            continue;
        }
        i += 1;
    }
    None
}

impl Lint for ManualTarCdToSetup {
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
        run_lint::<ManualTarCdToSetup>(src)
    }

    #[test]
    fn flags_tar_extraction_of_source0() {
        let src = "Name: x\n%prep\ntar xf %{SOURCE0}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM473");
        assert!(diags[0].message.contains("SOURCE0"));
    }

    #[test]
    fn flags_tar_with_higher_source_index() {
        let src = "Name: x\n%prep\ntar xfz %{SOURCE3}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_setup_macro() {
        let src = "Name: x\n%prep\n%setup -q\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_unrelated_tar_usage() {
        // tar in shell, not extracting a SOURCE — leave alone.
        let src = "Name: x\n%prep\n%setup -q\ntar cf out.tar files/\n";
        assert!(run(src).is_empty());
    }
}
