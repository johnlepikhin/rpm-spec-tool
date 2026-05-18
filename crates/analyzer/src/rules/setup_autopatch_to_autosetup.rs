//! RPM470 `setup-autopatch-to-autosetup` — flag `%prep` bodies that
//! use `%setup` together with `%autopatch`.
//!
//! `%autosetup` is the single macro that combines the two: it unpacks
//! the source like `%setup` and applies every declared patch like
//! `%autopatch`. Specs that still spell out the two-step idiom can
//! be folded into one line.

use rpm_spec::ast::{Span, SpecFile, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::prep_model::find_prep_body_with_span;
use crate::rules::util::{MACRO_AUTOPATCH, MACRO_AUTOSETUP, MACRO_SETUP};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM470",
    name: "setup-autopatch-to-autosetup",
    description: "`%prep` invokes both `%setup` and `%autopatch` — fold into a single \
                  `%autosetup` call which handles unpacking and patch application together.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// `%prep` invokes both `%setup` and `%autopatch` — fold into a single `%autosetup` call which handles unpacking and patch application together.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct SetupAutopatchToAutosetup {
    diagnostics: Vec<Diagnostic>,
}

impl SetupAutopatchToAutosetup {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SetupAutopatchToAutosetup {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some((body, prep_span)) = find_prep_body_with_span(spec) else {
            return;
        };
        let mut has_setup = false;
        let mut autopatch_count: usize = 0;
        let mut has_autosetup = false;
        for line in &body.lines {
            for seg in &line.segments {
                let TextSegment::Macro(m) = seg else { continue };
                match m.name.as_str() {
                    MACRO_SETUP => has_setup = true,
                    MACRO_AUTOSETUP => has_autosetup = true,
                    MACRO_AUTOPATCH => autopatch_count += 1,
                    _ => {}
                }
            }
        }
        // `%autosetup` already in play → suggestion would be a no-op.
        if has_autosetup {
            return;
        }
        // Multiple `%autopatch` calls usually mean per-range patch
        // application (e.g. `-m N -M M` partitions); collapsing into a
        // single `%autosetup` would lose that granularity.
        if has_setup && autopatch_count == 1 {
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    "`%prep` uses `%setup` + `%autopatch`; collapse into one `%autosetup` call",
                    prep_span,
                )
                .with_suggestion(Suggestion::new(
                    "replace `%setup [-q -n NAME]` and `%autopatch [-pN]` with a single \
                     `%autosetup [-q -n NAME -pN]`",
                    Vec::new(),
                    Applicability::MaybeIncorrect,
                )),
            );
        }
    }
}

impl Lint for SetupAutopatchToAutosetup {
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
        run_lint::<SetupAutopatchToAutosetup>(src)
    }

    #[test]
    fn flags_setup_plus_autopatch() {
        let src = "Name: x\n%prep\n%setup -q -n foo\n%autopatch -p1\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM470");
    }

    #[test]
    fn silent_when_only_setup() {
        let src = "Name: x\n%prep\n%setup -q -n foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_only_autopatch() {
        // Highly unusual but still not RPM470's case.
        let src = "Name: x\n%prep\n%autopatch -p1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_autosetup_already_present() {
        let src = "Name: x\n%prep\n%autosetup -n foo -p1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_no_prep() {
        let src = "Name: x\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_multiple_autopatch_calls() {
        // Real-world repro (gcc.spec): per-range patch application
        // with separate `%autopatch -m N -M M` calls cannot be folded
        // into a single `%autosetup` without losing the partitions.
        let src = "Name: x\n%prep\n%setup -q -n foo\n%autopatch -p0 -m 0 -M 4\n%autopatch -p0 -m 5 -M 6\n";
        assert!(run(src).is_empty(), "{:?}", run(src));
    }
}
