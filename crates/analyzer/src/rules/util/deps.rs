//! Dependency-expression walkers and canonicalisation.
//!
//! Rules that reason about `Requires:` / `BuildRequires:` / `Provides:` /
//! etc. atoms go through these helpers so that:
//! * atom enumeration handles rich (`(foo and bar)`) boolean deps
//!   uniformly, and
//! * structural equality has one definition that strips spans and
//!   bails conservatively on macros.

use std::fmt::Write as _;

use rpm_spec::ast::{
    BoolDep, DepAtom, DepExpr, PreambleItem, Span, SpecFile, Tag, TagValue, VerOp,
};

use super::preamble::collect_top_level_preamble;

/// Collect every [`DepAtom`] reachable from preamble items whose tag
/// satisfies `tag_matcher`. Walks into `DepExpr::Rich(BoolDep)` so
/// atoms inside boolean dependencies (`(foo and bar)`) are not missed.
pub(crate) fn collect_dep_atoms_in_items<'a, F>(
    items: &[&'a PreambleItem<Span>],
    tag_matcher: F,
) -> Vec<&'a DepAtom>
where
    F: Fn(&Tag) -> bool,
{
    let mut out = Vec::new();
    for item in items {
        if !tag_matcher(&item.tag) {
            continue;
        }
        if let TagValue::Dep(expr) = &item.value {
            collect_atoms(expr, &mut out);
        }
    }
    out
}

fn collect_atoms<'a>(expr: &'a DepExpr, out: &mut Vec<&'a DepAtom>) {
    match expr {
        DepExpr::Atom(a) => out.push(a),
        DepExpr::Rich(b) => collect_atoms_bool(b, out),
        _ => {}
    }
}

fn collect_atoms_bool<'a>(b: &'a BoolDep, out: &mut Vec<&'a DepAtom>) {
    match b {
        BoolDep::And(xs) | BoolDep::Or(xs) | BoolDep::With(xs) => {
            for x in xs {
                collect_atoms(x, out);
            }
        }
        BoolDep::If {
            cond,
            then,
            otherwise,
        }
        | BoolDep::Unless {
            cond,
            then,
            otherwise,
        } => {
            collect_atoms(cond, out);
            collect_atoms(then, out);
            if let Some(o) = otherwise {
                collect_atoms(o, out);
            }
        }
        BoolDep::Without { left, right } => {
            collect_atoms(left, out);
            collect_atoms(right, out);
        }
        _ => {}
    }
}

/// Canonical textual form of a [`DepAtom`] suitable as a dedup key
/// (e.g. `foo`, `foo(x86-64)`, `foo >= 1.2`, `pkgconfig(glib-2.0) = 1:2.78-1`).
///
/// Returns `None` when any part (name / arch / version / release) is
/// not a pure literal — macros defeat static equality. Internally a
/// fixed operator-table renders [`VerOp`], so equivalent constraints
/// produce identical strings.
pub(crate) fn dep_atom_text(atom: &DepAtom) -> Option<String> {
    let name = atom.name.literal_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let mut out = name.to_owned();
    if let Some(arch_text) = atom.arch.as_ref() {
        let arch = arch_text.literal_str()?.trim();
        out.push('(');
        out.push_str(arch);
        out.push(')');
    }
    if let Some(c) = atom.constraint.as_ref() {
        let op_str = match c.op {
            VerOp::Lt => "<",
            VerOp::Le => "<=",
            VerOp::Eq => "=",
            VerOp::Ne => "!=",
            VerOp::Ge => ">=",
            VerOp::Gt => ">",
            // `VerOp` is `#[non_exhaustive]`; future operators bail
            // out conservatively — we can't compare what we don't know.
            _ => return None,
        };
        let version = c.evr.version.literal_str()?.trim();
        out.push(' ');
        out.push_str(op_str);
        out.push(' ');
        if let Some(epoch) = c.evr.epoch {
            let _ = write!(out, "{epoch}:");
        }
        out.push_str(version);
        if let Some(rel) = c.evr.release.as_ref() {
            let r = rel.literal_str()?.trim();
            out.push('-');
            out.push_str(r);
        }
    }
    Some(out)
}

/// Structural equality between two [`DepExpr`] values.
///
/// Atom equality goes through [`dep_atom_text`] (so two atoms with
/// macros in any part compare unequal — conservative).
///
/// `And` / `Or` / `With` are compared *positionally*. Commutative
/// equality (`(foo and bar) ≡ (bar and foo)`) is left to higher-level
/// rules that want it — the canonical eq here is the strict baseline
/// every rule can rely on.
pub(crate) fn dep_expr_canonical_eq(a: &DepExpr, b: &DepExpr) -> bool {
    match (a, b) {
        (DepExpr::Atom(x), DepExpr::Atom(y)) => match (dep_atom_text(x), dep_atom_text(y)) {
            (Some(s1), Some(s2)) => s1 == s2,
            _ => false,
        },
        (DepExpr::Rich(x), DepExpr::Rich(y)) => bool_dep_canonical_eq(x, y),
        _ => false,
    }
}

fn bool_dep_canonical_eq(a: &BoolDep, b: &BoolDep) -> bool {
    match (a, b) {
        (BoolDep::And(xs), BoolDep::And(ys))
        | (BoolDep::Or(xs), BoolDep::Or(ys))
        | (BoolDep::With(xs), BoolDep::With(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys.iter())
                    .all(|(x, y)| dep_expr_canonical_eq(x, y))
        }
        (
            BoolDep::Without {
                left: l1,
                right: r1,
            },
            BoolDep::Without {
                left: l2,
                right: r2,
            },
        ) => dep_expr_canonical_eq(l1, l2) && dep_expr_canonical_eq(r1, r2),
        (
            BoolDep::If {
                cond: c1,
                then: t1,
                otherwise: o1,
            },
            BoolDep::If {
                cond: c2,
                then: t2,
                otherwise: o2,
            },
        )
        | (
            BoolDep::Unless {
                cond: c1,
                then: t1,
                otherwise: o1,
            },
            BoolDep::Unless {
                cond: c2,
                then: t2,
                otherwise: o2,
            },
        ) => {
            dep_expr_canonical_eq(c1, c2)
                && dep_expr_canonical_eq(t1, t2)
                && match (o1.as_deref(), o2.as_deref()) {
                    (None, None) => true,
                    (Some(x), Some(y)) => dep_expr_canonical_eq(x, y),
                    _ => false,
                }
        }
        // `BoolDep` is `#[non_exhaustive]`; mismatched or unknown
        // variants compare unequal.
        _ => false,
    }
}

/// Top-level convenience over [`collect_dep_atoms_in_items`]: collect
/// the resolved literal names of every dependency atom on a tag
/// matching `tag_matcher`. Atoms whose name is a macro (can't be
/// resolved literally) are silently skipped; whitespace around the
/// literal is trimmed.
///
/// Used by RPM324/RPM325/RPM328 — every "did the spec declare a BR/
/// Requires for this command?" rule lands here.
pub(crate) fn collect_top_level_dep_names<F>(
    spec: &SpecFile<Span>,
    tag_matcher: F,
) -> std::collections::BTreeSet<String>
where
    F: Fn(&Tag) -> bool,
{
    let items = collect_top_level_preamble(spec);
    collect_dep_atoms_in_items(&items, tag_matcher)
        .into_iter()
        .filter_map(|a| a.name.literal_str().map(|s| s.trim().to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::packages::iter_packages;
    use super::*;
    use crate::session::parse;

    fn p(src: &str) -> rpm_spec::ast::SpecFile<Span> {
        parse(src).spec
    }

    #[test]
    fn dep_atom_text_renders_name_arch_constraint() {
        // Build a spec with a known dep on `Requires:` and use the
        // parsed atom directly.
        let spec = p("Name: x\nRequires: kernel(x86-64) >= 9:5.0-1.fc40\n");
        let pkgs = iter_packages(&spec);
        let atoms = collect_dep_atoms_in_items(&pkgs[0].items, |t| matches!(t, Tag::Requires));
        assert_eq!(atoms.len(), 1);
        assert_eq!(
            dep_atom_text(atoms[0]).as_deref(),
            Some("kernel(x86-64) >= 9:5.0-1.fc40"),
        );
    }

    #[test]
    fn dep_atom_text_plain_name() {
        let spec = p("Name: x\nRequires: glibc\n");
        let pkgs = iter_packages(&spec);
        let atoms = collect_dep_atoms_in_items(&pkgs[0].items, |t| matches!(t, Tag::Requires));
        assert_eq!(dep_atom_text(atoms[0]).as_deref(), Some("glibc"));
    }

    #[test]
    fn dep_atom_text_none_for_macro_in_name() {
        let spec = p("Name: x\nRequires: %{thing}\n");
        let pkgs = iter_packages(&spec);
        let atoms = collect_dep_atoms_in_items(&pkgs[0].items, |t| matches!(t, Tag::Requires));
        assert!(
            dep_atom_text(atoms[0]).is_none(),
            "got {:?}",
            dep_atom_text(atoms[0])
        );
    }

    #[test]
    fn dep_expr_canonical_eq_equates_identical_atoms() {
        // Parse two specs with the same require atom; compare them.
        let spec_a = p("Name: x\nRequires: foo >= 1.0\n");
        let spec_b = p("Name: x\nRequires: foo >= 1.0\n");
        let atoms_a: Vec<_> = {
            let pkgs = iter_packages(&spec_a);
            pkgs[0]
                .items
                .iter()
                .filter_map(|p| match (&p.tag, &p.value) {
                    (Tag::Requires, TagValue::Dep(e)) => Some(e.clone()),
                    _ => None,
                })
                .collect()
        };
        let atoms_b: Vec<_> = {
            let pkgs = iter_packages(&spec_b);
            pkgs[0]
                .items
                .iter()
                .filter_map(|p| match (&p.tag, &p.value) {
                    (Tag::Requires, TagValue::Dep(e)) => Some(e.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(atoms_a.len(), 1);
        assert!(dep_expr_canonical_eq(&atoms_a[0], &atoms_b[0]));
    }

    #[test]
    fn dep_expr_canonical_eq_differs_on_constraint() {
        let spec = p("Name: x\nRequires: foo\nRequires: foo >= 1.0\n");
        let pkgs = iter_packages(&spec);
        let exprs: Vec<_> = pkgs[0]
            .items
            .iter()
            .filter_map(|p| match (&p.tag, &p.value) {
                (Tag::Requires, TagValue::Dep(e)) => Some(e.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(exprs.len(), 2);
        assert!(!dep_expr_canonical_eq(&exprs[0], &exprs[1]));
    }

    #[test]
    fn dep_expr_canonical_eq_equates_rich_and_with_positional() {
        let spec = p("Name: x\nRequires: (a and b and c)\nRequires: (a and b and c)\n");
        let pkgs = iter_packages(&spec);
        let exprs: Vec<_> = pkgs[0]
            .items
            .iter()
            .filter_map(|p| match (&p.tag, &p.value) {
                (Tag::Requires, TagValue::Dep(e)) => Some(e.clone()),
                _ => None,
            })
            .collect();
        assert!(dep_expr_canonical_eq(&exprs[0], &exprs[1]));
    }

    #[test]
    fn dep_expr_canonical_eq_rejects_reordered_operands() {
        // Positional comparison: `(a and b)` and `(b and a)` are NOT equal
        // under this helper — commutative equality is a separate concern.
        let spec = p("Name: x\nRequires: (a and b)\nRequires: (b and a)\n");
        let pkgs = iter_packages(&spec);
        let exprs: Vec<_> = pkgs[0]
            .items
            .iter()
            .filter_map(|p| match (&p.tag, &p.value) {
                (Tag::Requires, TagValue::Dep(e)) => Some(e.clone()),
                _ => None,
            })
            .collect();
        assert!(!dep_expr_canonical_eq(&exprs[0], &exprs[1]));
    }

    #[test]
    fn collect_dep_atoms_finds_plain_and_rich() {
        // `Requires: a, (b and c)` — three atoms total.
        let spec = p("Name: x\nRequires: a, (b and c)\n");
        let pkgs = iter_packages(&spec);
        let atoms = collect_dep_atoms_in_items(&pkgs[0].items, |t| matches!(t, Tag::Requires));
        let names: Vec<&str> = atoms.iter().filter_map(|a| a.name.literal_str()).collect();
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
        assert!(names.contains(&"c"));
    }
}
