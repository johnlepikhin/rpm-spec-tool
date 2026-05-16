//! RPM031 `requires-equal-version` — `Requires: foo = 1.2-3` pins both
//! version and release exactly, which blocks even patch-level rebuilds
//! of the dependency. Almost always the author meant `>= 1.2`.
//!
//! `Requires: foo = 1.2` (no release) is fine — sometimes intentional
//! when version-and-release semantics aren't needed.

use rpm_spec::ast::{DepAtom, EVR, Span, Tag, Text, TextSegment, VerOp};

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
            && !is_lockstep_evr(&c.evr)
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

/// `true` when both the version and the release portions of `evr`
/// reference the source-package's own `%{version}` and `%{release}`
/// macros — the canonical "co-versioned subpackage" idiom
/// (`Requires: cpp = %{version}-%{release}`). Pinning to that exact
/// VR is correct: a subpackage compiled against `cpp-1.2-3` must
/// not run with `cpp-1.2-4`. Suggesting `>=` here would be wrong.
fn is_lockstep_evr(evr: &EVR) -> bool {
    let v_locked = text_references_macro(&evr.version, "version");
    let r_locked = evr
        .release
        .as_ref()
        .map(|r| text_references_macro(r, "release"))
        .unwrap_or(false);
    v_locked && r_locked
}

/// `true` when any segment of `t` is a [`TextSegment::Macro`] whose
/// name matches `wanted` (e.g. `"version"`, `"release"`). The
/// `MacroRef.name` is stored without the `%`/`{}` sigils and without
/// any `?`/`!?` conditional prefix (those live in `MacroRef.conditional`),
/// so a plain `==` comparison is enough.
fn text_references_macro(t: &Text, wanted: &str) -> bool {
    t.segments.iter().any(|s| match s {
        TextSegment::Macro(m) => m.name == wanted,
        _ => false,
    })
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

    #[test]
    fn silent_for_version_release_lockstep() {
        // The canonical co-versioned-subpackage idiom. `%{version}`
        // and `%{release}` both refer to the source package; pinning
        // a subpackage to its parent's exact VR is correct here.
        let src = "Name: x\nRequires: cpp = %{version}-%{release}\n";
        assert!(
            run(src).is_empty(),
            "lockstep pattern must not trigger RPM031"
        );
    }

    #[test]
    fn silent_for_lockstep_inside_rich_dep() {
        // The lockstep skip must also apply to atoms buried inside
        // rich-dep expressions.
        let src = "Name: x\nRequires: (cpp = %{version}-%{release} and bar)\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn flags_partial_macro_with_literal_release() {
        // `%{version}-3` is NOT lockstep — release is a hard-coded
        // literal that blocks compatible rebuilds.
        let src = "Name: x\nRequires: foo = %{version}-3\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn flags_partial_literal_with_macro_release() {
        // Mirror of the above — literal version + `%{release}` is
        // also not the canonical lockstep pattern.
        let src = "Name: x\nRequires: foo = 1.2-%{release}\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
    }
}
