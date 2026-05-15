//! RPM031 `requires-equal-version` — `Requires: foo = 1.2-3` pins both
//! version and release exactly, which blocks even patch-level rebuilds
//! of the dependency. Almost always the author meant `>= 1.2`.
//!
//! `Requires: foo = 1.2` (no release) is fine — sometimes intentional
//! when version-and-release semantics aren't needed.

use rpm_spec::ast::{DepAtom, Span, Tag, VerOp};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM031",
    name: "requires-equal-version",
    description: "Requires with `=` operator pinned to a full version-release blocks compatible rebuilds.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Visit state. The two facts we need — *am I inside a `Requires:`
/// preamble item* and *what span should the diagnostic point at* —
/// are coupled: they're always set and unset together. An `enum`
/// captures that invariant in one place so it can't desync.
#[derive(Debug, Default)]
enum State {
    #[default]
    Outside,
    InsideRequires(Span),
}

#[derive(Debug, Default)]
pub struct RequiresEqualVersion {
    diagnostics: Vec<Diagnostic>,
    state: State,
}

impl RequiresEqualVersion {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for RequiresEqualVersion {
    fn visit_preamble(&mut self, node: &'ast rpm_spec::ast::PreambleItem<Span>) {
        let prev = std::mem::take(&mut self.state);
        if matches!(node.tag, Tag::Requires | Tag::BuildRequires) {
            self.state = State::InsideRequires(node.data);
        }
        visit::walk_preamble(self, node);
        self.state = prev;
    }

    fn visit_dep_atom(&mut self, atom: &'ast DepAtom) {
        let State::InsideRequires(span) = self.state else {
            return;
        };
        if let Some(c) = &atom.constraint
            && c.op == VerOp::Eq
            && c.evr.release.is_some()
            && let Some(name) = atom.name.literal_str()
        {
            self.diagnostics.push(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "`Requires: {name} = ...-release` pins exact release; \
                     consider `>=` instead"
                ),
                span,
            ));
        }
        visit::walk_dep_atom(self, atom);
    }
}

impl Lint for RequiresEqualVersion {
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
        let mut lint = RequiresEqualVersion::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_equal_with_release() {
        let diags = run("Name: x\nRequires: foo = 1.2-3\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].lint_id, "RPM031");
    }

    #[test]
    fn silent_for_equal_without_release() {
        assert!(run("Name: x\nRequires: foo = 1.2\n").is_empty());
    }

    #[test]
    fn silent_for_ge_operator() {
        assert!(run("Name: x\nRequires: foo >= 1.2-3\n").is_empty());
    }

    #[test]
    fn silent_for_provides_with_equal() {
        // Provides commonly uses `=` — out of scope.
        assert!(run("Name: x\nProvides: virt = 1.2-3\n").is_empty());
    }

    #[test]
    fn flags_buildrequires_equal() {
        let diags = run("Name: x\nBuildRequires: gcc = 11.2-1\n");
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn flags_atom_inside_rich_dep() {
        // Atoms nested in a boolean (rich) dep expression must be
        // visited via `walk_dep_atom` while `inside_requires` is still
        // true.
        let diags = run("Name: x\nRequires: (foo = 1.2-3 and bar)\n");
        assert_eq!(diags.len(), 1, "got {diags:?}");
        assert!(diags[0].message.contains("foo"));
    }
}
