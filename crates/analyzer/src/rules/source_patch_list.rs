//! RPM305 `source-patch-list-mixing` — flag specs that mix
//! `SourceN:` with `%sourcelist` (or `PatchN:` with `%patchlist`).
//!
//! Both forms describe the same set of files; mixing them is legal but
//! confusing and asks the reader to mentally merge two listings. RPM
//! documentation explicitly recommends sticking with one style per
//! spec.
//!
//! The diagnostic anchors at the `%sourcelist` / `%patchlist` section
//! header and points back at the first conflicting `SourceN:` /
//! `PatchN:` tag.

use rpm_spec::ast::{Section, Span, SpecFile, SpecItem, Tag};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::collect_top_level_preamble;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM305",
    name: "source-patch-list-mixing",
    description: "A spec mixes `SourceN:` tags with `%sourcelist` (or `PatchN:` with \
                  `%patchlist`). Use one form consistently.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

#[derive(Debug, Default)]
pub struct SourcePatchListMixing {
    diagnostics: Vec<Diagnostic>,
}

impl SourcePatchListMixing {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for SourcePatchListMixing {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let preamble = collect_top_level_preamble(spec);
        let first_source = preamble
            .iter()
            .find(|p| matches!(p.tag, Tag::Source(_)))
            .map(|p| p.data);
        let first_patch = preamble
            .iter()
            .find(|p| matches!(p.tag, Tag::Patch(_)))
            .map(|p| p.data);

        for item in &spec.items {
            let SpecItem::Section(boxed) = item else {
                continue;
            };
            match boxed.as_ref() {
                Section::SourceList { data, .. } => {
                    if let Some(src_span) = first_source {
                        self.diagnostics.push(
                            Diagnostic::new(
                                &METADATA,
                                Severity::Warn,
                                "spec uses both `SourceN:` tags and `%sourcelist`; pick one",
                                *data,
                            )
                            .with_label(src_span, "`SourceN:` tag declared here"),
                        );
                    }
                }
                Section::PatchList { data, .. } => {
                    if let Some(patch_span) = first_patch {
                        self.diagnostics.push(
                            Diagnostic::new(
                                &METADATA,
                                Severity::Warn,
                                "spec uses both `PatchN:` tags and `%patchlist`; pick one",
                                *data,
                            )
                            .with_label(patch_span, "`PatchN:` tag declared here"),
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

impl Lint for SourcePatchListMixing {
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
        let mut lint = SourcePatchListMixing::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_source_mixed_with_sourcelist() {
        let src = "Name: x\n\
Version: 1\n\
Source0: foo-1.tar.gz\n\
%sourcelist\n\
bar.tar.gz\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM305");
        assert!(diags[0].message.contains("Source"));
    }

    #[test]
    fn flags_patch_mixed_with_patchlist() {
        let src = "Name: x\n\
Version: 1\n\
Patch0: a.patch\n\
%patchlist\n\
b.patch\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Patch"));
    }

    #[test]
    fn silent_for_sourcelist_only() {
        let src = "Name: x\n\
Version: 1\n\
%sourcelist\n\
foo.tar.gz\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_source_only() {
        let src = "Name: x\n\
Version: 1\n\
Source0: foo-1.tar.gz\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_both_source_and_patch_mixing() {
        let src = "Name: x\n\
Version: 1\n\
Source0: foo.tar.gz\n\
Patch0: a.patch\n\
%sourcelist\n\
bar.tar.gz\n\
%patchlist\n\
b.patch\n";
        let diags = run(src);
        assert_eq!(diags.len(), 2);
    }
}
