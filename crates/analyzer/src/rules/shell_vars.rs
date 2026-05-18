//! Phase 4 lints over shell-style build scripts.
//!
//! Two symmetric rules (RPM053 / RPM054) detect the legacy environment
//! variables `$RPM_BUILD_ROOT` and `$RPM_SOURCE_DIR` and recommend
//! macro replacements (`%{buildroot}` and `%{_sourcedir}`). Modern spec
//! convention prefers the macros; the env-var form predates them and
//! still works, but pollutes the build environment.
//!
//! Both rules fire on `Section::BuildScript.body`, scriptlets, triggers,
//! and the `Verify` body — anywhere `ShellBody` lines appear. Span
//! tracking falls back to the owning section's span because
//! `TextSegment` doesn't carry per-segment spans, so auto-fix is
//! emitted as a `Manual` suggestion rather than `MachineApplicable`.

use rpm_spec::ast::{FileTrigger, Scriptlet, Section, Span, SpecFile, Text, TextSegment, Trigger};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

/// Lint metadata for RPM053 `rpm-buildroot-shell-var`.
pub static BUILDROOT_METADATA: LintMetadata = LintMetadata {
    id: "RPM053",
    name: "rpm-buildroot-shell-var",
    description: "Use `%{buildroot}` instead of the legacy `$RPM_BUILD_ROOT` environment variable.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Lint metadata for RPM054 `rpm-source-dir-shell-var`.
pub static SOURCE_DIR_METADATA: LintMetadata = LintMetadata {
    id: "RPM054",
    name: "rpm-source-dir-shell-var",
    description: "Use `%{_sourcedir}` instead of the legacy `$RPM_SOURCE_DIR` environment variable.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

// =====================================================================
// RPM053 rpm-buildroot-shell-var
// =====================================================================

/// Use `%{_sourcedir}` instead of the legacy `$RPM_SOURCE_DIR` environment variable.
///
/// See [`SOURCE_DIR_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RpmBuildrootShellVar {
    diagnostics: Vec<Diagnostic>,
    current_shell_span: Option<Span>,
}

impl RpmBuildrootShellVar {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RpmBuildrootShellVar {
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        scan_section(
            self,
            node,
            "$RPM_BUILD_ROOT",
            "%{buildroot}",
            &BUILDROOT_METADATA,
        );
    }
    fn visit_scriptlet(&mut self, node: &'ast Scriptlet<Span>) {
        scan_scriptlet(
            self,
            node,
            "$RPM_BUILD_ROOT",
            "%{buildroot}",
            &BUILDROOT_METADATA,
        );
    }
    fn visit_trigger(&mut self, node: &'ast Trigger<Span>) {
        scan_trigger(
            self,
            node,
            "$RPM_BUILD_ROOT",
            "%{buildroot}",
            &BUILDROOT_METADATA,
        );
    }
    fn visit_file_trigger(&mut self, node: &'ast FileTrigger<Span>) {
        scan_file_trigger(
            self,
            node,
            "$RPM_BUILD_ROOT",
            "%{buildroot}",
            &BUILDROOT_METADATA,
        );
    }
}

impl Lint for RpmBuildrootShellVar {
    fn metadata(&self) -> &'static LintMetadata {
        &BUILDROOT_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM054 rpm-source-dir-shell-var
// =====================================================================

/// Use `%{_sourcedir}` instead of the legacy `$RPM_SOURCE_DIR` environment variable.
///
/// See [`SOURCE_DIR_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct RpmSourceDirShellVar {
    diagnostics: Vec<Diagnostic>,
    current_shell_span: Option<Span>,
}

impl RpmSourceDirShellVar {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RpmSourceDirShellVar {
    fn visit_section(&mut self, node: &'ast Section<Span>) {
        scan_section(
            self,
            node,
            "$RPM_SOURCE_DIR",
            "%{_sourcedir}",
            &SOURCE_DIR_METADATA,
        );
    }
    fn visit_scriptlet(&mut self, node: &'ast Scriptlet<Span>) {
        scan_scriptlet(
            self,
            node,
            "$RPM_SOURCE_DIR",
            "%{_sourcedir}",
            &SOURCE_DIR_METADATA,
        );
    }
    fn visit_trigger(&mut self, node: &'ast Trigger<Span>) {
        scan_trigger(
            self,
            node,
            "$RPM_SOURCE_DIR",
            "%{_sourcedir}",
            &SOURCE_DIR_METADATA,
        );
    }
    fn visit_file_trigger(&mut self, node: &'ast FileTrigger<Span>) {
        scan_file_trigger(
            self,
            node,
            "$RPM_SOURCE_DIR",
            "%{_sourcedir}",
            &SOURCE_DIR_METADATA,
        );
    }
}

impl Lint for RpmSourceDirShellVar {
    fn metadata(&self) -> &'static LintMetadata {
        &SOURCE_DIR_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// Generic shell-body walkers
// =====================================================================

/// Lints sharing the shell-vars scan pattern implement this trait so
/// the per-section walkers stay DRY.
trait ShellLint {
    fn current_shell_span(&mut self) -> &mut Option<Span>;
    fn diagnostics(&mut self) -> &mut Vec<Diagnostic>;
}

impl ShellLint for RpmBuildrootShellVar {
    fn current_shell_span(&mut self) -> &mut Option<Span> {
        &mut self.current_shell_span
    }
    fn diagnostics(&mut self) -> &mut Vec<Diagnostic> {
        &mut self.diagnostics
    }
}

impl ShellLint for RpmSourceDirShellVar {
    fn current_shell_span(&mut self) -> &mut Option<Span> {
        &mut self.current_shell_span
    }
    fn diagnostics(&mut self) -> &mut Vec<Diagnostic> {
        &mut self.diagnostics
    }
}

fn scan_section<L: ShellLint + for<'a> Visit<'a>>(
    lint: &mut L,
    node: &Section<Span>,
    needle: &str,
    replacement: &str,
    meta: &'static LintMetadata,
) {
    let span = match node {
        Section::BuildScript { data, body, .. } | Section::Verify { data, body, .. } => {
            *lint.current_shell_span() = Some(*data);
            for line in &body.lines {
                emit_if_match(line, *data, needle, replacement, meta, lint.diagnostics());
            }
            *data
        }
        Section::Sepolicy { data, body, .. } => {
            *lint.current_shell_span() = Some(*data);
            for line in &body.lines {
                emit_if_match(line, *data, needle, replacement, meta, lint.diagnostics());
            }
            *data
        }
        _ => {
            visit::walk_section(lint, node);
            *lint.current_shell_span() = None;
            return;
        }
    };
    let _ = span;
    *lint.current_shell_span() = None;
}

fn scan_scriptlet<L: ShellLint + for<'a> Visit<'a>>(
    lint: &mut L,
    node: &Scriptlet<Span>,
    needle: &str,
    replacement: &str,
    meta: &'static LintMetadata,
) {
    let span = node.data;
    *lint.current_shell_span() = Some(span);
    for line in &node.body.lines {
        emit_if_match(line, span, needle, replacement, meta, lint.diagnostics());
    }
    *lint.current_shell_span() = None;
}

fn scan_trigger<L: ShellLint + for<'a> Visit<'a>>(
    lint: &mut L,
    node: &Trigger<Span>,
    needle: &str,
    replacement: &str,
    meta: &'static LintMetadata,
) {
    let span = node.data;
    *lint.current_shell_span() = Some(span);
    for line in &node.body.lines {
        emit_if_match(line, span, needle, replacement, meta, lint.diagnostics());
    }
    *lint.current_shell_span() = None;
}

fn scan_file_trigger<L: ShellLint + for<'a> Visit<'a>>(
    lint: &mut L,
    node: &FileTrigger<Span>,
    needle: &str,
    replacement: &str,
    meta: &'static LintMetadata,
) {
    let span = node.data;
    *lint.current_shell_span() = Some(span);
    for line in &node.body.lines {
        emit_if_match(line, span, needle, replacement, meta, lint.diagnostics());
    }
    *lint.current_shell_span() = None;
}

/// Emit one diagnostic per `Text` line that contains `needle` inside any
/// literal segment. We don't have per-segment spans, so the diagnostic
/// anchors at the enclosing section/scriptlet body and the auto-fix is
/// flagged `Manual` (the user must edit the body themselves).
fn emit_if_match(
    line: &Text,
    body_span: Span,
    needle: &str,
    replacement: &str,
    meta: &'static LintMetadata,
    out: &mut Vec<Diagnostic>,
) {
    let hit = line.segments.iter().any(|seg| match seg {
        TextSegment::Literal(s) => s.contains(needle),
        _ => false,
    });
    if !hit {
        return;
    }
    out.push(
        Diagnostic::new(
            meta,
            Severity::Warn,
            format!("use `{replacement}` instead of `{needle}`"),
            body_span,
        )
        .with_suggestion(Suggestion::new(
            format!("replace `{needle}` with `{replacement}` in this body"),
            Vec::new(),
            Applicability::Manual,
        )),
    );
}

// `SpecFile` is referenced only via the blanket `Visit` impl; suppress
// the unused-import warning the compiler would otherwise raise.
// NOTE: kept as `#[allow]` (not `#[expect]`) because the const reference
// already counts as a use of `SpecFile`, so `dead_code` does not fire and
// `#[expect]` would emit `unfulfilled_lint_expectations`.
#[allow(dead_code)]
const _: Option<SpecFile<Span>> = None;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run_buildroot(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = RpmBuildrootShellVar::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }
    fn run_source_dir(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = RpmSourceDirShellVar::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn rpm053_flags_build_root_in_install() {
        let src = "Name: x\n%install\nmkdir -p $RPM_BUILD_ROOT/usr/bin\n";
        let diags = run_buildroot(src);
        assert!(!diags.is_empty(), "expected RPM053");
        assert_eq!(diags[0].lint_id, "RPM053");
    }

    #[test]
    fn rpm053_silent_when_macro_used() {
        let src = "Name: x\n%install\nmkdir -p %{buildroot}/usr/bin\n";
        assert!(run_buildroot(src).is_empty());
    }

    #[test]
    fn rpm053_flags_build_root_in_scriptlet() {
        let src = "Name: x\n%post\necho $RPM_BUILD_ROOT\n";
        assert!(!run_buildroot(src).is_empty());
    }

    #[test]
    fn rpm054_flags_source_dir_in_build() {
        let src = "Name: x\n%build\ncp $RPM_SOURCE_DIR/patch .\n";
        let diags = run_source_dir(src);
        assert!(!diags.is_empty());
        assert_eq!(diags[0].lint_id, "RPM054");
    }

    #[test]
    fn rpm054_silent_when_macro_used() {
        let src = "Name: x\n%build\ncp %{_sourcedir}/patch .\n";
        assert!(run_source_dir(src).is_empty());
    }
}
