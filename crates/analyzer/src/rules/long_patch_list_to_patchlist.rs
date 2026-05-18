//! RPM477 `long-patch-list-to-patchlist` — flag specs that declare a
//! long sequence of `PatchN:` tags.
//!
//! `%patchlist` (rpm ≥ 4.15) lets you keep every patch filename in a
//! single block instead of repeating the `PatchN:` boilerplate. Above
//! a small threshold of declared patches, switching to `%patchlist`
//! makes the preamble far easier to scan.

use rpm_spec::ast::{Section, Span, SpecFile, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{collect_declared_patches, spec_span};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM477",
    name: "long-patch-list-to-patchlist",
    description: "Spec declares many `PatchN:` tags — switch to a single `%patchlist` block \
                  (rpm ≥ 4.15) for a more compact preamble.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Below this many declared patches the boilerplate cost is small; above
/// it `%patchlist` pays for itself. Five is the same threshold used by
/// Fedora packaging guidelines for "long list of patches".
const THRESHOLD: usize = 5;

/// Spec declares many `PatchN:` tags — switch to a single `%patchlist` block (rpm ≥ 4.15) for a more compact preamble.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct LongPatchListToPatchlist {
    diagnostics: Vec<Diagnostic>,
}

impl LongPatchListToPatchlist {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for LongPatchListToPatchlist {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Don't fire when the spec already has a `%patchlist` block —
        // RPM305 handles the mixed-form case separately.
        if has_patchlist_section(spec) {
            return;
        }
        let declared = collect_declared_patches(spec);
        if declared.len() < THRESHOLD {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "{n} `Patch:` declarations — collapse into a single `%patchlist` block for \
                     readability (requires rpm ≥ 4.15)",
                    n = declared.len(),
                ),
                spec_span(spec),
            )
            .with_suggestion(Suggestion::new(
                "move every patch filename into one `%patchlist` block and drop the per-line \
                 `PatchN:` tags",
                Vec::new(),
                Applicability::Manual,
            )),
        );
    }
}

fn has_patchlist_section(spec: &SpecFile<Span>) -> bool {
    spec.items.iter().any(|it| {
        let SpecItem::Section(boxed) = it else {
            return false;
        };
        matches!(boxed.as_ref(), Section::PatchList { .. })
    })
}

impl Lint for LongPatchListToPatchlist {
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
        run_lint::<LongPatchListToPatchlist>(src)
    }

    #[test]
    fn flags_at_threshold() {
        let src = "Name: x\n\
Patch0: a.patch\n\
Patch1: b.patch\n\
Patch2: c.patch\n\
Patch3: d.patch\n\
Patch4: e.patch\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM477");
    }

    #[test]
    fn silent_below_threshold() {
        let src = "Name: x\n\
Patch0: a.patch\n\
Patch1: b.patch\n\
Patch2: c.patch\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_no_patches() {
        let src = "Name: x\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_patchlist_already_present() {
        let src = "Name: x\n\
Patch0: a.patch\n\
Patch1: b.patch\n\
Patch2: c.patch\n\
Patch3: d.patch\n\
Patch4: e.patch\n\
%patchlist\nfoo.patch\nbar.patch\n";
        // `%patchlist` co-existing with `PatchN:` is RPM305's case;
        // RPM477 stays silent here.
        assert!(run(src).is_empty());
    }
}
