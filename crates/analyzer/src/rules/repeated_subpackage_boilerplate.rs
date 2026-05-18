//! RPM553 `repeated-subpackage-boilerplate` — flag two or more
//! subpackages whose preamble blocks (sans `Summary`) are byte-for-
//! byte identical.
//!
//! When the boilerplate is shared, extracting it into a `%global` or
//! a helper macro keeps the subpackages in sync on every edit.

use std::collections::HashMap;

use rpm_spec::ast::{
    PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem, Tag, TagValue,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM553",
    name: "repeated-subpackage-boilerplate",
    description: "Two or more subpackages share the same preamble boilerplate — extract the \
                  common lines into one place.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Two or more subpackages share the same preamble boilerplate — extract the common lines into one place.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RepeatedSubpackageBoilerplate {
    diagnostics: Vec<Diagnostic>,
}

impl RepeatedSubpackageBoilerplate {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RepeatedSubpackageBoilerplate {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let mut buckets: HashMap<String, Vec<Span>> = HashMap::new();
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Package { content, data, .. } = boxed.as_ref() else {
                continue;
            };
            let key = canonicalise_preamble(content);
            if key.is_empty() {
                continue;
            }
            buckets.entry(key).or_default().push(*data);
        }
        for (_key, spans) in buckets {
            if spans.len() < 2 {
                continue;
            }
            for span in spans {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "this subpackage's preamble matches another subpackage's preamble — \
                         consider extracting the shared lines into a `%global` or helper macro",
                        span,
                    )
                    .with_suggestion(Suggestion::new(
                        "define common values once at the top of the spec and reference them \
                         from each subpackage",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

/// Build a stable canonical key for a subpackage's preamble. Skips
/// `Summary` (which is expected to differ per subpackage), `Group`
/// (often profile-gated), and any blank/comment items. Items are
/// sorted to make the comparison order-insensitive.
fn canonicalise_preamble(content: &[PreambleContent<Span>]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for it in content {
        if let PreambleContent::Item(p) = it
            && let Some(s) = item_to_canon(p)
        {
            lines.push(s);
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    lines.sort();
    lines.join("\n")
}

fn item_to_canon(p: &PreambleItem<Span>) -> Option<String> {
    use rpm_spec::ast::TextSegment;
    // Skip volatile / per-subpkg-by-design tags.
    if matches!(p.tag, Tag::Summary | Tag::Group) {
        return None;
    }
    let tag_label = format!("{:?}", p.tag);
    let value = match &p.value {
        TagValue::Text(t) => {
            let mut out = String::new();
            for seg in &t.segments {
                match seg {
                    TextSegment::Literal(s) => out.push_str(s),
                    TextSegment::Macro(m) => out.push_str(&format!("%{{{}}}", m.name)),
                    _ => {}
                }
            }
            out.trim().to_owned()
        }
        TagValue::Dep(_) => format!("{:?}", &p.value),
        _ => return None,
    };
    if value.is_empty() {
        return None;
    }
    Some(format!("{tag_label}={value}"))
}

impl Lint for RepeatedSubpackageBoilerplate {
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
        run_lint::<RepeatedSubpackageBoilerplate>(src)
    }

    #[test]
    fn flags_two_subpkgs_with_same_boilerplate() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nLicense: MIT\n\
%package a\n\
Summary: a\n\
License: MIT\n\
Requires: shared\n\
%description a\nbody a\n\
%package b\n\
Summary: b\n\
License: MIT\n\
Requires: shared\n\
%description b\nbody b\n";
        let diags = run(src);
        assert_eq!(diags.len(), 2, "{diags:?}");
    }

    #[test]
    fn silent_for_distinct_preambles() {
        let src = "Name: x\n\
%package a\n\
Summary: a\n\
License: MIT\n\
Requires: lib-a\n\
%description a\nbody a\n\
%package b\n\
Summary: b\n\
License: MIT\n\
Requires: lib-b\n\
%description b\nbody b\n";
        assert!(run(src).is_empty());
    }
}
