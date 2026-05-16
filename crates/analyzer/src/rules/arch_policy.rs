//! RPM310 `arch-policy-contradiction` — flag arch-tag configurations
//! that contradict each other.
//!
//! Cases detected:
//!
//! 1. `BuildArch: noarch` combined with `ExclusiveArch:` or
//!    `ExcludeArch:` — restricting the arch set of a noarch package is
//!    meaningless.
//! 2. `ExclusiveArch:` and `ExcludeArch:` listing overlapping
//!    architectures — `ExcludeArch` cannot remove anything outside
//!    `ExclusiveArch`'s allowed set, so an overlap is dead policy at
//!    best, contradictory at worst.
//!
//! Conservative on macros: any arch token that isn't a plain literal
//! is ignored. The check still fires when at least one resolvable
//! arch is involved.

use std::collections::BTreeSet;

use rpm_spec::ast::{Span, SpecFile, Tag, TagValue, Text};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::collect_top_level_preamble;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM310",
    name: "arch-policy-contradiction",
    description: "`BuildArch: noarch` is combined with `ExclusiveArch`/`ExcludeArch`, or \
                  `ExclusiveArch` and `ExcludeArch` list overlapping architectures.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct ArchPolicyContradiction {
    diagnostics: Vec<Diagnostic>,
}

impl ArchPolicyContradiction {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for ArchPolicyContradiction {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let items = collect_top_level_preamble(spec);

        let mut buildarch_noarch: Option<Span> = None;
        let mut exclusive: Vec<(Span, BTreeSet<String>)> = Vec::new();
        let mut exclude: Vec<(Span, BTreeSet<String>)> = Vec::new();

        for item in &items {
            match &item.tag {
                Tag::BuildArch => {
                    if let TagValue::ArchList(list) = &item.value
                        && literal_archs(list).contains("noarch")
                    {
                        buildarch_noarch = Some(item.data);
                    }
                }
                Tag::ExclusiveArch => {
                    if let TagValue::ArchList(list) = &item.value {
                        exclusive.push((item.data, literal_archs(list)));
                    }
                }
                Tag::ExcludeArch => {
                    if let TagValue::ArchList(list) = &item.value {
                        exclude.push((item.data, literal_archs(list)));
                    }
                }
                _ => {}
            }
        }

        if let Some(noarch_span) = buildarch_noarch {
            for (span, _) in &exclusive {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "`ExclusiveArch:` set together with `BuildArch: noarch` — \
                         noarch packages run on any arch, so restricting is meaningless",
                        *span,
                    )
                    .with_label(noarch_span, "`BuildArch: noarch` declared here"),
                );
            }
            for (span, _) in &exclude {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "`ExcludeArch:` set together with `BuildArch: noarch` — \
                         noarch packages run on any arch, so excluding is meaningless",
                        *span,
                    )
                    .with_label(noarch_span, "`BuildArch: noarch` declared here"),
                );
            }
        }

        // ExclusiveArch ∩ ExcludeArch overlap check.
        for (ex_span, ex_set) in &exclusive {
            for (excl_span, excl_set) in &exclude {
                let overlap: Vec<&String> = ex_set.intersection(excl_set).collect();
                if !overlap.is_empty() {
                    let names: Vec<&str> = overlap.iter().map(|s| s.as_str()).collect();
                    self.diagnostics.push(
                        Diagnostic::new(
                            &METADATA,
                            Severity::Warn,
                            format!(
                                "`ExclusiveArch` and `ExcludeArch` both list `{}` — \
                                 either include or exclude, not both",
                                names.join(", "),
                            ),
                            *excl_span,
                        )
                        .with_label(*ex_span, "`ExclusiveArch:` declared here"),
                    );
                }
            }
        }
    }
}

fn literal_archs(list: &[Text]) -> BTreeSet<String> {
    list.iter()
        .filter_map(|t| t.literal_str())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

impl Lint for ArchPolicyContradiction {
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
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = ArchPolicyContradiction::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_noarch_with_exclusivearch() {
        let src = "Name: x\nBuildArch: noarch\nExclusiveArch: x86_64\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM310");
        assert!(diags[0].message.contains("noarch"));
    }

    #[test]
    fn flags_noarch_with_excludearch() {
        let src = "Name: x\nBuildArch: noarch\nExcludeArch: s390x\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("ExcludeArch"));
    }

    #[test]
    fn flags_exclusivearch_excludearch_overlap() {
        let src = "Name: x\nExclusiveArch: x86_64 aarch64\nExcludeArch: aarch64 s390x\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("aarch64"));
    }

    #[test]
    fn silent_for_distinct_arch_lists() {
        let src = "Name: x\nExclusiveArch: x86_64\nExcludeArch: s390x\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_buildarch_x86_64_alone() {
        let src = "Name: x\nBuildArch: x86_64\nExclusiveArch: x86_64\n";
        // No noarch hazard; ExclusiveArch alone is fine.
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_exclusivearch_only_macro_archs() {
        // `ExclusiveArch: %{ix86}` — we can't compare against literal,
        // so no flag.
        let src = "Name: x\nExclusiveArch: %{ix86}\nExcludeArch: s390x\n";
        assert!(run(src).is_empty());
    }
}
