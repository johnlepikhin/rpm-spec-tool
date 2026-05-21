//! RPM554 `subpackage-description-copied-from-main` — flag a
//! subpackage's `%description` body that matches the main package's
//! body verbatim.
//!
//! A subpackage normally explains how its contents differ from the
//! main package. A verbatim copy is filler that misleads users about
//! what they're installing.

use rpm_spec::ast::{Section, Span, SpecFile, SpecItem, TextBody, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM554",
    name: "subpackage-description-copied-from-main",
    description: "A subpackage `%description` is byte-for-byte identical to the main package's \
                  description — write a description that explains how the subpackage differs.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// A subpackage `%description` is byte-for-byte identical to the main package's description — write a description that explains how the subpackage differs.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct SubpackageDescriptionCopy {
    diagnostics: Vec<Diagnostic>,
}

impl SubpackageDescriptionCopy {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SubpackageDescriptionCopy {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut main_text: Option<(String, bool)> = None;
        let mut sub_descs: Vec<(Span, String, bool)> = Vec::new();
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Description { subpkg, body, data } = boxed.as_ref() else {
                continue;
            };
            let canon = canonical_body(body);
            let has_lit = body_has_literal_content(body);
            if subpkg.is_none() {
                main_text = Some((canon, has_lit));
            } else {
                sub_descs.push((*data, canon, has_lit));
            }
        }
        let Some((main, main_has_lit)) = main_text else {
            return;
        };
        if main.is_empty() {
            return;
        }
        // If the main body has no real literal text (e.g. only `%{summary}.`),
        // per-subpackage expansion makes textual equality meaningless — skip.
        if !main_has_lit {
            return;
        }
        for (span, sub, sub_has_lit) in sub_descs {
            if !sub_has_lit {
                continue;
            }
            if sub == main {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "subpackage `%description` is identical to the main package's — write a \
                         description that explains the subpackage's purpose",
                        span,
                    )
                    .with_suggestion(Suggestion::new(
                        "replace the copied body with a description specific to this subpackage",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn canonical_body(body: &TextBody) -> String {
    let mut out = String::new();
    for (i, line) in body.lines.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        if let Some(s) = line.literal_str() {
            out.push_str(s.trim_end());
        } else {
            // Mixed-segment line — render macros as `%{name}`.
            for seg in &line.segments {
                match seg {
                    TextSegment::Literal(s) => out.push_str(s),
                    TextSegment::Macro(m) => out.push_str(&format!("%{{{}}}", m.name)),
                    _ => {}
                }
            }
        }
    }
    out.trim().to_owned()
}

/// True iff any literal segment contains at least one alphabetic char.
/// Bodies that are pure macro references (e.g. `%{summary}.`) return
/// false — their per-subpackage expansion differs even when the source
/// text is identical, so textual equality is not a useful signal.
fn body_has_literal_content(body: &TextBody) -> bool {
    for line in &body.lines {
        for seg in &line.segments {
            if let TextSegment::Literal(s) = seg
                && s.chars().any(|c| c.is_alphabetic())
            {
                return true;
            }
        }
    }
    false
}

impl Lint for SubpackageDescriptionCopy {
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
        run_lint::<SubpackageDescriptionCopy>(src)
    }

    #[test]
    fn flags_copied_description() {
        let src = "Name: x\n\
%description\n\
Acme tools for X.\n\
%package devel\n\
Summary: dev\n\
%description devel\n\
Acme tools for X.\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM554");
    }

    #[test]
    fn silent_when_body_is_only_summary_macro() {
        // Per-subpackage `%{summary}` expands to a different value, so
        // textually identical bodies are not actually duplicates.
        let src = "Name: x\n\
%description\n\
%{summary}.\n\
%package devel\n\
Summary: dev\n\
%description devel\n\
%{summary}.\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_distinct_description() {
        let src = "Name: x\n\
%description\n\
Acme tools for X.\n\
%package devel\n\
Summary: dev\n\
%description devel\n\
Development headers and libraries for Acme.\n";
        assert!(run(src).is_empty());
    }
}
