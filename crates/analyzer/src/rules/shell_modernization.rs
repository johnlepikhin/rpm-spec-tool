//! Phase 10 — flag plain shell commands that have well-known
//! Fedora-style macro replacements.
//!
//! ## Rules
//!
//! - **RPM120 `make-without-make-build`** — `make %{?_smp_mflags}` /
//!   bare `make …` at the head of a `%build` (or other shell) line
//!   should be `%make_build`. Skips `make install` (handled by
//!   RPM121) and lines whose first word is anything else.
//! - **RPM121 `make-install-without-make-install`** — `make install …`
//!   at line head should be `%make_install`.
//! - **RPM122 `configure-without-configure-macro`** — `./configure`
//!   or `../configure` (with optional further `../`s) at line head
//!   should be `%configure`.
//!
//! All three scan the source slice covered by every shell-bearing
//! section (`Section::BuildScript`, `Verify`, `Sepolicy`,
//! `Scriptlet`, `Trigger`, `FileTrigger`) and inspect the first
//! word of each physical line. Suggestions carry `MachineApplicable`
//! edits that swap only the matched prefix (`make` / `make install` /
//! `../configure`); the rest of the line — flags, arguments — is
//! preserved.

use rpm_spec::ast::{FileTrigger, Scriptlet, Section, Span, Trigger};

use crate::diagnostic::{Applicability, Diagnostic, Edit, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

// =====================================================================
// Metadata
// =====================================================================

pub static MAKE_BUILD_METADATA: LintMetadata = LintMetadata {
    id: "RPM120",
    name: "make-without-make-build",
    description:
        "Use `%make_build` instead of bare `make …` / `make %{?_smp_mflags} …` so that \
         parallelism and build flags follow distro convention.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

pub static MAKE_INSTALL_METADATA: LintMetadata = LintMetadata {
    id: "RPM121",
    name: "make-install-without-make-install",
    description:
        "Use `%make_install` instead of `make install …`; the macro sets `DESTDIR`, \
         `INSTALL` paths and other distro conventions automatically.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

pub static CONFIGURE_MACRO_METADATA: LintMetadata = LintMetadata {
    id: "RPM122",
    name: "configure-without-configure-macro",
    description:
        "Use `%configure` instead of plain `./configure` / `../configure`; the macro \
         supplies `--prefix`, `--libdir`, hardening flags and other distro defaults.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

// =====================================================================
// Rule structs
// =====================================================================

#[derive(Debug, Default)]
pub struct MakeWithoutMakeBuild {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl MakeWithoutMakeBuild {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Default)]
pub struct MakeInstallWithoutMakeInstall {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl MakeInstallWithoutMakeInstall {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Default)]
pub struct ConfigureWithoutConfigureMacro {
    diagnostics: Vec<Diagnostic>,
    source: Option<String>,
}

impl ConfigureWithoutConfigureMacro {
    pub fn new() -> Self {
        Self::default()
    }
}

// =====================================================================
// Visit + Lint impls (one per rule, all wrappers around `scan_section`).
// =====================================================================

macro_rules! impl_rule {
    ($rule:ident, $meta:ident, $kind:ident) => {
        impl<'ast> Visit<'ast> for $rule {
            fn visit_section(&mut self, node: &'ast Section<Span>) {
                // Borrow `source` and `diagnostics` as disjoint fields
                // — NLL lets us read one immutably while pushing to
                // the other. Avoids a per-section full-source clone
                // (gcc.spec has dozens of shell-bearing sections; the
                // clone was multi-MB churn per lint run).
                if let Some(anchor) = shell_body_anchor(node)
                    && let Some(src) = self.source.as_deref()
                {
                    scan_section(src, anchor, RuleKind::$kind, &mut self.diagnostics);
                }
                visit::walk_section(self, node);
            }
            fn visit_scriptlet(&mut self, node: &'ast Scriptlet<Span>) {
                if let Some(src) = self.source.as_deref() {
                    scan_section(src, node.data, RuleKind::$kind, &mut self.diagnostics);
                }
                visit::walk_scriptlet(self, node);
            }
            fn visit_trigger(&mut self, node: &'ast Trigger<Span>) {
                if let Some(src) = self.source.as_deref() {
                    scan_section(src, node.data, RuleKind::$kind, &mut self.diagnostics);
                }
                visit::walk_trigger(self, node);
            }
            fn visit_file_trigger(&mut self, node: &'ast FileTrigger<Span>) {
                if let Some(src) = self.source.as_deref() {
                    scan_section(src, node.data, RuleKind::$kind, &mut self.diagnostics);
                }
                visit::walk_file_trigger(self, node);
            }
        }

        impl Lint for $rule {
            fn metadata(&self) -> &'static LintMetadata {
                &$meta
            }
            fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
                std::mem::take(&mut self.diagnostics)
            }
            fn set_source(&mut self, source: &str) {
                self.source = Some(source.to_owned());
            }
        }
    };
}

impl_rule!(MakeWithoutMakeBuild, MAKE_BUILD_METADATA, MakeBuild);
impl_rule!(MakeInstallWithoutMakeInstall, MAKE_INSTALL_METADATA, MakeInstall);
impl_rule!(ConfigureWithoutConfigureMacro, CONFIGURE_MACRO_METADATA, Configure);

// =====================================================================
// Pattern logic
// =====================================================================

#[derive(Debug, Clone, Copy)]
enum RuleKind {
    MakeBuild,
    MakeInstall,
    Configure,
}

fn shell_body_anchor(node: &Section<Span>) -> Option<Span> {
    match node {
        Section::BuildScript { data, .. }
        | Section::Verify { data, .. }
        | Section::Sepolicy { data, .. } => Some(*data),
        _ => None,
    }
}

/// Walk every physical line inside the section's source range and
/// dispatch to the rule-specific matcher.
fn scan_section(source: &str, anchor: Span, kind: RuleKind, out: &mut Vec<Diagnostic>) {
    let start = anchor.start_byte.min(source.len());
    let end = anchor.end_byte.min(source.len());
    let Some(slice) = source.get(start..end) else { return };

    let mut line_start_rel = 0usize;
    for line in slice.split_inclusive('\n') {
        let line_len = line.len();
        let trimmed_offset = leading_ws_bytes(line);
        let body = &line[trimmed_offset..];
        // Skip blank lines and `#` comments early.
        if body.is_empty() || body.starts_with('#') || body.starts_with('\n') {
            line_start_rel += line_len;
            continue;
        }
        let body_abs = start + line_start_rel + trimmed_offset;
        match kind {
            RuleKind::MakeBuild => match_make_build(body, body_abs, out),
            RuleKind::MakeInstall => match_make_install(body, body_abs, out),
            RuleKind::Configure => match_configure(body, body_abs, out),
        }
        line_start_rel += line_len;
    }
}

/// Length of the leading run of `[ \t]` bytes in `line`.
fn leading_ws_bytes(line: &str) -> usize {
    line.bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count()
}

/// First whitespace-delimited token + the rest of the line (with
/// leading whitespace stripped). Returns `None` for empty input.
fn split_first_word(s: &str) -> Option<(&str, &str)> {
    let end = s.find(|c: char| c.is_ascii_whitespace()).unwrap_or(s.len());
    if end == 0 {
        return None;
    }
    let first = &s[..end];
    let rest = s[end..].trim_start_matches([' ', '\t']);
    Some((first, rest))
}

// ---- RPM120 make-without-make-build ----

fn match_make_build(body: &str, body_abs: usize, out: &mut Vec<Diagnostic>) {
    let Some((first, rest)) = split_first_word(body) else {
        return;
    };
    if first != "make" {
        return;
    }
    // Defer to RPM121 when the line is actually `make install …`.
    if let Some((second, _)) = split_first_word(rest)
        && second == "install"
    {
        return;
    }
    let span = Span::from_bytes(body_abs, body_abs + first.len());
    out.push(
        Diagnostic::new(
            &MAKE_BUILD_METADATA,
            Severity::Warn,
            "use `%make_build` instead of bare `make`",
            span,
        )
        .with_suggestion(Suggestion::new(
            "replace `make` with `%make_build`",
            vec![Edit::new(span, "%make_build")],
            Applicability::MachineApplicable,
        )),
    );
}

// ---- RPM121 make-install-without-make-install ----

fn match_make_install(body: &str, body_abs: usize, out: &mut Vec<Diagnostic>) {
    let Some((first, rest)) = split_first_word(body) else {
        return;
    };
    if first != "make" {
        return;
    }
    let Some((second, _)) = split_first_word(rest) else {
        return;
    };
    if second != "install" {
        return;
    }
    // Span covers both words verbatim — counting the inter-word
    // whitespace exactly as it appears in source via pointer
    // arithmetic on the `&str` slices we already hold.
    let second_offset_in_body = (second.as_ptr() as usize) - (body.as_ptr() as usize);
    let span = Span::from_bytes(body_abs, body_abs + second_offset_in_body + second.len());
    out.push(
        Diagnostic::new(
            &MAKE_INSTALL_METADATA,
            Severity::Warn,
            "use `%make_install` instead of `make install`",
            span,
        )
        .with_suggestion(Suggestion::new(
            "replace `make install` with `%make_install`",
            vec![Edit::new(span, "%make_install")],
            Applicability::MachineApplicable,
        )),
    );
}

// ---- RPM122 configure-without-configure-macro ----

fn match_configure(body: &str, body_abs: usize, out: &mut Vec<Diagnostic>) {
    let Some((first, _)) = split_first_word(body) else {
        return;
    };
    // Accept ./configure and any number of leading ../ steps.
    if !is_relative_configure(first) {
        return;
    }
    let span = Span::from_bytes(body_abs, body_abs + first.len());
    out.push(
        Diagnostic::new(
            &CONFIGURE_MACRO_METADATA,
            Severity::Warn,
            format!("use `%configure` instead of plain `{first}`"),
            span,
        )
        .with_suggestion(Suggestion::new(
            format!("replace `{first}` with `%configure`"),
            vec![Edit::new(span, "%configure")],
            Applicability::MachineApplicable,
        )),
    );
}

/// `true` for `./configure`, `../configure`, `../../configure`, …
/// Strict ASCII match: rejects e.g. `././configure` or backslashes.
fn is_relative_configure(word: &str) -> bool {
    let mut rest = word;
    if let Some(after) = rest.strip_prefix("./") {
        rest = after;
    } else {
        // Must start with at least one `../`.
        let mut found = false;
        while let Some(after) = rest.strip_prefix("../") {
            rest = after;
            found = true;
        }
        if !found {
            return false;
        }
    }
    rest == "configure"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.set_source(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ---- RPM120 make-without-make-build ----

    #[test]
    fn rpm120_flags_bare_make_in_build() {
        let src = "Name: x\n%build\nmake %{?_smp_mflags}\n";
        let diags = run(src, MakeWithoutMakeBuild::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM120");
    }

    #[test]
    fn rpm120_silent_for_make_install() {
        // `make install` is RPM121 territory — RPM120 must defer.
        let src = "Name: x\n%install\nmake install DESTDIR=%{buildroot}\n";
        assert!(run(src, MakeWithoutMakeBuild::new()).is_empty());
    }

    #[test]
    fn rpm120_silent_for_make_underscore_word() {
        // `make_check` is a name-byte continuation of `make_`; first
        // word is `make_check`, not `make`. Already a modern macro form.
        let src = "Name: x\n%check\nmake_check\n";
        assert!(run(src, MakeWithoutMakeBuild::new()).is_empty());
    }

    #[test]
    fn rpm120_silent_for_indented_macro_use() {
        // `%make_build` and similar are macros, not `make`.
        let src = "Name: x\n%build\n%make_build\n";
        assert!(run(src, MakeWithoutMakeBuild::new()).is_empty());
    }

    #[test]
    fn rpm120_autofix_edit_swaps_just_make() {
        let src = "Name: x\n%build\nmake -j4 V=1\n";
        let diags = run(src, MakeWithoutMakeBuild::new());
        assert_eq!(diags.len(), 1);
        let edits = &diags[0].suggestions[0].edits;
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].replacement, "%make_build");
        let span = edits[0].span;
        assert_eq!(&src[span.start_byte..span.end_byte], "make");
    }

    // ---- RPM121 make-install-without-make-install ----

    #[test]
    fn rpm121_flags_make_install() {
        let src = "Name: x\n%install\nmake install DESTDIR=%{buildroot}\n";
        let diags = run(src, MakeInstallWithoutMakeInstall::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM121");
    }

    #[test]
    fn rpm121_autofix_swaps_both_words() {
        let src = "Name: x\n%install\nmake install prefix=/usr\n";
        let diags = run(src, MakeInstallWithoutMakeInstall::new());
        assert_eq!(diags.len(), 1);
        let edit = &diags[0].suggestions[0].edits[0];
        assert_eq!(edit.replacement, "%make_install");
        let s = edit.span;
        assert_eq!(&src[s.start_byte..s.end_byte], "make install");
    }

    #[test]
    fn rpm121_silent_for_make_alone() {
        let src = "Name: x\n%build\nmake\n";
        assert!(run(src, MakeInstallWithoutMakeInstall::new()).is_empty());
    }

    // ---- RPM122 configure-without-configure-macro ----

    #[test]
    fn rpm122_flags_dot_slash_configure() {
        let src = "Name: x\n%build\n./configure --prefix=%{_prefix}\n";
        let diags = run(src, ConfigureWithoutConfigureMacro::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM122");
    }

    #[test]
    fn rpm122_flags_dot_dot_slash_configure() {
        let src = "Name: x\n%build\n../configure --prefix=%{_prefix}\n";
        let diags = run(src, ConfigureWithoutConfigureMacro::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm122_flags_deeper_relative_configure() {
        let src = "Name: x\n%build\n../../isl-build/configure --prefix=/foo\n";
        // First word is `../../isl-build/configure` which is NOT
        // pure `../configure` — must NOT fire (sub-dir path).
        assert!(run(src, ConfigureWithoutConfigureMacro::new()).is_empty());
    }

    #[test]
    fn rpm122_silent_for_configure_macro() {
        let src = "Name: x\n%build\n%configure\n";
        assert!(run(src, ConfigureWithoutConfigureMacro::new()).is_empty());
    }

    #[test]
    fn rpm122_silent_for_absolute_configure_path() {
        let src = "Name: x\n%build\n/usr/src/foo/configure\n";
        assert!(run(src, ConfigureWithoutConfigureMacro::new()).is_empty());
    }

    #[test]
    fn rpm122_autofix_replaces_only_command_word() {
        let src = "Name: x\n%build\n./configure --foo\n";
        let diags = run(src, ConfigureWithoutConfigureMacro::new());
        assert_eq!(diags.len(), 1);
        let edit = &diags[0].suggestions[0].edits[0];
        assert_eq!(edit.replacement, "%configure");
        let s = edit.span;
        assert_eq!(&src[s.start_byte..s.end_byte], "./configure");
    }

    #[test]
    fn rpm120_silent_for_comment_lines() {
        let src = "Name: x\n%build\n# make foo\n";
        assert!(run(src, MakeWithoutMakeBuild::new()).is_empty());
    }
}
