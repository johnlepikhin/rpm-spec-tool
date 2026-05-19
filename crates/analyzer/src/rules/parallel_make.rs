//! RPM387 `j1-without-comment` — `make -j1` (or `-j 1`) without an
//! adjacent comment that explains why.
//!
//! Forcing serial builds is sometimes legitimate — a parallel-build
//! race upstream hasn't been fixed, or a non-reentrant code generator
//! ships with the source. But more often `-j1` is leftover debug from
//! an unrelated investigation, copied from another spec, or a
//! workaround for a race that's since been fixed upstream. Without a
//! comment, reviewers can't tell which.
//!
//! The rule asks for a neighbouring `#` comment on the line above (or,
//! exceptionally, the same line). It does not interpret the comment's
//! contents — just its presence is enough. This keeps the rule cheap
//! and the maintainer's intent visible.
//!
//! Only `%build` is inspected. A `-j1` in `%install` (often a real
//! `make install` race) follows the same convention but is rare in
//! practice; the same rule applies if it appears, since we walk every
//! build-script section.

use rpm_spec::ast::{ShellBody, Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::shell::{for_each_buildscript, tokenize_line};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM387",
    name: "j1-without-comment",
    description: "Build script forces serial make (`make -j1`) with no comment explaining why. \
                  `-j1` is often leftover debug or an obsolete workaround for an upstream race; \
                  add a comment so reviewers can tell intentional from accidental.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Lint state for RPM387 `j1-without-comment`. Collects diagnostics
/// across every build-script section walked by the visitor.
#[derive(Debug, Default)]
pub struct J1WithoutComment {
    diagnostics: Vec<Diagnostic>,
}

impl J1WithoutComment {
    /// Construct an empty lint instance with no buffered diagnostics.
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for J1WithoutComment {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for_each_buildscript(spec, |_kind, body, section_span| {
            scan_body(body, section_span, &mut self.diagnostics);
        });
    }
}

fn scan_body(body: &ShellBody<Span>, section_span: Span, out: &mut Vec<Diagnostic>) {
    // Emit at most one diag per build-script section to match the
    // Phase-23 convention (werror, optflags, network_in_build,
    // buildsystem_macros all dedup the same way). Three `make -j1`
    // lines in one section are one problem, not three.
    for (idx, line) in body.lines.iter().enumerate() {
        if !line_has_j1(line) {
            continue;
        }
        if neighbour_has_comment(&body.lines, idx) {
            continue;
        }
        out.push(Diagnostic::new(
            &METADATA,
            Severity::Warn,
            "build script forces `make -j1` with no comment explaining why; add a `#`-comment \
             on the preceding line so reviewers can tell intentional serial build from leftover \
             debug",
            section_span,
        ));
        return;
    }
}

/// `true` when `line` contains a `-j1` / `-j 1` token. Whitespace
/// inside the flag is tolerated (`-j1` and `-j 1` both flagged);
/// `-j2`, `-j%{?_smp_build_ncpus}`, etc. are ignored.
fn line_has_j1(line: &rpm_spec::ast::Text) -> bool {
    let tokens = tokenize_line(line);
    let mut iter = tokens.iter().peekable();
    while let Some(tok) = iter.next() {
        let Some(lit) = tok.literal_str() else {
            continue;
        };
        if lit == "-j1" {
            return true;
        }
        if lit == "-j"
            && let Some(next) = iter.peek()
            && let Some(nlit) = next.literal_str()
            && nlit == "1"
        {
            return true;
        }
    }
    false
}

/// `true` when the immediately preceding non-blank line is a `#`
/// comment (or `%dnl`), or when the `make -j1` line itself ends in a
/// trailing comment. Matches the openSUSE / Fedora convention of
/// putting the rationale right above the call.
fn neighbour_has_comment(lines: &[rpm_spec::ast::Text], idx: usize) -> bool {
    if let Some(lit) = lines[idx].literal_str()
        && let Some(hash) = lit.find('#')
    {
        // Tolerate trailing comment on the same line. Avoid matching
        // `#` inside quoted strings — cheap heuristic: the `#` must be
        // preceded by whitespace or be at the start.
        let before = &lit[..hash];
        if before.is_empty() || before.ends_with(char::is_whitespace) {
            return true;
        }
    }
    let mut j = idx;
    while j > 0 {
        j -= 1;
        let Some(lit) = lines[j].literal_str() else {
            return false;
        };
        let trimmed = lit.trim();
        if trimmed.is_empty() {
            continue;
        }
        return trimmed.starts_with('#');
    }
    false
}

impl Lint for J1WithoutComment {
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
        run_lint::<J1WithoutComment>(src)
    }

    #[test]
    fn flags_make_j1_without_comment() {
        let src = "Name: x\n%build\nmake -j1\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM387");
    }

    #[test]
    fn flags_make_j_space_1_without_comment() {
        let src = "Name: x\n%build\nmake -j 1\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_with_preceding_comment() {
        let src = "Name: x\n%build\n# upstream race in code generator, see #1234\nmake -j1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_with_trailing_comment_on_same_line() {
        let src = "Name: x\n%build\nmake -j1 # racy bison generator\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_with_comment_separated_by_blank() {
        // Blank between comment and command is tolerated.
        let src = "Name: x\n%build\n# upstream race\n\nmake -j1\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_normal_parallel_make() {
        let src = "Name: x\n%build\nmake %{?_smp_mflags}\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_higher_j_value() {
        let src = "Name: x\n%build\nmake -j4\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_j1_in_install_section() {
        // `-j1` in %install also fires — same convention.
        let src = "Name: x\n%install\nmake -j1 install\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_when_first_line_is_just_a_comment() {
        // Just to make sure we don't fire on something that doesn't
        // even mention -j1.
        let src = "Name: x\n%build\n# some comment\nmake\n";
        assert!(run(src).is_empty());
    }
}
