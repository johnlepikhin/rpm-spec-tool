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
use rpm_spec_profile::Profile;

use crate::diagnostic::{Applicability, Diagnostic, Edit, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

// =====================================================================
// Metadata
// =====================================================================

pub static MAKE_BUILD_METADATA: LintMetadata = LintMetadata {
    id: "RPM120",
    name: "make-without-make-build",
    description: "Use `%make_build` instead of bare `make …` / `make %{?_smp_mflags} …` so that \
         parallelism and build flags follow distro convention.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

pub static MAKE_INSTALL_METADATA: LintMetadata = LintMetadata {
    id: "RPM121",
    name: "make-install-without-make-install",
    description: "Use `%make_install` instead of `make install …`; the macro sets `DESTDIR`, \
         `INSTALL` paths and other distro conventions automatically.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

pub static CONFIGURE_MACRO_METADATA: LintMetadata = LintMetadata {
    id: "RPM122",
    name: "configure-without-configure-macro",
    description: "Use `%configure` instead of plain `./configure` / `../configure`; the macro \
         supplies `--prefix`, `--libdir`, hardening flags and other distro defaults.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

// =====================================================================
// Rule structs
// =====================================================================

/// `macro_available` defaults to `true` so that, when no profile has
/// been applied (or the profile carries no `macros` data), the rule
/// behaves exactly as before — assume the suggested macro exists and
/// emit the warning. `Lint::set_profile` flips it to `false` only when
/// the active profile explicitly demonstrates the macro is missing.
#[derive(Debug)]
pub struct MakeWithoutMakeBuild {
    diagnostics: Vec<Diagnostic>,
    source: Option<std::sync::Arc<str>>,
    macro_available: bool,
}

impl Default for MakeWithoutMakeBuild {
    fn default() -> Self {
        Self {
            diagnostics: Vec::new(),
            source: None,
            macro_available: true,
        }
    }
}

impl MakeWithoutMakeBuild {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Use `%configure` instead of plain `./configure` / `../configure`; the macro supplies `--prefix`, `--libdir`, hardening flags and other distro defaults.
///
/// See [`CONFIGURE_MACRO_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug)]
pub struct MakeInstallWithoutMakeInstall {
    diagnostics: Vec<Diagnostic>,
    source: Option<std::sync::Arc<str>>,
    macro_available: bool,
}

impl Default for MakeInstallWithoutMakeInstall {
    fn default() -> Self {
        Self {
            diagnostics: Vec::new(),
            source: None,
            macro_available: true,
        }
    }
}

impl MakeInstallWithoutMakeInstall {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Use `%configure` instead of plain `./configure` / `../configure`; the macro supplies `--prefix`, `--libdir`, hardening flags and other distro defaults.
///
/// See [`CONFIGURE_MACRO_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug)]
pub struct ConfigureWithoutConfigureMacro {
    diagnostics: Vec<Diagnostic>,
    source: Option<std::sync::Arc<str>>,
    macro_available: bool,
}

impl Default for ConfigureWithoutConfigureMacro {
    fn default() -> Self {
        Self {
            diagnostics: Vec::new(),
            source: None,
            macro_available: true,
        }
    }
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
    ($rule:ident, $meta:ident, $kind:ident, $macro_name:literal) => {
        impl<'ast> Visit<'ast> for $rule {
            fn visit_section(&mut self, node: &'ast Section<Span>) {
                // Profile-gating: silent when the replacement macro
                // isn't defined in the active profile. Saves the user
                // from being told to switch to `%make_build` on a
                // distro that doesn't have it.
                if !self.macro_available {
                    visit::walk_section(self, node);
                    return;
                }
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
                if !self.macro_available {
                    visit::walk_scriptlet(self, node);
                    return;
                }
                if let Some(src) = self.source.as_deref() {
                    scan_section(src, node.data, RuleKind::$kind, &mut self.diagnostics);
                }
                visit::walk_scriptlet(self, node);
            }
            fn visit_trigger(&mut self, node: &'ast Trigger<Span>) {
                if !self.macro_available {
                    visit::walk_trigger(self, node);
                    return;
                }
                if let Some(src) = self.source.as_deref() {
                    scan_section(src, node.data, RuleKind::$kind, &mut self.diagnostics);
                }
                visit::walk_trigger(self, node);
            }
            fn visit_file_trigger(&mut self, node: &'ast FileTrigger<Span>) {
                if !self.macro_available {
                    visit::walk_file_trigger(self, node);
                    return;
                }
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
            fn set_source(&mut self, source: std::sync::Arc<str>) {
                self.source = Some(source);
            }
            fn set_profile(&mut self, profile: &Profile) {
                // The rule is meaningful only when the replacement
                // macro actually exists on the target distro. If the
                // profile has no `macros` data at all (default
                // `Profile::default()`) we fall back to "assume
                // present" so pre-profile pipelines keep working.
                self.macro_available =
                    profile.macros.is_empty() || profile.macros.get($macro_name).is_some();
            }
        }
    };
}

impl_rule!(
    MakeWithoutMakeBuild,
    MAKE_BUILD_METADATA,
    MakeBuild,
    "make_build"
);
impl_rule!(
    MakeInstallWithoutMakeInstall,
    MAKE_INSTALL_METADATA,
    MakeInstall,
    "make_install"
);
impl_rule!(
    ConfigureWithoutConfigureMacro,
    CONFIGURE_MACRO_METADATA,
    Configure,
    "configure"
);

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
    let Some(slice) = source.get(start..end) else {
        return;
    };

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
    // Span covers `make` + inter-word whitespace + `install`, taken
    // verbatim from source. `split_first_word` strips leading
    // whitespace from its second return, so `rest` already starts at
    // `second`'s position inside `body`. Hence position of `second`
    // in `body` = `body.len() - rest.len()`. Index arithmetic on
    // slice lengths — no raw pointer subtraction.
    let second_offset_in_body = body.len() - rest.len();
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
        lint.set_source(std::sync::Arc::from(src));
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

    // ---- Profile-aware gating ----

    /// Run a rule against a `Profile` that has the named macro registered.
    /// Mimics what `LintSession` does in production: `set_profile` runs
    /// before `set_source`.
    fn run_with_profile<L: Lint>(
        src: &str,
        mut lint: L,
        present_macros: &[&str],
    ) -> Vec<Diagnostic> {
        use rpm_spec_profile::{MacroEntry, Provenance};
        let outcome = parse(src);
        let mut profile = Profile::default();
        for name in present_macros {
            profile
                .macros
                .insert(*name, MacroEntry::literal("", Provenance::Override));
        }
        lint.set_profile(&profile);
        lint.set_source(std::sync::Arc::from(src));
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn rpm120_silent_when_make_build_macro_missing() {
        // Profile says `make_build` doesn't exist on this distro —
        // suggesting it would mislead.
        let src = "Name: x\n%build\nmake %{?_smp_mflags}\n";
        let diags = run_with_profile(src, MakeWithoutMakeBuild::new(), &["something_else"]);
        assert!(diags.is_empty(), "expected silence; got {diags:?}");
    }

    #[test]
    fn rpm120_fires_when_make_build_macro_present() {
        let src = "Name: x\n%build\nmake %{?_smp_mflags}\n";
        let diags = run_with_profile(src, MakeWithoutMakeBuild::new(), &["make_build"]);
        assert_eq!(diags.len(), 1, "got {diags:?}");
        assert_eq!(diags[0].lint_id, "RPM120");
    }

    #[test]
    fn rpm121_silent_when_make_install_macro_missing() {
        let src = "Name: x\n%install\nmake install DESTDIR=%{buildroot}\n";
        let diags = run_with_profile(src, MakeInstallWithoutMakeInstall::new(), &["make_build"]);
        assert!(diags.is_empty(), "got {diags:?}");
    }

    #[test]
    fn rpm122_silent_when_configure_macro_missing() {
        let src = "Name: x\n%build\n./configure --foo\n";
        let diags = run_with_profile(src, ConfigureWithoutConfigureMacro::new(), &["make_build"]);
        assert!(diags.is_empty(), "got {diags:?}");
    }
}
