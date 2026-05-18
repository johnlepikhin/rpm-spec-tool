//! RPM574 `preamble-tag-clustering` — flag preamble tags that aren't
//! grouped in the canonical packaging order (identity → sources →
//! build deps → runtime deps → ordering tags).
//!
//! Mixing the groups (e.g. a `BuildRequires:` between two `Source*:`
//! lines) slows the review eye and risks accidental duplication on
//! refactors.

use rpm_spec::ast::{Span, SpecFile, Tag};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::{DepTagKey, collect_top_level_preamble};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM574",
    name: "preamble-tag-clustering",
    description: "Preamble tags are not clustered in the canonical packaging order — group \
                  identity → sources → build deps → runtime deps.",
    default_severity: Severity::Allow,
    category: LintCategory::Style,
};

/// Preamble tags are not clustered in the canonical packaging order — group identity → sources → build deps → runtime deps.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct PreambleTagClustering {
    diagnostics: Vec<Diagnostic>,
}

impl PreambleTagClustering {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for PreambleTagClustering {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let items = collect_top_level_preamble(spec);
        let mut max_seen: u8 = 0;
        for p in items {
            let Some(w) = tag_weight(&p.tag) else {
                continue;
            };
            if w < max_seen {
                self.diagnostics.push(
                    Diagnostic::new(
                        &METADATA,
                        Severity::Warn,
                        "preamble tag out of canonical cluster order — group identity → sources \
                         → build deps → runtime deps",
                        p.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "move the tag to its canonical group up-front",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            } else {
                max_seen = w;
            }
        }
    }
}

fn tag_weight(t: &Tag) -> Option<u8> {
    // 1: identity / build_arch / metadata.
    // 2: Source / Patch / URL.
    // 3: BuildRequires / BuildConflicts (build-time deps).
    // 4: Runtime deps + ordering tags (`OrderWithRequires`).
    //
    // The dep-tag priorities (3, 4) come from
    // [`DepTagKey::cluster_priority`] so the table stays in sync with
    // the central dep-tag catalogue — earlier versions hand-rolled
    // both lists and risked drift.
    match t {
        Tag::Name
        | Tag::Version
        | Tag::Release
        | Tag::Epoch
        | Tag::Summary
        | Tag::License
        | Tag::Group
        | Tag::BuildArch
        | Tag::ExclusiveArch
        | Tag::ExcludeArch
        | Tag::AutoReqProv
        | Tag::AutoReq
        | Tag::AutoProv => Some(1),
        Tag::Source(_) | Tag::Patch(_) | Tag::URL | Tag::Icon => Some(2),
        _ => DepTagKey::from_tag(t).map(DepTagKey::cluster_priority),
    }
}

impl Lint for PreambleTagClustering {
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
        run_lint::<PreambleTagClustering>(src)
    }

    #[test]
    fn flags_buildrequires_between_sources() {
        let src = "Name: x\nVersion: 1\nRelease: 1\n\
Source0: a.tar\n\
BuildRequires: gcc\n\
Source1: b.tar\n";
        let diags = run(src);
        assert!(!diags.is_empty(), "{diags:?}");
        assert!(diags.iter().all(|d| d.lint_id == "RPM574"));
    }

    #[test]
    fn flags_url_after_runtime_deps() {
        let src = "Name: x\nVersion: 1\nRelease: 1\n\
Requires: foo\n\
URL: https://example.com/\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_canonical_clustering() {
        let src = "Name: x\nVersion: 1\nRelease: 1\nLicense: MIT\nSummary: s\n\
URL: https://example.com/\n\
Source0: a.tar\n\
Source1: b.tar\n\
BuildRequires: gcc\n\
Requires: foo\n";
        assert!(run(src).is_empty());
    }
}
