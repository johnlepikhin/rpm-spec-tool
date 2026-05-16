//! Phase 12 — `Source` and `%description` style hygiene.
//!
//! ## Rules
//!
//! - **RPM125 `source-without-url`** — Fedora packaging guidelines
//!   require every `SourceN:` to be a URL where the tarball can be
//!   downloaded. A `Source0: gcc-%{version}.tar.xz` style entry
//!   carries the **filename** but loses the provenance.
//! - **RPM126 `description-leads-with-this-package`** — opt-in
//!   style nit. Fedora style guide discourages descriptions that
//!   open with `This package contains …` / `The X package is …` —
//!   prefer starting with the subject (`C++ compiler from the GNU
//!   Compiler Collection.`).
//!
//! Both walk only top-level sections / preamble; both bail
//! conservatively when the value is pure-macro (we can't reason
//! about what the macro expands to).

use rpm_spec::ast::{PreambleItem, Section, Span, Tag, TagValue, TextBody, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

// =====================================================================
// RPM125 — source-without-url
// =====================================================================

pub static SOURCE_WITHOUT_URL_METADATA: LintMetadata = LintMetadata {
    id: "RPM125",
    name: "source-without-url",
    description: "`SourceN:` should be a URL (http/https/ftp) where the upstream tarball can \
         be downloaded — Fedora packaging guideline.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct SourceWithoutUrl {
    diagnostics: Vec<Diagnostic>,
}

impl SourceWithoutUrl {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SourceWithoutUrl {
    fn visit_preamble(&mut self, node: &'ast PreambleItem<Span>) {
        if matches!(node.tag, Tag::Source(_))
            && let TagValue::Text(t) = &node.value
            && needs_url(t)
        {
            self.diagnostics.push(
                Diagnostic::new(
                    &SOURCE_WITHOUT_URL_METADATA,
                    Severity::Warn,
                    "`Source` value is a filename, not a download URL; \
                     Fedora policy expects an `http://` / `https://` / `ftp://` link",
                    node.data,
                )
                .with_suggestion(Suggestion::new(
                    "rewrite as the full upstream download URL (the filename is \
                     derived automatically via `basename`)",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
        visit::walk_preamble(self, node);
    }
}

impl Lint for SourceWithoutUrl {
    fn metadata(&self) -> &'static LintMetadata {
        &SOURCE_WITHOUT_URL_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

/// `true` when the value has at least one non-empty literal segment
/// **and** no segment contains a URL scheme marker (`://`). Pure-macro
/// values (`Source0: %{upstream_url}`) skip — we can't see through
/// the macro at lint time and would over-fire.
fn needs_url(t: &rpm_spec::ast::Text) -> bool {
    let mut has_literal = false;
    // Macros could carry the scheme; conservative skip is applied
    // via the `has_literal` test at the end.
    for seg in &t.segments {
        if let TextSegment::Literal(s) = seg {
            if s.contains("://") {
                return false;
            }
            if !s.trim().is_empty() {
                has_literal = true;
            }
        }
    }
    has_literal
}

// =====================================================================
// RPM126 — description-leads-with-this-package
// =====================================================================

/// Cap on the length of the subject between leading `The ` and
/// trailing ` package …` in RPM126's third pattern. A real subject
/// (the package's short name) fits in a handful of characters;
/// matching arbitrarily deep into the line risks false positives
/// on prose like "The above and below limits within this package …".
const MAX_SUBJECT_LEN: usize = 50;

pub static DESCRIPTION_LEADS_WITH_THIS_PACKAGE_METADATA: LintMetadata = LintMetadata {
    id: "RPM126",
    name: "description-leads-with-this-package",
    description: "`%description` body begins with `This package …` / `The X package …` — \
         Fedora style guide prefers leading with the subject of the description.",
    // Style preference; opt-in so consistency-focused projects can
    // enable it without surprising others.
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct DescriptionLeadsWithThisPackage {
    diagnostics: Vec<Diagnostic>,
}

impl DescriptionLeadsWithThisPackage {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for DescriptionLeadsWithThisPackage {
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        if let Section::Description { body, data, .. } = node
            && let Some(first) = first_meaningful_line(body)
            && leads_with_this_or_the_package(&first)
        {
            self.diagnostics.push(
                Diagnostic::new(
                    &DESCRIPTION_LEADS_WITH_THIS_PACKAGE_METADATA,
                    Severity::Warn,
                    "`%description` opens with a `This package …` / `The X package …` \
                     filler phrase — start with the subject directly",
                    *data,
                )
                .with_suggestion(Suggestion::new(
                    "rewrite the opening sentence to begin with the subject",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
        visit::walk_section(self, node);
    }
}

impl Lint for DescriptionLeadsWithThisPackage {
    fn metadata(&self) -> &'static LintMetadata {
        &DESCRIPTION_LEADS_WITH_THIS_PACKAGE_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

/// First non-blank line of the body as plain literal text. Returns
/// `None` for empty bodies or bodies whose first line begins with a
/// macro reference (we can't read past unknown expansions).
fn first_meaningful_line(body: &TextBody) -> Option<String> {
    for line in &body.lines {
        // Render only literal segments — if a macro precedes the
        // meaningful text, we abandon the check rather than guess.
        // Whitespace-only literal followed by a macro counts as
        // "leading macro" too: `   %{foo}` still hides the opening
        // word behind an unknown expansion.
        let mut buf = String::new();
        let mut leading_macro = false;
        for seg in &line.segments {
            match seg {
                TextSegment::Literal(s) => buf.push_str(s),
                TextSegment::Macro(_) => {
                    if buf.trim().is_empty() {
                        leading_macro = true;
                    }
                    break;
                }
                // `TextSegment` is `#[non_exhaustive]`; treat future
                // variants as opaque and stop scanning this line.
                _ => break,
            }
        }
        if leading_macro {
            return None;
        }
        if !buf.trim().is_empty() {
            return Some(buf);
        }
    }
    None
}

/// Case-insensitive pattern check for the discouraged opening
/// phrases. Three forms cover ~all real-world hits:
///
/// - `This package …`
/// - `This is the …`
/// - `The <subject> package …` (subject ≤ ~50 chars)
fn leads_with_this_or_the_package(line: &str) -> bool {
    let lower = line.trim_start().to_ascii_lowercase();
    if lower.starts_with("this package ") {
        return true;
    }
    if lower.starts_with("this is the ") {
        return true;
    }
    if let Some(rest) = lower.strip_prefix("the ")
        && let Some(idx) = rest.find(" package ")
        && idx > 0
        && idx < MAX_SUBJECT_LEN
    {
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ---- RPM125 ----

    #[test]
    fn rpm125_flags_filename_only() {
        let src = "Name: x\nVersion: 1\nSource0: foo-1.0.tar.gz\n";
        let diags = run(src, SourceWithoutUrl::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM125");
    }

    #[test]
    fn rpm125_silent_for_http_url() {
        let src = "Name: x\nSource0: https://example.org/foo-1.0.tar.gz\n";
        assert!(run(src, SourceWithoutUrl::new()).is_empty());
    }

    #[test]
    fn rpm125_silent_for_ftp_url() {
        let src = "Name: x\nSource0: ftp://example.org/foo.tar.gz\n";
        assert!(run(src, SourceWithoutUrl::new()).is_empty());
    }

    #[test]
    fn rpm125_flags_filename_with_macros() {
        // `gcc-%{version}-%{DATE}.tar.xz` — literal stretches exist,
        // none contain `://` → fire.
        let src = "Name: x\nSource0: gcc-%{version}-%{DATE}.tar.xz\n";
        let diags = run(src, SourceWithoutUrl::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm125_silent_for_pure_macro_value() {
        // Pure-macro value: we can't see what it expands to.
        // Conservative skip — no diagnostic.
        let src = "Name: x\nSource0: %{upstream_tarball}\n";
        assert!(run(src, SourceWithoutUrl::new()).is_empty());
    }

    #[test]
    fn rpm125_silent_for_url_with_macros() {
        let src = "Name: x\nSource3: https://gcc.gnu.org/pub/gcc/isl-%{isl_version}.tar.bz2\n";
        assert!(run(src, SourceWithoutUrl::new()).is_empty());
    }

    #[test]
    fn rpm125_flags_numbered_source() {
        let src = "Name: x\nSource17: extra-setup.in\n";
        let diags = run(src, SourceWithoutUrl::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    // ---- RPM126 ----

    #[test]
    fn rpm126_flags_this_package_opening() {
        let src = "\
Name: x

%description
This package contains the GNU C++ compiler.

%files
";
        let diags = run(src, DescriptionLeadsWithThisPackage::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM126");
    }

    #[test]
    fn rpm126_flags_the_x_package_opening() {
        let src = "\
Name: x

%description
The libstdc++ package contains the C++ standard library.

%files
";
        let diags = run(src, DescriptionLeadsWithThisPackage::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm126_flags_this_is_the_opening() {
        let src = "\
Name: x

%description
This is the GNU implementation of the standard C++ libraries.

%files
";
        let diags = run(src, DescriptionLeadsWithThisPackage::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm126_silent_for_subject_first_opening() {
        let src = "\
Name: x

%description
C++ compiler from the GNU Compiler Collection.

%files
";
        assert!(run(src, DescriptionLeadsWithThisPackage::new()).is_empty());
    }

    #[test]
    fn rpm126_silent_when_first_line_is_blank() {
        let src = "\
Name: x

%description


C++ compiler from GCC.

%files
";
        assert!(run(src, DescriptionLeadsWithThisPackage::new()).is_empty());
    }

    #[test]
    fn rpm126_silent_when_first_line_starts_with_macro() {
        // Leading macro reference — can't read past it; bail.
        let src = "\
Name: x

%description
%{summary} — extended description.

%files
";
        assert!(run(src, DescriptionLeadsWithThisPackage::new()).is_empty());
    }
}
