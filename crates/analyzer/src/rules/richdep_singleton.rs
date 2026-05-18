//! RPM590 `richdep-singleton` — flag rich-dep declarations wrapping a
//! single atom in unnecessary parentheses (`Requires: (foo)`).
//!
//! The parser collapses `(foo)` to a plain atom internally, so this
//! rule scans the source slice covered by each dep line for the
//! pattern `(SINGLE_ATOM)`.

use rpm_spec::ast::{PreambleItem, Span, SpecFile, Tag, TagValue};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{DepTagKey, collect_top_level_preamble};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM590",
    name: "richdep-singleton",
    description: "Rich-dep declaration wraps a single atom in `(…)` — drop the parentheses.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Rich-dep declaration wraps a single atom in `(…)` — drop the parentheses.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RichdepSingleton {
    diagnostics: Vec<Diagnostic>,
    source: std::sync::Arc<str>,
}

impl RichdepSingleton {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RichdepSingleton {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        if self.source.is_empty() {
            return;
        }
        for item in collect_top_level_preamble(spec) {
            if !is_dep_tag(&item.tag) {
                continue;
            }
            if !matches!(item.value, TagValue::Dep(_)) {
                continue;
            }
            if let Some(value_text) = preamble_value_text(item, &self.source)
                && looks_like_paren_wrapped_singleton(value_text)
            {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "rich-dep declaration wraps a single atom in `(…)` — drop the parens",
                        item.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "remove the surrounding parentheses; a single atom doesn't need them",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn is_dep_tag(t: &Tag) -> bool {
    DepTagKey::from_tag(t).is_some()
}

/// Slice the source under `item.data` and return the part after the
/// first `:` (i.e. the tag's value text).
fn preamble_value_text<'a>(item: &PreambleItem<Span>, source: &'a str) -> Option<&'a str> {
    let end = item.data.end_byte.min(source.len());
    let start = item.data.start_byte.min(end);
    let slice = source.get(start..end)?;
    let (_tag, value) = slice.split_once(':')?;
    Some(value)
}

/// `true` when `text` is `(<single-atom>)`, optionally with whitespace
/// around it. A "single atom" here means a token with no rich-dep
/// operators (`and`, `or`, `with`, `without`, `if`, `unless`, `else`).
fn looks_like_paren_wrapped_singleton(text: &str) -> bool {
    let trimmed = text.trim();
    let Some(inner) = trimmed.strip_prefix('(').and_then(|s| s.strip_suffix(')')) else {
        return false;
    };
    let inner = inner.trim();
    if inner.is_empty() {
        return false;
    }
    // Single atom: no rich-dep operator keyword surrounded by whitespace.
    let tokens: Vec<&str> = inner.split_ascii_whitespace().collect();
    for w in &tokens {
        if matches!(
            *w,
            "and" | "or" | "with" | "without" | "if" | "unless" | "else"
        ) {
            return false;
        }
    }
    // Reject nested parens — those imply more structure.
    if inner.contains('(') || inner.contains(')') {
        return false;
    }
    true
}

impl Lint for RichdepSingleton {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: std::sync::Arc<str>) {
        self.source = source;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::test_support::run_lint;

    fn run(src: &str) -> Vec<Diagnostic> {
        run_lint::<RichdepSingleton>(src)
    }

    #[test]
    fn flags_paren_wrapped_single_atom() {
        let src = "Name: x\nRequires: (foo)\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM590");
    }

    #[test]
    fn flags_paren_wrapped_with_version() {
        let src = "Name: x\nRequires: (foo >= 1.2)\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_real_rich_dep() {
        let src = "Name: x\nRequires: (foo and bar)\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_plain_atom() {
        let src = "Name: x\nRequires: foo\n";
        assert!(run(src).is_empty());
    }
}
