//! RPM051 `tab-indent` — flag lines that start with a tab character.
//! rpmlint reports this as `mixed-use-of-spaces-and-tabs` because tabs
//! interact badly with the preamble alignment column and look
//! different in every editor. Convention is 8-space indentation
//! matching rpm's preamble value column.
//!
//! **Scope limit**: shell-script sections (`%prep`/`%build`/`%install`/
//! `%check`/`%clean`/`%pre`/`%post`/triggers/…) get a free pass.
//! Real-world specs copy paragraphs out of Makefiles or write tabbed
//! heredocs there, and we don't want to drown users in style noise on
//! shell code where tabs are syntactically meaningful (e.g. recipe
//! continuations). Preamble / `%files` / `%description` still get the
//! alignment lint.

use rpm_spec::ast::{Section, Span, SpecFile, SpecItem};

use crate::diagnostic::{Applicability, Diagnostic, Edit, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM051",
    name: "tab-indent",
    description: "Lines indented with tabs make alignment fragile; use spaces instead.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Width of one tab when expanded. 8 matches rpm's preamble value
/// alignment column convention.
const TAB_WIDTH: usize = 8;

#[derive(Debug, Default)]
pub struct TabIndent {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl TabIndent {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for TabIndent {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Borrow `source` as `&str` so the per-line work can write into
        // `self.diagnostics` without aliasing the source buffer. The old
        // shape called `self.source.clone()` on every visit just to dodge
        // this borrow conflict — cloning the entire spec text per lint
        // run is wasteful on large specs.
        let Some(source) = self.source.as_deref() else {
            return;
        };
        let shell_ranges = collect_shell_section_ranges(spec);
        let mut line_start = 0usize;
        for (idx, byte) in source.bytes().enumerate() {
            if byte == b'\n' {
                if !is_inside_any(&shell_ranges, line_start) {
                    check_line(source, line_start, idx, &mut self.diagnostics);
                }
                line_start = idx + 1;
            }
        }
        // Trailing line without a terminating newline.
        if line_start < source.len() && !is_inside_any(&shell_ranges, line_start) {
            check_line(source, line_start, source.len(), &mut self.diagnostics);
        }
    }
}

/// Byte ranges of every shell-bodied section in `spec`. RPM051 stays
/// silent inside these ranges because tabs in shell code are routine
/// (Makefile recipes, heredocs, continuation lines).
fn collect_shell_section_ranges(spec: &SpecFile<Span>) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for item in &spec.items {
        collect_from_item(item, &mut ranges);
    }
    ranges
}

fn collect_from_item(item: &SpecItem<Span>, out: &mut Vec<(usize, usize)>) {
    match item {
        SpecItem::Section(boxed) => match boxed.as_ref() {
            // Build-script sections own raw shell bodies — tabs are
            // routine here (Makefile recipes, heredocs, continuation
            // lines), so RPM051 stays silent inside their spans.
            Section::BuildScript { data, .. }
            | Section::Verify { data, .. }
            | Section::Sepolicy { data, .. } => {
                out.push((data.start_byte, data.end_byte));
            }
            Section::Scriptlet(s) => {
                out.push((s.data.start_byte, s.data.end_byte));
            }
            Section::Trigger(t) => {
                out.push((t.data.start_byte, t.data.end_byte));
            }
            Section::FileTrigger(t) => {
                out.push((t.data.start_byte, t.data.end_byte));
            }
            // Non-shell sections: preamble/text/list bodies still get
            // the indentation lint, so they contribute no range. Listed
            // explicitly (no `_`) so a new variant in upstream
            // `rpm-spec` surfaces here as a compile-time audit — same
            // convention as `bcond_on_non_fedora.rs`.
            Section::Description { .. }
            | Section::Package { .. }
            | Section::Files { .. }
            | Section::Changelog { .. }
            | Section::SourceList { .. }
            | Section::PatchList { .. } => {}
            // `Section` is `#[non_exhaustive]`; future upstream variants
            // default to silent (no shell-range exemption) until a
            // human triages whether they own a shell body. The explicit
            // arms above force that triage by failing the match if a
            // currently-known variant is renamed.
            #[allow(unreachable_patterns)]
            _ => {}
        },
        SpecItem::Conditional(c) => {
            for branch in &c.branches {
                for nested in &branch.body {
                    collect_from_item(nested, out);
                }
            }
            if let Some(els) = &c.otherwise {
                for nested in els {
                    collect_from_item(nested, out);
                }
            }
        }
        // Other `SpecItem` variants (preamble entries, raw text, etc.)
        // carry no nested sections to inspect. `SpecItem` is also
        // `#[non_exhaustive]`, so the wildcard below is the deliberate
        // fallback; revisit if upstream adds another container variant
        // that could wrap shell-bodied sections.
        _ => {}
    }
}

fn is_inside_any(ranges: &[(usize, usize)], offset: usize) -> bool {
    ranges.iter().any(|&(s, e)| offset >= s && offset < e)
}

/// Free function so `visit_spec` can borrow `&self.source` as `&str`
/// while still mutating `self.diagnostics`. Keeping this off `Self`
/// avoids the `&self.source` / `&mut self` aliasing conflict that
/// previously forced a full `source.clone()` per visit.
fn check_line(source: &str, start: usize, end: usize, diagnostics: &mut Vec<Diagnostic>) {
    let line = &source[start..end];
    // Count leading tabs (we only flag tabs that appear in the
    // indentation; a stray `\t` mid-line is unusual but harmless).
    let leading_tabs = line.bytes().take_while(|b| *b == b'\t').count();
    if leading_tabs == 0 {
        return;
    }
    let replacement = " ".repeat(TAB_WIDTH * leading_tabs);
    let edit_span = Span::from_bytes(start, start + leading_tabs);
    let line_span = Span::from_bytes(start, end);
    diagnostics.push(
        Diagnostic::new(
            &METADATA,
            Severity::Warn,
            format!("line starts with {leading_tabs} tab(s); use spaces for stable alignment"),
            line_span,
        )
        .with_suggestion(Suggestion::new(
            format!("replace leading tabs with {TAB_WIDTH} spaces each"),
            vec![Edit::new(edit_span, replacement)],
            Applicability::MachineApplicable,
        )),
    );
}

impl Lint for TabIndent {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_source(&mut self, source: &str) {
        self.source = Some(source.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = TabIndent::new();
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_tab_indented_line() {
        let src = "Name: x\n\tRequires: foo\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM051");
        // Auto-fix should propose 8 spaces.
        let edits = &diags[0].suggestions[0].edits;
        assert_eq!(edits[0].replacement, " ".repeat(8));
    }

    #[test]
    fn silent_when_no_tabs() {
        let src = "Name: x\nRequires: foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_multiple_tabs() {
        let src = "Name: x\n\t\tindented\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].suggestions[0].edits[0].replacement, " ".repeat(16));
    }

    #[test]
    fn flags_tab_followed_by_text() {
        let src = "Name: x\n\tValue:\tfoo\n";
        // The mid-line `\t` between `Value:` and `foo` doesn't count —
        // only the leading tab does.
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        let leading = &diags[0].suggestions[0].edits[0];
        assert_eq!(leading.span.end_byte - leading.span.start_byte, 1);
    }
}
