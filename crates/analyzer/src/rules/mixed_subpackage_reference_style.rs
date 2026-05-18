//! RPM551 `mixed-subpackage-reference-style` — flag specs that
//! reference the same subpackage with both the relative (`foo`) and
//! absolute (`-n %{name}-foo`) form.
//!
//! Pick one style and stick with it; the mix is a tripping hazard on
//! `%package` rename refactors.

use std::collections::HashMap;

use rpm_spec::ast::{PackageName, Section, Span, SpecFile, SpecItem, SubpkgRef, Text};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::package_name;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM551",
    name: "mixed-subpackage-reference-style",
    description: "Subpackage referenced both as `foo` (relative) and `-n %{name}-foo` (absolute) \
                  — pick one style.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Subpackage referenced both as `foo` (relative) and `-n %{name}-foo` (absolute) — pick one style.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct MixedSubpackageReferenceStyle {
    diagnostics: Vec<Diagnostic>,
}

impl MixedSubpackageReferenceStyle {
    pub fn new() -> Self {
        Self::default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Style {
    Relative,
    Absolute,
}

impl<'ast> Visit<'ast> for MixedSubpackageReferenceStyle {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some(main) = package_name(spec).map(|s| s.to_owned()) else {
            return;
        };
        // canonical_name → list of (style, span) observations.
        let mut occurrences: HashMap<String, Vec<(Style, Span)>> = HashMap::new();
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            match boxed.as_ref() {
                Section::Package { name_arg, data, .. } => {
                    if let Some((canon, style)) = classify_package_name(name_arg, &main) {
                        occurrences.entry(canon).or_default().push((style, *data));
                    }
                }
                Section::Files { subpkg, data, .. } => {
                    if let Some(r) = subpkg.as_ref()
                        && let Some((canon, style)) = classify_subpkg(r, &main)
                    {
                        occurrences.entry(canon).or_default().push((style, *data));
                    }
                }
                Section::Description { subpkg, data, .. } => {
                    if let Some(r) = subpkg.as_ref()
                        && let Some((canon, style)) = classify_subpkg(r, &main)
                    {
                        occurrences.entry(canon).or_default().push((style, *data));
                    }
                }
                Section::Scriptlet(s) => {
                    if let Some(r) = s.subpkg.as_ref()
                        && let Some((canon, style)) = classify_subpkg(r, &main)
                    {
                        occurrences.entry(canon).or_default().push((style, s.data));
                    }
                }
                Section::Trigger(tr) => {
                    if let Some(r) = tr.subpkg.as_ref()
                        && let Some((canon, style)) = classify_subpkg(r, &main)
                    {
                        occurrences.entry(canon).or_default().push((style, tr.data));
                    }
                }
                _ => {}
            }
        }
        for (canon, obs) in occurrences {
            let mut styles = std::collections::BTreeSet::new();
            for (st, _) in &obs {
                styles.insert(*st);
            }
            if styles.len() < 2 {
                continue;
            }
            for (_, span) in obs {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        format!(
                            "subpackage `{canon}` is referenced with both relative and absolute \
                             header styles — pick one"
                        ),
                        span,
                    )
                    .with_suggestion(Suggestion::new(
                        "use either `foo` everywhere or `-n %{name}-foo` everywhere",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

fn classify_package_name(arg: &PackageName, main: &str) -> Option<(String, Style)> {
    match arg {
        PackageName::Relative(t) => {
            let suffix = t.literal_str()?.trim();
            Some((format!("{main}-{suffix}"), Style::Relative))
        }
        PackageName::Absolute(t) => {
            let abs = absolute_resolved(t, main)?;
            Some((abs, Style::Absolute))
        }
        _ => None,
    }
}

fn classify_subpkg(r: &SubpkgRef, main: &str) -> Option<(String, Style)> {
    match r {
        SubpkgRef::Relative(t) => {
            let suffix = t.literal_str()?.trim();
            Some((format!("{main}-{suffix}"), Style::Relative))
        }
        SubpkgRef::Absolute(t) => {
            let abs = absolute_resolved(t, main)?;
            Some((abs, Style::Absolute))
        }
        _ => None,
    }
}

/// Resolve an absolute-form text — accept either a literal `main-suffix`
/// or `%{name}-suffix`. Returns the full canonical name.
fn absolute_resolved(t: &Text, main: &str) -> Option<String> {
    use rpm_spec::ast::{ConditionalMacro, MacroKind, TextSegment};
    if let Some(lit) = t.literal_str() {
        return Some(lit.trim().to_owned());
    }
    if t.segments.len() == 2
        && let (TextSegment::Macro(m), TextSegment::Literal(s)) = (&t.segments[0], &t.segments[1])
        && m.name == "name"
        && matches!(m.kind, MacroKind::Plain | MacroKind::Braced)
        && matches!(m.conditional, ConditionalMacro::None)
        && m.args.is_empty()
        && m.with_value.is_none()
        && let Some(suffix) = s.trim_end().strip_prefix('-')
    {
        return Some(format!("{main}-{suffix}"));
    }
    None
}

impl Lint for MixedSubpackageReferenceStyle {
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
        run_lint::<MixedSubpackageReferenceStyle>(src)
    }

    #[test]
    fn flags_mixed_styles() {
        let src = "Name: acme\n\
%package devel\n\
Summary: dev\n\
%description devel\nbody\n\
%files -n acme-devel\n\
%{_bindir}/foo\n";
        let diags = run(src);
        assert!(!diags.is_empty(), "{diags:?}");
        assert!(diags.iter().all(|d| d.lint_id == "RPM551"));
    }

    #[test]
    fn silent_for_consistent_relative() {
        let src = "Name: acme\n\
%package devel\n\
Summary: dev\n\
%description devel\nbody\n\
%files devel\n\
%{_bindir}/foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_consistent_absolute() {
        let src = "Name: acme\n\
%package -n libacme\n\
Summary: lib\n\
%description -n libacme\nbody\n\
%files -n libacme\n\
%{_bindir}/foo\n";
        assert!(run(src).is_empty());
    }
}
