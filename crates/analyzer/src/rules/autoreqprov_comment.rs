//! RPM308 `autoreqprov-disabled-without-comment` — flag explicit
//! disabling of RPM's automatic Requires/Provides generation without a
//! neighbouring comment explaining why.
//!
//! ```rpm
//! AutoReqProv: no                # ← flagged
//!
//! # Bundled libraries; auto-deps would pull in the system ones.
//! AutoReqProv: no                # ← silent
//! ```
//!
//! Auto-generated dependencies are how RPM keeps packages consistent
//! with library SONAME bumps and pkg-config requires. Disabling that
//! is sometimes correct (vendored deps, bootstrap packages) but is
//! almost always either a mistake or a maintenance trap. Requiring a
//! comment forces the maintainer to spell the reason out so reviewers
//! can decide.
//!
//! "Neighbouring" means a `Comment` AST node immediately before or
//! after the preamble item — same scope, no blank line in between
//! counts when the next/previous item is a comment.

use rpm_spec::ast::{
    Conditional, PreambleContent, PreambleItem, Section, Span, SpecFile, SpecItem, Tag, TagValue,
};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM308",
    name: "autoreqprov-disabled-without-comment",
    description: "`AutoReqProv` / `AutoReq` / `AutoProv` is set to `no` without a neighbouring \
                  comment. Disabling RPM's auto-dependency generation is unusual and almost \
                  always needs justification.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

#[derive(Debug, Default)]
pub struct AutoreqprovWithoutComment {
    diagnostics: Vec<Diagnostic>,
}

impl AutoreqprovWithoutComment {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for AutoreqprovWithoutComment {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        walk_top_items(&spec.items, &mut self.diagnostics);

        for item in &spec.items {
            walk_subpackages(item, &mut self.diagnostics);
        }
    }
}

fn walk_top_items(items: &[SpecItem<Span>], out: &mut Vec<Diagnostic>) {
    for (i, item) in items.iter().enumerate() {
        match item {
            SpecItem::Preamble(p) => {
                if let Some(label) = disable_label(p)
                    && !top_neighbour_is_comment(items, i)
                {
                    emit(p.data, label, out);
                }
            }
            SpecItem::Conditional(c) => walk_top_conditional(c, out),
            _ => {}
        }
    }
}

fn walk_top_conditional(cond: &Conditional<Span, SpecItem<Span>>, out: &mut Vec<Diagnostic>) {
    for branch in &cond.branches {
        walk_top_items(&branch.body, out);
    }
    if let Some(els) = &cond.otherwise {
        walk_top_items(els, out);
    }
}

fn walk_subpackages(item: &SpecItem<Span>, out: &mut Vec<Diagnostic>) {
    match item {
        SpecItem::Section(boxed) => {
            if let Section::Package { content, .. } = boxed.as_ref() {
                walk_preamble_contents(content, out);
            }
        }
        SpecItem::Conditional(c) => {
            for branch in &c.branches {
                for nested in &branch.body {
                    walk_subpackages(nested, out);
                }
            }
            if let Some(els) = &c.otherwise {
                for nested in els {
                    walk_subpackages(nested, out);
                }
            }
        }
        _ => {}
    }
}

fn walk_preamble_contents(items: &[PreambleContent<Span>], out: &mut Vec<Diagnostic>) {
    for (i, item) in items.iter().enumerate() {
        match item {
            PreambleContent::Item(p) => {
                if let Some(label) = disable_label(p)
                    && !preamble_neighbour_is_comment(items, i)
                {
                    emit(p.data, label, out);
                }
            }
            PreambleContent::Conditional(c) => {
                for branch in &c.branches {
                    walk_preamble_contents(&branch.body, out);
                }
                if let Some(els) = &c.otherwise {
                    walk_preamble_contents(els, out);
                }
            }
            _ => {}
        }
    }
}

fn disable_label(p: &PreambleItem<Span>) -> Option<&'static str> {
    let is_off = matches!(p.value, TagValue::Bool(false));
    if !is_off {
        return None;
    }
    match p.tag {
        Tag::AutoReqProv => Some("AutoReqProv"),
        Tag::AutoReq => Some("AutoReq"),
        Tag::AutoProv => Some("AutoProv"),
        _ => None,
    }
}

fn top_neighbour_is_comment(items: &[SpecItem<Span>], i: usize) -> bool {
    let before = i.checked_sub(1).map(|j| &items[j]);
    let after = items.get(i + 1);
    before.is_some_and(|x| matches!(x, SpecItem::Comment(_)))
        || after.is_some_and(|x| matches!(x, SpecItem::Comment(_)))
}

fn preamble_neighbour_is_comment(items: &[PreambleContent<Span>], i: usize) -> bool {
    let before = i.checked_sub(1).map(|j| &items[j]);
    let after = items.get(i + 1);
    before.is_some_and(|x| matches!(x, PreambleContent::Comment(_)))
        || after.is_some_and(|x| matches!(x, PreambleContent::Comment(_)))
}

fn emit(span: Span, label: &str, out: &mut Vec<Diagnostic>) {
    out.push(Diagnostic::new(
        &METADATA,
        Severity::Warn,
        format!(
            "`{label}: no` disables RPM's auto-dependency generation; add a neighbouring \
             comment that explains why"
        ),
        span,
    ));
}

impl Lint for AutoreqprovWithoutComment {
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
        let mut lint = AutoreqprovWithoutComment::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_autoreqprov_no_without_comment() {
        let src = "Name: x\nVersion: 1\nAutoReqProv: no\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM308");
        assert!(diags[0].message.contains("AutoReqProv"));
    }

    #[test]
    fn flags_autoreq_no_and_autoprov_no_separately() {
        let src = "Name: x\nAutoReq: no\nAutoProv: no\n";
        let diags = run(src);
        // Each is its own offence; AutoReq has neighbour AutoProv (not
        // a comment), AutoProv's neighbour AutoReq (not a comment).
        assert_eq!(diags.len(), 2);
    }

    #[test]
    fn silent_when_preceding_comment_present() {
        let src = "Name: x\n# bundled libs — auto-deps would pull system ones\nAutoReqProv: no\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_following_comment_present() {
        let src = "Name: x\nAutoReqProv: no\n# explanation continues on next line\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_autoreqprov_yes() {
        // We only care about `no` / `0`.
        let src = "Name: x\nAutoReqProv: yes\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_inside_subpackage() {
        let src = "Name: x\n\
%package devel\n\
Summary: dev\n\
AutoReqProv: no\n\
%description devel\nbody\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn silent_inside_subpackage_with_comment() {
        let src = "Name: x\n\
%package devel\n\
Summary: dev\n\
# vendored deps\n\
AutoReqProv: no\n\
%description devel\nbody\n";
        assert!(run(src).is_empty());
    }
}
