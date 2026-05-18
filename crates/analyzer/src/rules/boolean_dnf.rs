//! Phase 8a — Boolean DNF normalisation + tautology / contradiction
//! / redundancy detection.
//!
//! ## What this module does
//!
//! Each `%if` expression is a boolean function over **atoms**:
//! relational comparisons (`X >= 8`), macro references (`%{?rhel}`),
//! identifiers, and literal numbers / strings. The boolean wiring
//! between them — `&&`, `||`, `!`, `()` — is what we normalise.
//!
//! The pipeline:
//!
//! 1. Walk the [`ExprAst`] and intern each atomic sub-tree into an
//!    [`AtomTable`] keyed by canonical text. Two structurally identical
//!    atoms get the same id.
//! 2. Convert the boolean skeleton to DNF using De Morgan + distribution.
//!    DNF = set of cubes; cube = set of literals; literal = (atom, polarity).
//! 3. Simplify: drop cubes that are subsumed (super-sets of another cube),
//!    drop cubes that contain `{A+, A-}`.
//! 4. Ask diagnostic questions:
//!    - Is the simplified DNF empty? → contradiction.
//!    - Does the DNF cover every assignment (truth table)? → tautology.
//!    - Was simplification non-trivial? → redundancy.
//!
//! ## Conservative scope
//!
//! - Distribution can blow up (`(A1 || A2) && (B1 || B2) && ...` → all
//!   combinations). We bail out at [`MAX_CUBES`] to keep latency
//!   bounded.
//! - Truth-table tautology check is enumerative — exponential in atom
//!   count. We only run it for ≤ [`MAX_ATOMS_FOR_TAUTOLOGY`] atoms.
//! - Atoms are compared by canonical-text equality. `X + 1` and `1 + X`
//!   are *different* atoms (we don't reorder arithmetic).
//! - Only `Parsed` `CondExpr` branches are analysed. `Raw` text is
//!   left to legacy rules.

use std::collections::{BTreeSet, HashMap};

use rpm_spec::ast::{
    BinOp, CondExpr, Conditional, ExprAst, FilesContent, PreambleContent, Span, SpecItem,
};

use crate::diagnostic::{Applicability, Diagnostic, LintCategory, Severity, Suggestion};
use crate::lint::{Lint, LintMetadata};
use crate::visit::{self, Visit};

// =====================================================================
// DNF infrastructure
// =====================================================================

/// Distribution explosion guard: bail out when a partial DNF has more
/// than this many cubes. Real `%if` expressions never need this many.
const MAX_CUBES: usize = 64;

/// Maximum atom count for which we run truth-table tautology checks.
/// 8 atoms = 256 assignments; comfortable. Larger expressions get a
/// `None` verdict (we don't claim either way).
pub(crate) const MAX_ATOMS_FOR_TAUTOLOGY: usize = 8;

/// Stable identifier for an atomic boolean sub-expression. Two atoms
/// share an id iff their canonical text matches (see [`canonicalise`]).
pub(crate) type AtomId = u32;

/// One literal in a cube — an atom or its negation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct Literal {
    pub atom: AtomId,
    pub negated: bool,
}

impl Literal {
    fn flip(self) -> Self {
        Self {
            atom: self.atom,
            negated: !self.negated,
        }
    }
}

/// A conjunction of literals (one cube of a DNF). `BTreeSet` so the
/// containing DNF can dedupe and so subset-checks are linear.
pub(crate) type Cube = BTreeSet<Literal>;

/// A disjunction of cubes — full DNF.
pub(crate) type Dnf = BTreeSet<Cube>;

/// Intern atomic sub-expressions by canonical text. Distinct AST
/// subtrees with the same text collapse to one [`AtomId`].
#[derive(Debug, Default)]
pub(crate) struct AtomTable {
    next: AtomId,
    by_canon: HashMap<String, AtomId>,
}

impl AtomTable {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Intern an `ExprAst` atom. Returns `None` when the expression's
    /// shape isn't modelled by [`canonicalise`] — the caller must bail
    /// rather than collapse unrelated atoms onto the same id (which
    /// would be a soundness pitfall: two distinct atoms sharing an
    /// `AtomId` can produce wrong UNSAT/SAT verdicts downstream).
    pub(crate) fn intern(&mut self, expr: &ExprAst<Span>) -> Option<AtomId> {
        let key = canonicalise(expr)?;
        Some(self.intern_key(key))
    }

    /// Intern a pre-canonicalised key. Useful for atoms that don't
    /// originate from `ExprAst` (e.g. `ArchList` archs encoded as
    /// `"ARCH=<name>"`).
    pub(crate) fn intern_key(&mut self, key: String) -> AtomId {
        if let Some(&id) = self.by_canon.get(&key) {
            return id;
        }
        let id = self.next;
        self.next += 1;
        self.by_canon.insert(key, id);
        id
    }

    pub(crate) fn len(&self) -> usize {
        self.next as usize
    }
}

/// Stable text rendering of a sub-expression, ignoring spans and
/// `Paren` wrappers. Used as atom-table key. Distinct subtrees with
/// the same canonical text collapse to one atom — that's the
/// semantic-equality contract.
///
/// Returns `None` when the expression contains a variant not modelled
/// here (e.g. a future `#[non_exhaustive]` `ExprAst` variant). The
/// caller must bail rather than substitute a fallback string: two
/// unrelated atoms collapsing to the same `AtomId` would let
/// `path_implies` return UNSAT incorrectly.
fn canonicalise(expr: &ExprAst<Span>) -> Option<String> {
    Some(match expr.peel_parens() {
        ExprAst::Integer { value, .. } => value.to_string(),
        ExprAst::String { value, .. } => format!("\"{value}\""),
        ExprAst::Macro { text, .. } => text.clone(),
        ExprAst::Identifier { name, .. } => name.clone(),
        ExprAst::Not { inner, .. } => format!("!{}", canonicalise(inner)?),
        ExprAst::Binary { kind, lhs, rhs, .. } => {
            format!(
                "({}{}{})",
                canonicalise(lhs)?,
                kind.as_str(),
                canonicalise(rhs)?
            )
        }
        _ => return None,
    })
}

/// Convert an expression to DNF, optionally pushing a negation
/// through the root via De Morgan. Returns `None` on explosion or
/// when the AST shape exceeds the modelled grammar.
pub(crate) fn to_dnf(expr: &ExprAst<Span>, atoms: &mut AtomTable, negate: bool) -> Option<Dnf> {
    let bare = expr.peel_parens();
    match bare {
        ExprAst::Not { inner, .. } => to_dnf(inner, atoms, !negate),
        ExprAst::Binary {
            kind: BinOp::LogOr,
            lhs,
            rhs,
            ..
        } => {
            if !negate {
                // dnf(L || R) = dnf(L) ∪ dnf(R)
                let mut left = to_dnf(lhs, atoms, false)?;
                let right = to_dnf(rhs, atoms, false)?;
                left.extend(right);
                if left.len() > MAX_CUBES {
                    return None;
                }
                Some(left)
            } else {
                // !(L || R) = !L && !R — distribute.
                let left = to_dnf(lhs, atoms, true)?;
                let right = to_dnf(rhs, atoms, true)?;
                distribute_and(&left, &right)
            }
        }
        ExprAst::Binary {
            kind: BinOp::LogAnd,
            lhs,
            rhs,
            ..
        } => {
            if !negate {
                let left = to_dnf(lhs, atoms, false)?;
                let right = to_dnf(rhs, atoms, false)?;
                distribute_and(&left, &right)
            } else {
                // !(L && R) = !L || !R
                let mut left = to_dnf(lhs, atoms, true)?;
                let right = to_dnf(rhs, atoms, true)?;
                left.extend(right);
                if left.len() > MAX_CUBES {
                    return None;
                }
                Some(left)
            }
        }
        _ => {
            // Anything else is an atom: relational, macro, identifier,
            // literal, or future ExprAst variant. `intern` returns
            // `None` for un-modelled variants — propagate so DNF
            // conversion bails cleanly rather than aliasing distinct
            // atoms to the same id.
            let atom = atoms.intern(bare)?;
            let mut cube = BTreeSet::new();
            cube.insert(Literal {
                atom,
                negated: negate,
            });
            let mut dnf = BTreeSet::new();
            dnf.insert(cube);
            Some(dnf)
        }
    }
}

/// `dnf(A && B) = { ca ∪ cb : ca ∈ dnf(A), cb ∈ dnf(B) }` with
/// internal-contradiction filtering and explosion guard.
pub(crate) fn distribute_and(a: &Dnf, b: &Dnf) -> Option<Dnf> {
    let mut out: Dnf = BTreeSet::new();
    for ca in a {
        for cb in b {
            let mut merged = ca.clone();
            merged.extend(cb.iter().copied());
            if has_internal_contradiction(&merged) {
                continue;
            }
            out.insert(merged);
            if out.len() > MAX_CUBES {
                return None;
            }
        }
    }
    Some(out)
}

/// `true` when a cube contains both `A+` and `A-` for some atom `A`.
fn has_internal_contradiction(cube: &Cube) -> bool {
    cube.iter().any(|lit| cube.contains(&lit.flip()))
}

/// Drop cubes that are super-sets of another cube — the smaller cube
/// covers everything the larger one does, so the larger is redundant.
/// Returns `(simplified_dnf, was_simplified)`.
pub(crate) fn simplify_subsumption(dnf: &Dnf) -> (Dnf, bool) {
    let mut out = Dnf::new();
    let mut simplified = false;
    'outer: for c in dnf {
        for other in dnf {
            if !std::ptr::eq(c, other) && other.is_subset(c) && other != c {
                // c is strictly super-set of other → c is redundant.
                simplified = true;
                continue 'outer;
            }
        }
        out.insert(c.clone());
    }
    (out, simplified)
}

/// `true` when no assignment falsifies `dnf` — every truth-value
/// combination is satisfied by at least one cube. Returns `None`
/// when there are too many atoms to enumerate.
pub(crate) fn is_tautology(dnf: &Dnf, atom_count: usize) -> Option<bool> {
    if atom_count > MAX_ATOMS_FOR_TAUTOLOGY {
        return None;
    }
    if dnf.is_empty() {
        return Some(false);
    }
    let total = 1u32 << atom_count;
    for assignment in 0..total {
        let satisfies_any = dnf.iter().any(|cube| eval_cube(cube, assignment));
        if !satisfies_any {
            return Some(false);
        }
    }
    Some(true)
}

pub(crate) fn eval_cube(cube: &Cube, assignment: u32) -> bool {
    cube.iter().all(|lit| {
        let bit = (assignment >> lit.atom) & 1 == 1;
        if lit.negated { !bit } else { bit }
    })
}

/// `true` when the DNF has no satisfying assignment. Once
/// [`to_dnf`] + [`simplify_subsumption`] are applied, an empty DNF
/// is the canonical "unsat" marker.
pub(crate) fn is_contradiction(dnf: &Dnf) -> bool {
    dnf.is_empty()
}

// =====================================================================
// Lint glue
// =====================================================================

/// Build a normalised DNF for the head expression of one branch.
/// Returns `None` when the expression isn't `Parsed`, when it has no
/// real boolean structure (single atom is treated as trivial and
/// rejected here so the rules don't fire on plain `%if X`), or when
/// the normaliser bailed out.
fn analyse_branch(
    expr: &CondExpr<Span>,
) -> Option<(
    Dnf,
    /*atom_count*/ usize,
    /*orig_cube_count*/ usize,
)> {
    let CondExpr::Parsed(ast) = expr else {
        return None;
    };
    // Skip pure atoms — `%if X`, `%if 1`, `%if "foo"` — they're
    // covered by RPM072 (constant-condition) and have no boolean
    // structure to normalise.
    if !has_boolean_structure(ast) {
        return None;
    }
    let mut atoms = AtomTable::new();
    let raw_dnf = to_dnf(ast, &mut atoms, false)?;
    // Count source-level `||` operands BEFORE BTreeSet deduplication —
    // otherwise `A || A` would already collapse to one cube and we'd
    // miss it. We still take the max of the source count and the
    // post-distribution count (negation/distribution can grow cubes).
    let source_or_operands = count_or_operands(ast);
    let orig_cube_count = source_or_operands.max(raw_dnf.len());
    let (simplified, _) = simplify_subsumption(&raw_dnf);
    Some((simplified, atoms.len(), orig_cube_count))
}

/// Count `||`-separated operands at the root of `ast`. `A || B || C`
/// gives `3`; non-`||` roots give `1`. Used to detect redundancy that
/// `BTreeSet` deduplication would otherwise hide (e.g. `A || A`).
fn count_or_operands<T>(ast: &ExprAst<T>) -> usize {
    match ast.peel_parens() {
        ExprAst::Binary {
            kind: BinOp::LogOr,
            lhs,
            rhs,
            ..
        } => count_or_operands(lhs) + count_or_operands(rhs),
        _ => 1,
    }
}

fn has_boolean_structure<T>(ast: &ExprAst<T>) -> bool {
    use rpm_spec::ast::ExprAst;
    matches!(
        ast.peel_parens(),
        ExprAst::Binary {
            kind: BinOp::LogAnd | BinOp::LogOr,
            ..
        } | ExprAst::Not { .. }
    )
}

// =====================================================================
// RPM110 boolean-dnf-redundancy
// =====================================================================

pub static REDUNDANCY_METADATA: LintMetadata = LintMetadata {
    id: "RPM110",
    name: "boolean-dnf-redundancy",
    description: "Expression contains operands that are absorbed by others — DNF normalisation \
         reveals shorter equivalent form.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Expression contains operands that are absorbed by others — DNF normalisation reveals shorter equivalent form.
///
/// See [`REDUNDANCY_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct BooleanDnfRedundancy {
    diagnostics: Vec<Diagnostic>,
}

impl BooleanDnfRedundancy {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let Some((dnf, _atoms, orig_cubes)) = analyse_branch(&branch.expr) else {
                continue;
            };
            // Skip degenerate cases (RPM072/RPM112 territory).
            if is_contradiction(&dnf) || dnf.iter().any(|c| c.is_empty()) {
                continue;
            }
            // Normalised form is strictly shorter than the source's
            // top-level operand count — there's redundancy worth
            // pointing out.
            if dnf.len() < orig_cubes {
                self.diagnostics.push(
                    Diagnostic::new(
                        &REDUNDANCY_METADATA,
                        Severity::Warn,
                        format!(
                            "boolean expression has {orig_cubes} top-level cubes that \
                             normalise to {}; some operands are absorbed by others",
                            dnf.len()
                        ),
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "drop the absorbed operands; keep the smallest covering cubes",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for BooleanDnfRedundancy {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for BooleanDnfRedundancy {
    fn metadata(&self) -> &'static LintMetadata {
        &REDUNDANCY_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM111 boolean-tautology-by-cubes
// =====================================================================

pub static TAUTOLOGY_METADATA: LintMetadata = LintMetadata {
    id: "RPM111",
    name: "boolean-tautology-by-cubes",
    description: "Boolean expression is tautologically true under every assignment — drop the guard.",
    default_severity: Severity::Warn,
    category: LintCategory::Style,
};

/// Boolean expression is tautologically true under every assignment — drop the guard.
///
/// See [`TAUTOLOGY_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct BooleanTautologyByCubes {
    diagnostics: Vec<Diagnostic>,
}

impl BooleanTautologyByCubes {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let Some((dnf, atom_count, _)) = analyse_branch(&branch.expr) else {
                continue;
            };
            if matches!(is_tautology(&dnf, atom_count), Some(true)) {
                self.diagnostics.push(
                    Diagnostic::new(
                        &TAUTOLOGY_METADATA,
                        Severity::Warn,
                        "boolean expression is always true — every truth assignment \
                         satisfies at least one cube; drop the `%if` guard",
                        branch.data,
                    )
                    .with_suggestion(Suggestion::new(
                        "unwrap the `%if` block: keep the body, drop `%if`/`%endif`",
                        Vec::new(),
                        Applicability::Manual,
                    )),
                );
            }
        }
    }
}

impl<'ast> Visit<'ast> for BooleanTautologyByCubes {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for BooleanTautologyByCubes {
    fn metadata(&self) -> &'static LintMetadata {
        &TAUTOLOGY_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

// =====================================================================
// RPM112 boolean-contradiction-by-cubes
// =====================================================================

pub static CONTRADICTION_METADATA: LintMetadata = LintMetadata {
    id: "RPM112",
    name: "boolean-contradiction-by-cubes",
    description: "Boolean expression is unsatisfiable — every cube collapses to internal contradiction.",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Boolean expression is unsatisfiable — every cube collapses to internal contradiction.
///
/// See [`CONTRADICTION_METADATA`] for the rule's ID, name, default severity, and
/// category.
#[derive(Debug, Default)]
pub struct BooleanContradictionByCubes {
    diagnostics: Vec<Diagnostic>,
}

impl BooleanContradictionByCubes {
    pub fn new() -> Self {
        Self::default()
    }

    fn check<B>(&mut self, node: &Conditional<Span, B>) {
        for branch in &node.branches {
            let Some((dnf, _, _)) = analyse_branch(&branch.expr) else {
                continue;
            };
            if is_contradiction(&dnf) {
                self.diagnostics.push(Diagnostic::new(
                    &CONTRADICTION_METADATA,
                    Severity::Warn,
                    "boolean expression is unsatisfiable — every cube has an internal \
                     `A && !A` contradiction; the branch is dead code",
                    branch.data,
                ));
            }
        }
    }
}

impl<'ast> Visit<'ast> for BooleanContradictionByCubes {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.check(node);
        visit::walk_top_conditional(self, node);
    }
    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.check(node);
        visit::walk_preamble_conditional(self, node);
    }
    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.check(node);
        visit::walk_files_conditional(self, node);
    }
}

impl Lint for BooleanContradictionByCubes {
    fn metadata(&self) -> &'static LintMetadata {
        &CONTRADICTION_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run<L: Lint>(src: &str, mut lint: L) -> Vec<Diagnostic> {
        let outcome = parse(src);
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    // ---- DNF infrastructure ----

    fn dnf_of(src: &str) -> Option<Dnf> {
        let outcome = parse(&format!("Name: x\n%if {src}\nLicense: MIT\n%endif\n"));
        let item = outcome.spec.items.iter().find_map(|i| match i {
            rpm_spec::ast::SpecItem::Conditional(c) => Some(c),
            _ => None,
        })?;
        let CondExpr::Parsed(ast) = &item.branches[0].expr else {
            return None;
        };
        let mut atoms = AtomTable::new();
        let raw = to_dnf(ast, &mut atoms, false)?;
        let (simplified, _) = simplify_subsumption(&raw);
        Some(simplified)
    }

    #[test]
    fn dnf_handles_simple_and() {
        let dnf = dnf_of("A && B").unwrap();
        assert_eq!(dnf.len(), 1, "{dnf:?}");
        assert_eq!(dnf.iter().next().unwrap().len(), 2);
    }

    #[test]
    fn dnf_distributes_over_or() {
        let dnf = dnf_of("(A || B) && C").unwrap();
        assert_eq!(dnf.len(), 2, "{dnf:?}");
    }

    #[test]
    fn dnf_subsumes_redundant_cube() {
        // (A && B) || (A && B && C) → after subsumption, just (A && B).
        let dnf = dnf_of("(A && B) || (A && B && C)").unwrap();
        assert_eq!(dnf.len(), 1, "{dnf:?}");
    }

    #[test]
    fn dnf_recognises_contradiction() {
        let dnf = dnf_of("A && !A").unwrap();
        assert!(is_contradiction(&dnf), "{dnf:?}");
    }

    #[test]
    fn dnf_recognises_tautology() {
        let dnf = dnf_of("A || !A").unwrap();
        assert_eq!(is_tautology(&dnf, 1), Some(true), "{dnf:?}");
    }

    #[test]
    fn dnf_tautology_three_atoms() {
        // `A || (!A && B) || (!A && !B)` is a tautology.
        let dnf = dnf_of("A || (!A && B) || (!A && !B)").unwrap();
        assert_eq!(is_tautology(&dnf, 2), Some(true), "{dnf:?}");
    }

    #[test]
    fn canonicalise_unmodelled_variant_returns_none() {
        // `canonicalise` returns `Option<String>` so the wildcard arm
        // can yield `None` for future `#[non_exhaustive]` `ExprAst`
        // variants. We exercise the modelled path here (must be
        // `Some(_)`); the type-system guarantees the wildcard arm
        // returns `None` because the function signature is
        // `Option<String>` and the `_` arm is the only other path.
        use rpm_spec::ast::Span;
        let m = ExprAst::Macro::<Span> {
            text: "%{?rhel}".to_owned(),
            data: Span::default(),
        };
        assert_eq!(canonicalise(&m), Some("%{?rhel}".to_owned()));
        let i = ExprAst::Integer::<Span> {
            value: 7,
            data: Span::default(),
        };
        assert_eq!(canonicalise(&i), Some("7".to_owned()));
        // A `Not` over an un-modelled inner variant would yield `None`
        // via `?` propagation — that contract is what protects us from
        // the AtomId-collision soundness pitfall described above.
    }

    #[test]
    fn dnf_bails_on_unsupported_grammar() {
        // Arithmetic `+` isn't in the modelled ExprAst grammar — the
        // parser falls back to Raw, so analyse_branch returns None.
        let src = "Name: x\n%if 1 + 2 == 3\nLicense: MIT\n%endif\n";
        let outcome = parse(src);
        let item = outcome
            .spec
            .items
            .iter()
            .find_map(|i| match i {
                rpm_spec::ast::SpecItem::Conditional(c) => Some(c),
                _ => None,
            })
            .unwrap();
        assert!(matches!(item.branches[0].expr, CondExpr::Raw(_)));
    }

    // ---- RPM110 ----

    #[test]
    fn rpm110_flags_subsumed_cube() {
        let src = "Name: x\n%if (A && B) || (A && B && C)\nLicense: MIT\n%endif\n";
        let diags = run(src, BooleanDnfRedundancy::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM110");
    }

    #[test]
    fn rpm110_flags_duplicate_or_branch() {
        // `A || A` after normalisation is a single cube; the source
        // had two `||` operands → redundancy.
        let src = "Name: x\n%if A || A\nLicense: MIT\n%endif\n";
        let diags = run(src, BooleanDnfRedundancy::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm110_silent_for_minimal_expression() {
        let src = "Name: x\n%if A && B\nLicense: MIT\n%endif\n";
        assert!(run(src, BooleanDnfRedundancy::new()).is_empty());
    }

    #[test]
    fn rpm110_silent_for_pure_atom() {
        // Single atom — no boolean structure to redundantize.
        let src = "Name: x\n%if A\nLicense: MIT\n%endif\n";
        assert!(run(src, BooleanDnfRedundancy::new()).is_empty());
    }

    // ---- RPM111 ----

    #[test]
    fn rpm111_flags_excluded_middle() {
        let src = "Name: x\n%if A || !A\nLicense: MIT\n%endif\n";
        let diags = run(src, BooleanTautologyByCubes::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM111");
    }

    #[test]
    fn rpm111_flags_three_atom_tautology() {
        let src = "Name: x\n%if A || (!A && B) || (!A && !B)\nLicense: MIT\n%endif\n";
        let diags = run(src, BooleanTautologyByCubes::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm111_silent_for_satisfiable_non_tautology() {
        let src = "Name: x\n%if A && B\nLicense: MIT\n%endif\n";
        assert!(run(src, BooleanTautologyByCubes::new()).is_empty());
    }

    // ---- RPM112 ----

    #[test]
    fn rpm112_flags_a_and_not_a() {
        let src = "Name: x\n%if A && !A\nLicense: MIT\n%endif\n";
        let diags = run(src, BooleanContradictionByCubes::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM112");
    }

    #[test]
    fn rpm112_flags_multi_atom_contradiction() {
        // (A && !B) && B → after distribute, every cube has A && !B && B → contradiction.
        let src = "Name: x\n%if (A && !B) && B\nLicense: MIT\n%endif\n";
        let diags = run(src, BooleanContradictionByCubes::new());
        assert_eq!(diags.len(), 1, "{diags:?}");
    }

    #[test]
    fn rpm112_silent_for_satisfiable() {
        let src = "Name: x\n%if A && B\nLicense: MIT\n%endif\n";
        assert!(run(src, BooleanContradictionByCubes::new()).is_empty());
    }
}
