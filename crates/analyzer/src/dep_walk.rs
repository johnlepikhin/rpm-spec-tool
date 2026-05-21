//! Shared helpers for walking dependency expressions and rendering
//! their text-bearing parts.
//!
//! Both [`crate::contract`] (BuildRequires contract verification) and
//! `matrix diff` walk every `DepExpr` extracted from a preamble item
//! and want the same answer: "give me the atom names, flattening
//! conjunctive rich-dep clauses (`And` / `Or` / `With`) plus the
//! `left` side of `Without`, and skipping conditional `If` / `Unless`
//! arms". Centralising the traversal avoids drift between the two
//! implementations — three copies of the rich-dep policy is one
//! refactor away from silently disagreeing.
//!
//! ## Tracing
//!
//! Skipped variants emit `tracing::debug!` events so an operator
//! running with `-v` can see why an atom is missing from the result:
//!
//! * `dep_walk::unknown_variant` — a `DepExpr` discriminant the
//!   walker doesn't recognise (the AST is `#[non_exhaustive]`).
//! * `dep_walk::rich_dep_skip` — a `BoolDep::If` / `Unless` arm
//!   whose conditional semantics we deliberately don't interpret.

use rpm_spec::ast::{BoolDep, DepExpr, MacroKind, Text, TextSegment};

/// Invoke `on_atom` for every classic `DepAtom` reachable from `dep`,
/// flattening `And`/`Or` rich-dep clauses recursively. `If`/`Unless`/
/// `Not` arms are skipped because their conditional semantics
/// ("does `foo` count when `cond` is false?") are too policy-laden
/// for static analysis to interpret without an explicit user choice.
///
/// Future AST variants (`DepExpr` is `#[non_exhaustive]`) are silently
/// skipped — a conservative default that avoids synthesising names
/// from a shape the walker can't understand. The trace event lets
/// observability catch the omission.
pub fn for_each_dep_atom<F>(dep: &DepExpr, mut on_atom: F)
where
    F: FnMut(&Text),
{
    visit_dep(dep, &mut on_atom);
}

fn visit_dep<F>(dep: &DepExpr, on_atom: &mut F)
where
    F: FnMut(&Text),
{
    match dep {
        DepExpr::Atom(atom) => on_atom(&atom.name),
        DepExpr::Rich(boxed) => visit_bool_dep(boxed, on_atom),
        _ => {
            tracing::debug!(
                target: "dep_walk::unknown_variant",
                "skipped unknown DepExpr variant; result may under-represent build deps"
            );
        }
    }
}

fn visit_bool_dep<F>(b: &BoolDep, on_atom: &mut F)
where
    F: FnMut(&Text),
{
    match b {
        // Conjunctive forms — every atom contributes. `With` is RPM's
        // "all-of" boolean and semantically identical to `And` for
        // atom collection (the difference is install-time semantics:
        // packages must be present simultaneously).
        BoolDep::And(items) | BoolDep::Or(items) | BoolDep::With(items) => {
            for d in items {
                visit_dep(d, on_atom);
            }
        }
        // `(left without right)` — `left` carries the required dep,
        // `right` is the negation half ("must not be satisfied by
        // right"). For atom-name collection only `left` matters; the
        // right-hand exclusion is a constraint we can't represent.
        BoolDep::Without { left, .. } => {
            visit_dep(left, on_atom);
        }
        // If/Unless arms are conditional — their semantics ("does
        // `foo` count when `cond` is false?") are too policy-laden
        // to interpret. Same conservative skip as for `%if` bodies.
        _ => {
            tracing::debug!(
                target: "dep_walk::rich_dep_skip",
                "skipped a rich If/Unless boolean dep; \
                 result may under-represent gated deps"
            );
        }
    }
}

/// Render a [`Text`] preserving macro surface form. `%name` becomes
/// `%name`; `%{name}`/`%{?name}`/`%(...)`/`%[...]` keep their braced
/// form. Used by every consumer that wants a stable key for a dep
/// name that may include macros (e.g. `%{name}-devel`).
///
/// Non-recognised `TextSegment` variants (the AST is
/// `#[non_exhaustive]`) contribute nothing — better to drop the
/// segment than synthesise a misleading surface form for callers
/// using the output as a hash key.
#[must_use]
pub fn render_text_with_macros(text: &Text) -> String {
    let mut out = String::new();
    for seg in &text.segments {
        match seg {
            TextSegment::Literal(s) => out.push_str(s),
            TextSegment::Macro(m) => match m.kind {
                MacroKind::Plain => {
                    out.push('%');
                    out.push_str(&m.name);
                }
                _ => {
                    out.push_str("%{");
                    out.push_str(&m.name);
                    out.push('}');
                }
            },
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    /// Parse a tiny spec containing `BuildRequires` line, return the
    /// first dep expression. Easier and more accurate than
    /// hand-assembling AST nodes for `#[non_exhaustive]` types.
    fn first_buildrequires_dep(spec_src: &str) -> rpm_spec::ast::DepExpr {
        use crate::visit::{self, Visit};
        use rpm_spec::ast::{Tag, TagValue};

        struct Grab(Option<rpm_spec::ast::DepExpr>);
        impl<'ast> Visit<'ast> for Grab {
            fn visit_preamble(
                &mut self,
                p: &'ast rpm_spec::ast::PreambleItem<rpm_spec::ast::Span>,
            ) {
                if self.0.is_none()
                    && matches!(p.tag, Tag::BuildRequires)
                    && let TagValue::Dep(d) = &p.value
                {
                    self.0 = Some(d.clone());
                }
                visit::walk_preamble(self, p);
            }
        }
        let parsed = parse(spec_src);
        let mut g = Grab(None);
        visit::walk_spec(&mut g, &parsed.spec);
        g.0.expect("spec must declare a BuildRequires")
    }

    const ATOM_SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: gcc

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    const AND_SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: (gcc and make)

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    const IF_SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: (gcc if systemd)

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    const MACRO_SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: %{name}-devel

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn for_each_atom_visits_single_atom() {
        let dep = first_buildrequires_dep(ATOM_SPEC);
        let mut seen: Vec<String> = Vec::new();
        for_each_dep_atom(&dep, |t| seen.push(render_text_with_macros(t)));
        assert_eq!(seen, vec!["gcc"]);
    }

    #[test]
    fn for_each_atom_flattens_and_clause() {
        let dep = first_buildrequires_dep(AND_SPEC);
        let mut seen: Vec<String> = Vec::new();
        for_each_dep_atom(&dep, |t| seen.push(render_text_with_macros(t)));
        seen.sort();
        assert_eq!(seen, vec!["gcc", "make"]);
    }

    #[test]
    fn for_each_atom_skips_rich_if() {
        // `(gcc if systemd)` — the If arm contributes nothing; the
        // walker emits a debug trace and moves on.
        let dep = first_buildrequires_dep(IF_SPEC);
        let mut seen: Vec<String> = Vec::new();
        for_each_dep_atom(&dep, |t| seen.push(render_text_with_macros(t)));
        assert!(
            seen.is_empty(),
            "rich If-dep must not contribute atoms; got {seen:?}"
        );
    }

    #[test]
    fn render_preserves_macro_surface_form() {
        let dep = first_buildrequires_dep(MACRO_SPEC);
        let mut seen: Vec<String> = Vec::new();
        for_each_dep_atom(&dep, |t| seen.push(render_text_with_macros(t)));
        // Macro-bearing name keeps the `%{name}` braced form.
        assert_eq!(seen, vec!["%{name}-devel"]);
    }

    /// Nested And: `(gcc and (make and meson))`. Catches a regression
    /// where the recursive `visit_dep`→`visit_bool_dep` chain stops
    /// after one level — every shared consumer would silently
    /// under-report nested conjunctive deps if this broke.
    #[test]
    fn for_each_atom_recurses_into_nested_and() {
        const NESTED_AND: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: (gcc and (make and meson))

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dep = first_buildrequires_dep(NESTED_AND);
        let mut seen: Vec<String> = Vec::new();
        for_each_dep_atom(&dep, |t| seen.push(render_text_with_macros(t)));
        seen.sort();
        assert_eq!(seen, vec!["gcc", "make", "meson"]);
    }

    /// Or clause: `(gcc or clang)`. Both atoms contribute — RPM's
    /// `or` semantics for dep resolution is "any one satisfies"
    /// but for atom *naming* both candidates are valid contract
    /// targets, mirroring how `And` is treated.
    #[test]
    fn for_each_atom_flattens_or_clause() {
        const OR_SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: (gcc or clang)

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dep = first_buildrequires_dep(OR_SPEC);
        let mut seen: Vec<String> = Vec::new();
        for_each_dep_atom(&dep, |t| seen.push(render_text_with_macros(t)));
        seen.sort();
        assert_eq!(seen, vec!["clang", "gcc"]);
    }

    /// `With`: `(foo with bar)` is a conjunctive form — must yield
    /// both. Before this fix, both atoms were silently dropped via
    /// the catch-all arm.
    #[test]
    fn for_each_atom_flattens_with_clause() {
        const WITH_SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: (foo with bar)

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dep = first_buildrequires_dep(WITH_SPEC);
        let mut seen: Vec<String> = Vec::new();
        for_each_dep_atom(&dep, |t| seen.push(render_text_with_macros(t)));
        seen.sort();
        assert_eq!(seen, vec!["bar", "foo"]);
    }

    /// `Without`: only `left` side carries the required dep.
    /// `right` is the negation half (semantically "left required
    /// EXCEPT right is installed"); for atom collection we record
    /// `left` only.
    #[test]
    fn for_each_atom_only_left_of_without() {
        const WITHOUT_SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: (foo without bar)

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dep = first_buildrequires_dep(WITHOUT_SPEC);
        let mut seen: Vec<String> = Vec::new();
        for_each_dep_atom(&dep, |t| seen.push(render_text_with_macros(t)));
        assert_eq!(
            seen,
            vec!["foo"],
            "Without must record only the left atom; got {seen:?}"
        );
    }
}
