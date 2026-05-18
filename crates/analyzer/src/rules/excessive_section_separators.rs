//! RPM572 `excessive-section-separators` — flag decorative comment
//! lines like `########` or `==========` used as visual separators
//! between sections.
//!
//! Spec viewers and diff tools already make section boundaries
//! obvious; the decoration adds noise without information.

use rpm_spec::ast::{Span, SpecFile};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM572",
    name: "excessive-section-separators",
    description: "Decorative `#########` / `==========` separator comment — drop it; section \
                  boundaries are already obvious.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Decorative `#########` / `==========` separator comment — drop it; section boundaries are already obvious.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct ExcessiveSectionSeparators {
    diagnostics: Vec<Diagnostic>,
    source: std::sync::Arc<str>,
}

impl ExcessiveSectionSeparators {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ExcessiveSectionSeparators {
    fn visit_spec(&mut self, _spec: &'ast SpecFile<Span>) {
        if self.source.is_empty() {
            return;
        }
        // Split on `\n` and strip a trailing `\r` from each piece so
        // byte spans stay accurate on CRLF input. `str::lines()` would
        // hide the `\r` and we would drift one byte per CRLF line.
        let mut cursor = 0usize;
        for raw_line in self.source.split('\n') {
            let line_start = cursor;
            let raw_len = raw_line.len();
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            // Advance past the consumed `\n` (if any). The final
            // `split` chunk has no trailing `\n`, so guard at EOF.
            let consumed_newline = if cursor + raw_len < self.source.len() {
                1
            } else {
                0
            };
            cursor += raw_len + consumed_newline;
            if is_decorative_separator(line) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "decorative separator comment — drop it",
                        Span::from_bytes(line_start, line_start + line.len()),
                    )
                    .with_suggestion(Suggestion::new(
                        "remove the decorative comment line",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

/// `true` for lines like `#######`, `# ====== build ======`, `#-----`,
/// etc. We require either a run of 6+ consecutive decoration
/// characters somewhere in the line, OR the body to be ≥80%
/// decoration. The "consecutive run" branch catches `### build ###`;
/// the "ratio" branch catches `# - - - - - -`.
fn is_decorative_separator(line: &str) -> bool {
    let stripped = line.trim_start();
    let Some(body) = stripped.strip_prefix('#') else {
        return false;
    };
    let body = body.trim();
    if body.is_empty() {
        return false;
    }
    if has_decoration_run(body, 6) {
        return true;
    }
    let deco_chars: usize = body
        .chars()
        .filter(|c| matches!(*c, '#' | '=' | '-' | '*' | '_'))
        .count();
    let total: usize = body.chars().filter(|c| !c.is_whitespace()).count();
    deco_chars >= 6 && deco_chars * 5 >= total * 4
}

fn has_decoration_run(s: &str, min: usize) -> bool {
    let mut current = 0usize;
    for c in s.chars() {
        if matches!(c, '#' | '=' | '-' | '*' | '_') {
            current += 1;
            if current >= min {
                return true;
            }
        } else {
            current = 0;
        }
    }
    false
}

impl Lint for ExcessiveSectionSeparators {
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
        run_lint::<ExcessiveSectionSeparators>(src)
    }

    #[test]
    fn flags_pure_hash_separator() {
        let src = "Name: x\n#########\nVersion: 1\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM572");
    }

    #[test]
    fn flags_label_inside_separator() {
        let src = "Name: x\n# ====== build ======\n%build\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_normal_comment() {
        let src = "Name: x\n# Brief note about the package.\nVersion: 1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn correct_span_under_crlf_input() {
        // CRLF input: the `\r` is part of each line's byte run. The
        // span for the decorative `#########` line must point at the
        // actual bytes of `#########` in the source, not be shifted by
        // the number of preceding CRLF newlines.
        let src = "Name: x\r\n#########\r\nVersion: 1\r\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        let span = diags[0].primary_span;
        let start = span.start_byte;
        let end = span.end_byte;
        let slice = &src[start..end];
        assert_eq!(
            slice, "#########",
            "span pointed at `{slice}` instead of `#########`"
        );
    }
}
