//! RPM300 `duplicate-singleton-tag` — flag repeated singleton preamble
//! tags within one package scope.
//!
//! A "singleton" tag is one that RPM expects exactly once per package:
//! `Name`, `Version`, `Release`, `License`, `URL`, `Summary`, `Epoch`,
//! `BuildArch`, `AutoReqProv` / `AutoReq` / `AutoProv`. RPM resolves
//! duplicates with last-wins, so the earlier line is dead code — almost
//! always a copy-paste mistake.
//!
//! ### Conditional branches are alternatives, not duplicates
//!
//! ```rpm
//! %if 0%{?fedora}
//! Version: 1.2
//! %else
//! Version: 1.1
//! %endif
//! ```
//!
//! is not flagged: only one branch executes at build time. We track
//! occurrences per conditional branch independently, mirroring
//! [`super::macro_redefinition`].
//!
//! Subpackages have their own scope, so `Summary:` in `%package devel`
//! does not collide with `Summary:` in the main preamble.

use std::collections::HashMap;

use rpm_spec::ast::{
    Conditional, PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem, Tag,
};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

/// Singletons keyed by their canonical RPM tag label. The label
/// doubles as a stable map key (string comparison is the right
/// granularity for `Tag::Other`-style future extensions) and the
/// human-readable name shown in the diagnostic message.
type Seen = HashMap<&'static str, Span>;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM300",
    name: "duplicate-singleton-tag",
    description: "A singleton preamble tag (Name, Version, Release, License, URL, Summary, \
                  Epoch, BuildArch, AutoReq/AutoProv/AutoReqProv) appears more than once in the \
                  same package scope; RPM keeps the last value and the earlier line is dead code.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// A singleton preamble tag (Name, Version, Release, License, URL, Summary, Epoch, BuildArch, AutoReq/AutoProv/AutoReqProv) appears more than once in the same package scope; RPM keeps the last value and the earlier line is dead code.
///
/// See [`METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct DuplicateSingletonTag {
    diagnostics: Vec<Diagnostic>,
}

impl DuplicateSingletonTag {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for DuplicateSingletonTag {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        // Main package scope.
        let mut seen: Seen = HashMap::new();
        walk_items(&spec.items, &mut seen, &mut self.diagnostics);

        // Each top-level `%package` opens a fresh scope: subpackages are
        // independent. Subpackages nested inside an `%if` are also walked.
        for item in &spec.items {
            self.walk_subpackages(item);
        }
    }
}

impl DuplicateSingletonTag {
    fn walk_subpackages(&mut self, item: &SpecItem<Span>) {
        match item {
            SpecItem::Section(boxed) => {
                if let Section::Package { content, .. } = boxed.as_ref() {
                    let mut sub_seen: Seen = HashMap::new();
                    walk_preamble_contents(content, &mut sub_seen, &mut self.diagnostics);
                }
            }
            SpecItem::Conditional(c) => {
                for branch in &c.branches {
                    for nested in &branch.body {
                        self.walk_subpackages(nested);
                    }
                }
                if let Some(els) = &c.otherwise {
                    for nested in els {
                        self.walk_subpackages(nested);
                    }
                }
            }
            _ => {}
        }
    }
}

fn walk_items(items: &[SpecItem<Span>], seen: &mut Seen, out: &mut Vec<Diagnostic>) {
    for item in items {
        match item {
            SpecItem::Preamble(p) => check_preamble(p, seen, out),
            SpecItem::Conditional(c) => walk_conditional(c, out),
            _ => {}
        }
    }
}

fn walk_preamble_contents(
    items: &[PreambleContent<Span>],
    seen: &mut Seen,
    out: &mut Vec<Diagnostic>,
) {
    for item in items {
        match item {
            PreambleContent::Item(p) => check_preamble(p, seen, out),
            PreambleContent::Conditional(c) => walk_preamble_conditional(c, out),
            _ => {}
        }
    }
}

fn walk_conditional(cond: &Conditional<Span, SpecItem<Span>>, out: &mut Vec<Diagnostic>) {
    // Each branch is its own scope so that `%if cond Name: a %else Name: b %endif`
    // is not flagged — only one branch executes at build time. Same
    // trade-off as `macro_redefinition::walk_conditional`.
    for branch in &cond.branches {
        let mut branch_seen: Seen = HashMap::new();
        walk_items(&branch.body, &mut branch_seen, out);
    }
    if let Some(els) = &cond.otherwise {
        let mut branch_seen: Seen = HashMap::new();
        walk_items(els, &mut branch_seen, out);
    }
}

fn walk_preamble_conditional(
    cond: &Conditional<Span, PreambleContent<Span>>,
    out: &mut Vec<Diagnostic>,
) {
    for branch in &cond.branches {
        let mut branch_seen: Seen = HashMap::new();
        walk_preamble_contents(&branch.body, &mut branch_seen, out);
    }
    if let Some(els) = &cond.otherwise {
        let mut branch_seen: Seen = HashMap::new();
        walk_preamble_contents(els, &mut branch_seen, out);
    }
}

fn check_preamble(item: &PreambleItem<Span>, seen: &mut Seen, out: &mut Vec<Diagnostic>) {
    let Some(label) = singleton_label(&item.tag) else {
        return;
    };
    match seen.get(label) {
        None => {
            seen.insert(label, item.data);
        }
        Some(&first) => {
            out.push(
                Diagnostic::new(
                    &METADATA,
                    Severity::Warn,
                    format!(
                        "`{label}` tag appears more than once in this package scope; RPM keeps the last value",
                    ),
                    item.data,
                )
                .with_label(first, "previously defined here"),
            );
        }
    }
}

/// Map a `Tag` to its singleton canonical label, or `None` if this
/// rule does not police the tag. Adding a new singleton is a one-line
/// change here.
fn singleton_label(tag: &Tag) -> Option<&'static str> {
    Some(match tag {
        Tag::Name => "Name",
        Tag::Version => "Version",
        Tag::Release => "Release",
        Tag::License => "License",
        Tag::URL => "URL",
        Tag::Summary => "Summary",
        Tag::Epoch => "Epoch",
        Tag::BuildArch => "BuildArch",
        Tag::AutoReq => "AutoReq",
        Tag::AutoProv => "AutoProv",
        Tag::AutoReqProv => "AutoReqProv",
        _ => return None,
    })
}

impl Lint for DuplicateSingletonTag {
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
        run_lint::<DuplicateSingletonTag>(src)
    }

    #[test]
    fn flags_duplicate_name() {
        let diags = run("Name: foo\nName: bar\nVersion: 1\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM300");
        assert!(diags[0].message.contains("Name"));
        assert_eq!(diags[0].labels.len(), 1);
    }

    #[test]
    fn flags_duplicate_version_and_release() {
        let diags = run("Name: x\nVersion: 1\nVersion: 2\nRelease: 1\nRelease: 2\n");
        assert_eq!(diags.len(), 2);
    }

    #[test]
    fn flags_duplicate_url_summary_license_epoch() {
        let diags = run("Name: x\n\
             License: MIT\nLicense: GPL-2.0\n\
             URL: https://a\nURL: https://b\n\
             Summary: a\nSummary: b\n\
             Epoch: 0\nEpoch: 1\n");
        assert_eq!(diags.len(), 4);
    }

    #[test]
    fn flags_duplicate_buildarch_and_autoreqprov() {
        let diags = run("Name: x\n\
             BuildArch: noarch\nBuildArch: x86_64\n\
             AutoReqProv: no\nAutoReqProv: yes\n");
        assert_eq!(diags.len(), 2);
    }

    #[test]
    fn silent_for_distinct_singletons() {
        assert!(
            run("Name: x\nVersion: 1\nRelease: 1\nLicense: MIT\nURL: https://a\nSummary: s\n",)
                .is_empty()
        );
    }

    #[test]
    fn silent_for_if_else_alternatives() {
        let src = "%if 0%{?fedora}\n\
Name: foo-fedora\n\
%else\n\
Name: foo\n\
%endif\n\
Version: 1\n";
        assert!(run(src).is_empty(), "alternatives must not flag");
    }

    #[test]
    fn flags_duplicate_inside_same_branch() {
        let src = "%if 1\nName: a\nName: b\n%endif\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_for_subpackage_summary() {
        // `Summary:` in `%package devel` is a separate scope from the
        // main package's `Summary:` — not a duplicate.
        let src = "Name: x\nSummary: main\n\
%package devel\n\
Summary: devel files\n\
%description devel\nbody\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_duplicate_within_subpackage() {
        let src = "Name: x\n\
%package devel\n\
Summary: a\n\
Summary: b\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("Summary"));
    }

    #[test]
    fn silent_for_repeated_non_singleton_tags() {
        // `Requires:` is not a singleton — multiple lines accumulate.
        let src = "Name: x\nRequires: a\nRequires: b\nProvides: p1\nProvides: p2\n";
        assert!(run(src).is_empty());
    }
}
