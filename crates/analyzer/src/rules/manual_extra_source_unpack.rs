//! RPM476 `manual-extra-source-unpack-to-setup-a-b` — flag manual
//! `tar xf %{SOURCEN}` (`N >= 1`) when the spec already uses `%setup`
//! / `%autosetup`.
//!
//! `%setup` accepts `-a N` (extract `SourceN` inside the source dir
//! *after* `cd`) or `-b N` (extract `SourceN` in the source dir's
//! parent *before* `cd`). Both fold the manual extra extraction back
//! into the canonical setup invocation.

use rpm_spec::ast::{Span, SpecFile, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::prep_model::find_prep_body_with_span;
use crate::rules::shell_walk::render_shell_line;
use crate::rules::util::{MACRO_AUTOSETUP, MACRO_SETUP};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM476",
    name: "manual-extra-source-unpack-to-setup-a-b",
    description: "Manual `tar xf %{SOURCE<N>}` (N >= 1) alongside `%setup` — fold into \
                  `%setup -a N` or `%setup -b N`.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Manual `tar xf %{SOURCE<N>}` (N >= 1) alongside `%setup` — fold into `%setup -a N` or `%setup -b N`.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ManualExtraSourceUnpack {
    diagnostics: Vec<Diagnostic>,
}

impl ManualExtraSourceUnpack {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ManualExtraSourceUnpack {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some((body, prep_span)) = find_prep_body_with_span(spec) else {
            return;
        };
        let has_setup = body.lines.iter().any(|line| {
            line.segments.iter().any(|s| {
                matches!(
                    s,
                    TextSegment::Macro(m) if m.name == MACRO_SETUP || m.name == MACRO_AUTOSETUP
                )
            })
        });
        if !has_setup {
            return;
        }
        for line in &body.lines {
            let raw = render_shell_line(line);
            let trimmed = raw.trim();
            let Some(n) = match_tar_source_n(trimmed) else {
                continue;
            };
            if n == 0 {
                // SOURCE0 is the main source — RPM473's territory when
                // setup is absent; here setup IS present so SOURCE0 is
                // already extracted. A redundant manual `tar xf SOURCE0`
                // is unusual and out of RPM476's scope.
                continue;
            }
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "manual `tar xf %{{SOURCE{n}}}` alongside `%setup` — use `%setup -a {n}` \
                         (or `-b {n}`) instead"
                    ),
                    prep_span,
                )
                .with_suggestion(Suggestion::new(
                    format!(
                        "add `-a {n}` (extract after cd) or `-b {n}` (extract before cd) to \
                         the `%setup` call"
                    ),
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

/// Match `tar [flags] %{SOURCE<N>}` (with optional redirect `< …` etc.)
/// and return `N` for the LITERAL numeric variant; `None` if not a tar
/// + SOURCE line, or if the source index isn't a literal integer.
fn match_tar_source_n(line: &str) -> Option<u32> {
    let mut words = line.split_ascii_whitespace();
    let first = words.next()?;
    if first != "tar" && first != "/usr/bin/tar" && first != "/bin/tar" {
        return None;
    }
    // `tar -C <dir>` / `tar --directory=<dir>` extracts into a custom
    // directory rather than the source-top — `%setup -a N` would put it
    // somewhere different. Bail out so we don't suggest a behaviourally
    // different rewrite.
    if line.contains(" -C ") || line.contains("--directory") {
        return None;
    }
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
                && let Ok(n) = num_part.parse::<u32>()
            {
                return Some(n);
            }
            i += 2 + close_idx + 1;
            continue;
        }
        i += 1;
    }
    None
}

impl Lint for ManualExtraSourceUnpack {
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
        run_lint::<ManualExtraSourceUnpack>(src)
    }

    #[test]
    fn flags_tar_source1_alongside_setup() {
        let src = "Name: x\nSource0: a.tar\nSource1: b.tar\n%prep\n%setup -q\ntar xf %{SOURCE1}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM476");
        assert!(diags[0].message.contains("-a 1"));
    }

    #[test]
    fn silent_for_tar_source0_with_setup() {
        // Unusual but not RPM476's case.
        let src = "Name: x\n%prep\n%setup -q\ntar xf %{SOURCE0}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_without_setup() {
        // RPM473's territory.
        let src = "Name: x\n%prep\ntar xf %{SOURCE1}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_no_tar_extract() {
        let src = "Name: x\n%prep\n%setup -q\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_tar_with_dash_c_custom_dir() {
        // Real-world repro (mesa.spec): `-C subprojects/...` extracts
        // outside the canonical source-top, so `%setup -a 1` would not
        // be equivalent.
        let src =
            "Name: x\nSource1: a.tar\n%prep\n%setup -q\ntar -xvf %{SOURCE1} -C subprojects/\n";
        assert!(run(src).is_empty(), "{:?}", run(src));
    }
}
