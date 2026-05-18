//! RPM597 `guarded-dependency-constraint-subsumption` — flag a
//! conditional `Requires: foo` (any constraint) when the same package
//! already carries an unconditional `Requires: foo OP V`.
//!
//! The unconditional versioned requirement already pulls `foo` in;
//! the guarded copy is dead.
//!
//! Distinct from RPM450 (`guarded-item-already-unconditional`), which
//! requires full-text equality. RPM597 dominates by **name** only —
//! the constraint may differ.

use rpm_spec::ast::{
    DepExpr, PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem, TagValue,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::DepTagKey;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM597",
    name: "guarded-dependency-constraint-subsumption",
    description: "A guarded `Requires:` (or similar) is dominated by an unconditional, \
                  versioned requirement on the same name — drop the guarded copy.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// A guarded `Requires:` (or similar) is dominated by an unconditional, versioned requirement on the same name — drop the guarded copy.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct GuardedDependencySubsumption {
    diagnostics: Vec<Diagnostic>,
}

impl GuardedDependencySubsumption {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for GuardedDependencySubsumption {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Main package.
        let mut uncond: std::collections::HashSet<(DepTagKey, String, Option<String>)> =
            std::collections::HashSet::new();
        let mut cond: Vec<(DepTagKey, String, Option<String>, Span)> = Vec::new();
        scan_spec_items(&spec.items, 0, &mut uncond, &mut cond);
        self.emit_dominated(&uncond, &cond);

        // Subpackages.
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Package { content, .. } = boxed.as_ref() else {
                continue;
            };
            let mut sub_uncond: std::collections::HashSet<(DepTagKey, String, Option<String>)> =
                std::collections::HashSet::new();
            let mut sub_cond: Vec<(DepTagKey, String, Option<String>, Span)> = Vec::new();
            scan_preamble_content(content, 0, &mut sub_uncond, &mut sub_cond);
            self.emit_dominated(&sub_uncond, &sub_cond);
        }
    }
}

impl GuardedDependencySubsumption {
    fn emit_dominated(
        &mut self,
        uncond: &std::collections::HashSet<(DepTagKey, String, Option<String>)>,
        cond: &[(DepTagKey, String, Option<String>, Span)],
    ) {
        for (tag, name, arch, span) in cond {
            if !uncond.contains(&(*tag, name.clone(), arch.clone())) {
                continue;
            }
            let pretty = match arch {
                Some(a) => format!("{name}({a})"),
                None => name.clone(),
            };
            let label = tag.label();
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "guarded `{label}: {pretty}` is dominated by an unconditional \
                         `{label}:` for the same name; drop the guarded copy"
                    ),
                    *span,
                )
                .with_suggestion(Suggestion::new(
                    "remove the conditional entry; the unconditional requirement already pulls \
                     the package in",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
    }
}

fn scan_preamble_item(
    item: &PreambleItem<Span>,
    depth: u32,
    uncond: &mut std::collections::HashSet<(DepTagKey, String, Option<String>)>,
    cond: &mut Vec<(DepTagKey, String, Option<String>, Span)>,
) {
    let Some(tag) = DepTagKey::from_tag_weak_only(&item.tag) else {
        return;
    };
    let TagValue::Dep(expr) = &item.value else {
        return;
    };
    let DepExpr::Atom(atom) = expr else {
        return;
    };
    let Some(name) = atom.name.literal_str() else {
        return;
    };
    let name = name.trim().to_owned();
    if name.is_empty() {
        return;
    }
    let arch = atom
        .arch
        .as_ref()
        .and_then(|t| t.literal_str().map(|s| s.trim().to_owned()));
    // The "dominator" must be versioned (any constraint). An
    // unconditional ATOM-only entry is RPM450's territory.
    if depth == 0 {
        if atom.constraint.is_some() {
            uncond.insert((tag, name, arch));
        }
    } else {
        cond.push((tag, name, arch, item.data));
    }
}

fn scan_spec_items(
    items: &[SpecItem<Span>],
    depth: u32,
    uncond: &mut std::collections::HashSet<(DepTagKey, String, Option<String>)>,
    cond: &mut Vec<(DepTagKey, String, Option<String>, Span)>,
) {
    for it in items {
        match it {
            SpecItem::Preamble(p) => scan_preamble_item(p, depth, uncond, cond),
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    scan_spec_items(&branch.body, depth + 1, uncond, cond);
                }
                if let Some(els) = &c.otherwise {
                    scan_spec_items(els, depth + 1, uncond, cond);
                }
            }
            _ => {}
        }
    }
}

fn scan_preamble_content(
    items: &[PreambleContent<Span>],
    depth: u32,
    uncond: &mut std::collections::HashSet<(DepTagKey, String, Option<String>)>,
    cond: &mut Vec<(DepTagKey, String, Option<String>, Span)>,
) {
    for it in items {
        match it {
            PreambleContent::Item(p) => scan_preamble_item(p, depth, uncond, cond),
            PreambleContent::Conditional(c) => {
                for branch in &c.branches {
                    scan_preamble_content(&branch.body, depth + 1, uncond, cond);
                }
                if let Some(els) = &c.otherwise {
                    scan_preamble_content(els, depth + 1, uncond, cond);
                }
            }
            _ => {}
        }
    }
}

impl Lint for GuardedDependencySubsumption {
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
        run_lint::<GuardedDependencySubsumption>(src)
    }

    #[test]
    fn flags_guarded_when_unconditional_versioned() {
        let src = "Name: x\nRequires: foo >= 1.2\n%if A\nRequires: foo\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM597");
    }

    #[test]
    fn silent_when_unconditional_unversioned() {
        // No version on the unconditional one — RPM450's territory.
        let src = "Name: x\nRequires: foo\n%if A\nRequires: foo\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_no_unconditional_at_all() {
        let src = "Name: x\n%if A\nRequires: foo\n%endif\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_pkgconfig_arch_atoms_differ() {
        // `pkgconfig(libfoo)` parses with name="pkgconfig", arch="libfoo";
        // `pkgconfig(libbar)` parses with name="pkgconfig", arch="libbar".
        // They are distinct atoms and must not collide on the bare name.
        let src = "Name: x\n\
BuildRequires: pkgconfig(libfoo)\n\
%if A\n\
BuildRequires: pkgconfig(libbar) >= 1.0\n\
%endif\n";
        let diags = run(src);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn fires_in_subpackage() {
        let src = "Name: x\n\
%package devel\n\
Summary: dev\n\
Requires: foo >= 2\n\
%if A\nRequires: foo\n%endif\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
