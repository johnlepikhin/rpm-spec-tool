//! RPM550 `prefer-relative-subpackage-name` — flag absolute subpackage
//! references that simply spell out `<main_name>-<suffix>`.
//!
//! `%package -n %{name}-devel` and `%package devel` produce the same
//! canonical name. The relative form is shorter and survives renames
//! of the main package; the `-n %{name}-foo` idiom is mechanical noise.
//!
//! Sections inspected: `%package`, `%description`, `%files`, every
//! scriptlet (`%pre`, `%post`, ...), and triggers.

use rpm_spec::ast::{PackageName, Section, Span, SpecFile, SpecItem, SubpkgRef, Text, TextSegment};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::package_name;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM550",
    name: "prefer-relative-subpackage-name",
    description: "Absolute subpackage reference (`-n <main>-<suffix>` or `-n %{name}-<suffix>`) \
                  can be replaced with the bare relative form.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Absolute subpackage reference (`-n <main>-<suffix>` or `-n %{name}-<suffix>`) can be replaced with the bare relative form.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct PreferRelativeSubpackageName {
    diagnostics: Vec<Diagnostic>,
}

impl PreferRelativeSubpackageName {
    pub fn new() -> Self {
        Self::default()
    }

    fn check(&mut self, label: &'static str, text: &Text, main: &str, anchor: Span) {
        let Some(suffix) = absolute_resolves_to_relative(text, main) else {
            return;
        };
        if suffix.is_empty() {
            return;
        }
        self.diagnostics.push(
            Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "{label} uses `-n` with an absolute name that re-spells `{main}-{suffix}`; \
                     drop `-n` and write `{suffix}`"
                ),
                anchor,
            )
            .with_suggestion(Suggestion::new(
                format!("rewrite header as `{label} {suffix}`"),
                Vec::new(),
                Applicability::MachineApplicable,
            )),
        );
    }
}

/// Returns `Some(suffix)` when `text` resolves to `<main>-<suffix>`
/// either as a pure literal (`acme-devel`) or as the canonical
/// `%{name}-suffix` form, where `%{name}` is a Plain/Braced macro
/// without arguments or conditional flags. `None` otherwise (cannot
/// statically prove equivalence).
fn absolute_resolves_to_relative<'a>(text: &'a Text, main: &str) -> Option<&'a str> {
    // Case 1: pure literal.
    if let Some(lit) = text.literal_str() {
        let trimmed = lit.trim();
        let stripped = trimmed.strip_prefix(main)?;
        let suffix = stripped.strip_prefix('-')?;
        return Some(suffix);
    }
    // Case 2: %{name} macro + literal `-suffix`.
    if text.segments.len() != 2 {
        return None;
    }
    let TextSegment::Macro(m) = &text.segments[0] else {
        return None;
    };
    if m.name != "name" {
        return None;
    }
    use rpm_spec::ast::{ConditionalMacro, MacroKind};
    if !matches!(m.kind, MacroKind::Plain | MacroKind::Braced) {
        return None;
    }
    if !matches!(m.conditional, ConditionalMacro::None) {
        return None;
    }
    if !m.args.is_empty() || m.with_value.is_some() {
        return None;
    }
    let TextSegment::Literal(lit) = &text.segments[1] else {
        return None;
    };
    let trimmed = lit.trim_end();
    let suffix = trimmed.strip_prefix('-')?;
    Some(suffix)
}

fn package_name_absolute_text(arg: &PackageName) -> Option<&Text> {
    match arg {
        PackageName::Absolute(t) => Some(t),
        PackageName::Relative(_) => None,
        _ => None,
    }
}

fn subpkg_absolute_text(r: &SubpkgRef) -> Option<&Text> {
    match r {
        SubpkgRef::Absolute(t) => Some(t),
        SubpkgRef::Relative(_) => None,
        _ => None,
    }
}

impl<'ast> Visit<'ast> for PreferRelativeSubpackageName {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some(main) = package_name(spec).map(|s| s.to_owned()) else {
            return; // can't compare without a literal main name
        };
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            match boxed.as_ref() {
                Section::Package { name_arg, data, .. } => {
                    if let Some(t) = package_name_absolute_text(name_arg) {
                        self.check("%package", t, &main, *data);
                    }
                }
                Section::Files { subpkg, data, .. } => {
                    if let Some(r) = subpkg.as_ref()
                        && let Some(t) = subpkg_absolute_text(r)
                    {
                        self.check("%files", t, &main, *data);
                    }
                }
                Section::Description { subpkg, data, .. } => {
                    if let Some(r) = subpkg.as_ref()
                        && let Some(t) = subpkg_absolute_text(r)
                    {
                        self.check("%description", t, &main, *data);
                    }
                }
                Section::Scriptlet(s) => {
                    if let Some(r) = s.subpkg.as_ref()
                        && let Some(t) = subpkg_absolute_text(r)
                    {
                        self.check("scriptlet", t, &main, s.data);
                    }
                }
                Section::Trigger(tr) => {
                    if let Some(r) = tr.subpkg.as_ref()
                        && let Some(t) = subpkg_absolute_text(r)
                    {
                        self.check("trigger", t, &main, tr.data);
                    }
                }
                _ => {}
            }
        }
    }
}

impl Lint for PreferRelativeSubpackageName {
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
        run_lint::<PreferRelativeSubpackageName>(src)
    }

    #[test]
    fn flags_package_n_main_dash_suffix_literal() {
        let src = "Name: acme\n\
%package -n acme-devel\n\
Summary: dev\n\
%description -n acme-devel\nbody\n";
        let diags = run(src);
        let pkg = diags.iter().find(|d| d.lint_id == "RPM550");
        assert!(pkg.is_some(), "{diags:?}");
    }

    #[test]
    fn flags_package_n_name_macro_form() {
        let src = "Name: acme\n\
%package -n %{name}-devel\n\
Summary: dev\n\
%description -n %{name}-devel\nbody\n";
        let diags = run(src);
        assert!(diags.iter().any(|d| d.lint_id == "RPM550"), "{diags:?}");
    }

    #[test]
    fn silent_when_already_relative() {
        let src = "Name: acme\n\
%package devel\n\
Summary: dev\n\
%description devel\nbody\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_truly_absolute_subpackage_name() {
        // `%package -n libfoo` doesn't share the main package's prefix.
        let src = "Name: acme\n\
%package -n libfoo\n\
Summary: lib\n\
%description -n libfoo\nbody\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_main_name_contains_macro() {
        // Main `Name: %{foo}` isn't a literal — can't safely compare.
        let src = "Name: %{foo}\n\
%package -n %{name}-devel\n\
Summary: dev\n\
%description -n %{name}-devel\nbody\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn fires_on_files_section_subpkg() {
        let src = "Name: acme\n\
%package devel\n\
Summary: dev\n\
%description devel\nbody\n\
%files -n acme-devel\n\
%{_bindir}/foo\n";
        let diags = run(src);
        assert!(
            diags.iter().any(|d| d.message.contains("%files")),
            "{diags:?}"
        );
    }
}
