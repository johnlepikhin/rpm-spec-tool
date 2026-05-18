//! RPM596 `dependency-constraint-subsumption` — flag an unversioned
//! `Requires: foo` when the same tag also carries a versioned
//! `Requires: foo OP V`.
//!
//! The versioned constraint already requires `foo` to be installed;
//! the unversioned line is dead weight. Reverse direction (versioned
//! subsumed by tighter versioned) is left to a richer EVR-aware
//! follow-up.

use std::collections::HashMap;

use rpm_spec::ast::{DepExpr, PreambleContent, Section, Span, SpecFile, SpecItem, TagValue};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{DepTagKey, collect_top_level_preamble, dep_atom_text};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM596",
    name: "dependency-constraint-subsumption",
    description: "Unversioned dep atom is subsumed by a versioned one for the same name; drop \
                  the unversioned line.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

// Note: Provides / Obsoletes are intentionally excluded by
// `DepTagKey::from_tag_no_provides_obsoletes` — RPM's unversioned-vs-versioned
// semantics are different on those tags. Conflicts is still in scope.

type GroupKey = (DepTagKey, String, Option<String>);
type GroupEntries = Vec<(bool, Span)>;

/// Unversioned dep atom is subsumed by a versioned one for the same name; drop the unversioned line.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct DependencyConstraintSubsumption {
    diagnostics: Vec<Diagnostic>,
}

impl DependencyConstraintSubsumption {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for DependencyConstraintSubsumption {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Process main package.
        let mut occurrences: HashMap<GroupKey, GroupEntries> = HashMap::new();
        for item in collect_top_level_preamble(spec) {
            let Some(key) = DepTagKey::from_tag_no_provides_obsoletes(&item.tag) else {
                continue;
            };
            let TagValue::Dep(expr) = &item.value else {
                continue;
            };
            let DepExpr::Atom(atom) = expr else {
                continue;
            };
            let Some(name) = atom.name.literal_str() else {
                continue;
            };
            let name = name.trim().to_owned();
            if name.is_empty() {
                continue;
            }
            let arch = atom
                .arch
                .as_ref()
                .and_then(|t| t.literal_str().map(|s| s.trim().to_owned()));
            let is_versioned = atom.constraint.is_some();
            occurrences
                .entry((key, name, arch))
                .or_default()
                .push((is_versioned, item.data));
        }
        for ((tag, name, arch), entries) in &occurrences {
            self.emit_for_group(*tag, name, arch.as_deref(), entries);
        }
        // Process subpackages.
        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            let Section::Package { content, .. } = boxed.as_ref() else {
                continue;
            };
            let mut sub_occ: HashMap<GroupKey, GroupEntries> = HashMap::new();
            for sub in content {
                let PreambleContent::Item(p) = sub else {
                    continue;
                };
                let Some(key) = DepTagKey::from_tag_no_provides_obsoletes(&p.tag) else {
                    continue;
                };
                let TagValue::Dep(expr) = &p.value else {
                    continue;
                };
                let DepExpr::Atom(atom) = expr else {
                    continue;
                };
                let Some(name) = atom.name.literal_str() else {
                    continue;
                };
                let name = name.trim().to_owned();
                if name.is_empty() {
                    continue;
                }
                let arch = atom
                    .arch
                    .as_ref()
                    .and_then(|t| t.literal_str().map(|s| s.trim().to_owned()));
                let is_versioned = atom.constraint.is_some();
                sub_occ
                    .entry((key, name, arch))
                    .or_default()
                    .push((is_versioned, p.data));
            }
            for ((tag, name, arch), entries) in &sub_occ {
                self.emit_for_group(*tag, name, arch.as_deref(), entries);
            }
        }
    }
}

impl DependencyConstraintSubsumption {
    fn emit_for_group(
        &mut self,
        tag: DepTagKey,
        name: &str,
        arch: Option<&str>,
        entries: &[(bool, Span)],
    ) {
        let has_versioned = entries.iter().any(|(v, _)| *v);
        if !has_versioned {
            return;
        }
        let label = tag.label();
        for (is_versioned, span) in entries {
            if *is_versioned {
                continue;
            }
            let pretty = match arch {
                Some(a) => format!("{name}({a})"),
                None => name.to_string(),
            };
            self.diagnostics.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`{label}: {pretty}` (no version) is subsumed by a versioned `{label}: \
                         {pretty} OP V` elsewhere — drop the unversioned line"
                    ),
                    *span,
                )
                .with_suggestion(Suggestion::new(
                    "delete the unversioned entry; the versioned line already requires the package",
                    Vec::new(),
                    Applicability::Manual,
                )),
            );
        }
        // Silence the unused `dep_atom_text` import if entries list
        // is empty — keep the import as a future hook.
        let _ = dep_atom_text;
    }
}

impl Lint for DependencyConstraintSubsumption {
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
        run_lint::<DependencyConstraintSubsumption>(src)
    }

    #[test]
    fn flags_unversioned_when_versioned_present() {
        let src = "Name: x\nRequires: foo\nRequires: foo >= 1.2\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM596");
    }

    #[test]
    fn silent_when_only_versioned() {
        let src = "Name: x\nRequires: foo >= 1.2\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_only_unversioned() {
        let src = "Name: x\nRequires: foo\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_across_different_tags() {
        let src = "Name: x\nRequires: foo\nBuildRequires: foo >= 1.2\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn fires_inside_subpackage() {
        let src = "Name: x\n%package devel\nSummary: dev\nRequires: bar\nRequires: bar = 1\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
