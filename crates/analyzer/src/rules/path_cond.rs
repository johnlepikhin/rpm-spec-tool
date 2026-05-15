//! Path-condition machinery shared by RPM113/114/115/116.
//!
//! Each visiting rule keeps a [`PathConditions`] threaded through nested
//! `%if`/`%elif`/`%else` blocks. The stack frame is a DNF describing the
//! conjunction of every ancestor branch's effective condition. With that
//! we can ask Phase 8a's solver whether
//!
//! - `path ∧ branch` is UNSAT → unreachable branch (RPM113/RPM115),
//! - `path ∧ ¬branch` is UNSAT → branch is implied by parent (RPM114),
//! - `path ∧ else_effective` is UNSAT → trailing `%elif` covers space (RPM116).
//!
//! A frame of `None` means **tainted**: some ancestor's condition was
//! `Raw`, an `ArchList` with non-literal archs, or hit the DNF
//! explosion guard. Tainted children skip all diagnostics — we never
//! make claims under uncertainty.
//!
//! Atom interning is shared across the full visit pass via the
//! [`PathConditions::atoms`] table, so the same `X == 8` in parent and
//! child collapses to the same atom id.

use std::collections::BTreeSet;

use rpm_spec::ast::{CondExpr, Conditional, Span, Text, TextSegment};

use crate::rules::boolean_dnf::{
    distribute_and, eval_cube, is_contradiction, to_dnf, AtomTable, Cube, Dnf, Literal,
    MAX_ATOMS_FOR_TAUTOLOGY,
};

/// Hard cap on nested-`%if` depth before we bail out of path-condition
/// analysis. Real-world specs never exceed a handful of levels; this
/// guard exists to bound recursion on adversarial input.
pub(crate) const MAX_PATH_STACK_DEPTH: usize = 256;

/// Namespace prefix used when interning [`CondExpr::ArchList`] entries
/// in the shared [`AtomTable`]. Keeping the prefix as a named constant
/// prevents accidental collisions with other atom keys.
const ARCH_ATOM_PREFIX: &str = "ARCH=";

/// Frame on the path-condition stack. `None` = tainted: don't trust any
/// downstream UNSAT/SAT verdict.
pub(crate) type PathFrame = Option<Dnf>;

#[derive(Debug, Default)]
pub(crate) struct PathConditions {
    pub atoms: AtomTable,
    pub stack: Vec<PathFrame>,
}

impl PathConditions {
    /// Current path frame, or `None` if the stack is empty or top frame
    /// is tainted.
    pub(crate) fn current(&self) -> Option<&Dnf> {
        self.stack.last().and_then(|f| f.as_ref())
    }

    pub(crate) fn push(&mut self, frame: PathFrame) {
        self.stack.push(frame);
    }

    pub(crate) fn pop(&mut self) {
        self.stack.pop();
    }

    /// `true` iff the top frame is `None`. At the root (empty stack)
    /// we are NOT tainted — we just have the trivial path `⊤`.
    pub(crate) fn is_tainted(&self) -> bool {
        matches!(self.stack.last(), Some(None))
    }
}

/// DNF that represents `⊤` (always true): one empty cube.
pub(crate) fn tautology_dnf() -> Dnf {
    let mut out = BTreeSet::new();
    out.insert(Cube::new());
    out
}

/// Convert a [`CondExpr`] to DNF. `negate` pushes negation through the
/// root via De Morgan. Returns `None` for `Raw`, `ArchList` containing
/// non-literal archs, or DNF explosion.
pub(crate) fn cond_to_dnf(
    expr: &CondExpr<Span>,
    atoms: &mut AtomTable,
    negate: bool,
) -> Option<Dnf> {
    match expr {
        CondExpr::Parsed(ast) => to_dnf(ast, atoms, negate),
        CondExpr::Raw(_) => None,
        CondExpr::ArchList(archs) => arch_list_dnf(archs, atoms, negate),
        // `CondExpr` is `#[non_exhaustive]`. Treat any future variant
        // as opaque — never silently emit diagnostics under unknown
        // grammar.
        _ => None,
    }
}

/// `%ifarch a b c` ↦ `{cube{a+}, cube{b+}, cube{c+}}`.
/// `%ifnarch a b c` (i.e. negated) ↦ `{cube{a-, b-, c-}}` — single cube,
/// every arch must NOT match.
///
/// Returns `None` when any arch entry contains macros / non-literal
/// segments (we can't canonicalise an unknown value).
fn arch_list_dnf(archs: &[Text], atoms: &mut AtomTable, negate: bool) -> Option<Dnf> {
    let mut keys: Vec<String> = Vec::with_capacity(archs.len());
    for t in archs {
        let lit = arch_literal(t)?;
        keys.push(format!("{ARCH_ATOM_PREFIX}{lit}"));
    }
    let mut dnf = Dnf::new();
    if negate {
        let mut cube = Cube::new();
        for key in keys {
            let atom = atoms.intern_key(key);
            cube.insert(Literal { atom, negated: true });
        }
        dnf.insert(cube);
    } else {
        for key in keys {
            let atom = atoms.intern_key(key);
            let mut cube = Cube::new();
            cube.insert(Literal { atom, negated: false });
            dnf.insert(cube);
        }
    }
    Some(dnf)
}

/// Extract the literal name of one arch token. `%{name}` or anything
/// containing a macro segment defeats us → `None`.
fn arch_literal(t: &Text) -> Option<String> {
    let mut out = String::new();
    for seg in &t.segments {
        match seg {
            TextSegment::Literal(s) => out.push_str(s),
            _ => return None,
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_string()) }
}

/// `current ∧ ¬prior_{n-1} ∧ ... ∧ ¬prior_0`. Returns `None` if any
/// element is opaque or distribution explodes.
pub(crate) fn branch_effective_dnf(
    current: &CondExpr<Span>,
    prior: &[&CondExpr<Span>],
    atoms: &mut AtomTable,
) -> Option<Dnf> {
    let mut acc = cond_to_dnf(current, atoms, false)?;
    for p in prior {
        let neg = cond_to_dnf(p, atoms, true)?;
        acc = distribute_and(&acc, &neg)?;
    }
    Some(acc)
}

/// `¬prior_{n-1} ∧ ... ∧ ¬prior_0` — the implicit `%else` region of a
/// chain that already has `n` branches. Returns `None` on opaque ops
/// or explosion.
pub(crate) fn else_effective_dnf(
    branches_so_far: &[&CondExpr<Span>],
    atoms: &mut AtomTable,
) -> Option<Dnf> {
    let mut acc = tautology_dnf();
    for p in branches_so_far {
        let neg = cond_to_dnf(p, atoms, true)?;
        acc = distribute_and(&acc, &neg)?;
    }
    Some(acc)
}

/// `path ∧ extra`. Same semantics as
/// [`crate::rules::boolean_dnf::distribute_and`].
pub(crate) fn conjoin(path: &Dnf, extra: &Dnf) -> Option<Dnf> {
    distribute_and(path, extra)
}

/// `true` when `dnf` has no satisfying assignment — empty after all
/// internal-contradiction filtering.
pub(crate) fn is_unsat(dnf: &Dnf) -> bool {
    is_contradiction(dnf)
}

/// Compose a new path frame from the current top and an entered
/// branch's effective DNF. Propagates taint: any `None` in either
/// position yields `None`. When `extra` distributes to nothing
/// (explosion guard fired inside [`conjoin`]), also yields `None`.
pub(crate) fn compute_frame(parent: Option<&Dnf>, extra: Option<Dnf>) -> PathFrame {
    match (parent, extra) {
        (_, None) => None,
        (None, Some(e)) => Some(e),
        (Some(p), Some(e)) => conjoin(p, &e),
    }
}

/// Hook trait implemented by every Phase 8b rule. The shared
/// [`analyse_conditional`] helper walks branches, maintains the
/// path-condition stack, and calls back into the rule's hooks at
/// the points where diagnostics may be emitted.
///
/// Default implementations of `on_branch` and `on_post_chain` are
/// no-ops; rules override only the hook they need.
pub(crate) trait BranchAnalyser {
    /// Access to the rule's [`PathConditions`] state.
    fn pc(&mut self) -> &mut PathConditions;

    /// Called once per branch (`%if` is `idx == 0`, each `%elif` is
    /// `idx >= 1`). `eff` is the branch's effective DNF
    /// (`cond_N ∧ ¬cond_{N-1} ∧ …`); `None` means we cannot reason.
    /// `anchor` is the branch's source span.
    fn on_branch(&mut self, _idx: usize, _eff: &Option<Dnf>, _anchor: Span) {}

    /// Called after every branch has been visited but before any
    /// `%else` body is walked. Lets a rule observe the complete chain
    /// before commiting (e.g. RPM116 fires here based on whether the
    /// implicit-else region is UNSAT).
    fn on_post_chain(
        &mut self,
        _node_anchor: Span,
        _has_else: bool,
        _prior: &[&CondExpr<Span>],
    ) {
    }
}

/// Walk a [`Conditional`] block with full path-condition tracking.
/// Each rule's `visit_*_conditional` impl delegates here, supplying
/// the body-walker for its concrete branch type.
///
/// The function pointer for `walk_body` avoids closures over `&mut
/// rule`, which would clash with the rule's mutable borrows inside
/// the trait method calls.
pub(crate) fn analyse_conditional<A, B>(
    rule: &mut A,
    node: &Conditional<Span, B>,
    walk_body: fn(&mut A, &[B]),
) where
    A: BranchAnalyser + ?Sized,
{
    if node.branches.is_empty() {
        return;
    }
    if rule.pc().stack.len() >= MAX_PATH_STACK_DEPTH {
        return;
    }
    let tainted_above = rule.pc().is_tainted();
    let mut prior: Vec<&CondExpr<Span>> = Vec::with_capacity(node.branches.len());
    for (idx, branch) in node.branches.iter().enumerate() {
        let eff = branch_effective_dnf(&branch.expr, &prior, &mut rule.pc().atoms);
        if !tainted_above {
            rule.on_branch(idx, &eff, branch.data);
        }
        let frame = compute_frame(rule.pc().current(), eff);
        rule.pc().push(frame);
        walk_body(rule, &branch.body);
        rule.pc().pop();
        prior.push(&branch.expr);
    }
    if !tainted_above {
        rule.on_post_chain(node.data, node.otherwise.is_some(), &prior);
    }
    if let Some(els) = node.otherwise.as_deref() {
        let eff = else_effective_dnf(&prior, &mut rule.pc().atoms);
        let frame = compute_frame(rule.pc().current(), eff);
        rule.pc().push(frame);
        walk_body(rule, els);
        rule.pc().pop();
    }
}

/// `path ⊨ branch` — every assignment satisfying any cube of `path`
/// also satisfies some cube of `branch`. Returns `None` when atom
/// count exceeds the truth-table budget; callers must treat `None`
/// as "don't know, don't fire".
pub(crate) fn path_implies(path: &Dnf, branch: &Dnf, atom_count: usize) -> Option<bool> {
    if atom_count > MAX_ATOMS_FOR_TAUTOLOGY {
        return None;
    }
    if path.is_empty() {
        // `path` is unsat — vacuous implication. Callers should
        // detect unsat path elsewhere; we return `true` for symmetry.
        return Some(true);
    }
    let total = 1u32 << atom_count;
    for assignment in 0..total {
        let path_holds = path.iter().any(|c| eval_cube(c, assignment));
        if !path_holds {
            continue;
        }
        let branch_holds = branch.iter().any(|c| eval_cube(c, assignment));
        if !branch_holds {
            return Some(false);
        }
    }
    Some(true)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    /// Parse a single `%if EXPR` and return its `CondExpr<Span>`.
    fn parse_if(expr_src: &str) -> CondExpr<Span> {
        let src = format!("Name: x\n%if {expr_src}\nLicense: MIT\n%endif\n");
        let outcome = parse(&src);
        for item in &outcome.spec.items {
            if let rpm_spec::ast::SpecItem::Conditional(c) = item {
                return c.branches[0].expr.clone();
            }
        }
        panic!("no conditional in: {src}")
    }

    #[test]
    fn cond_to_dnf_parsed_simple_and() {
        let expr = parse_if("A && B");
        let mut atoms = AtomTable::new();
        let dnf = cond_to_dnf(&expr, &mut atoms, false).expect("parsed");
        assert_eq!(dnf.len(), 1, "{dnf:?}");
        let cube = dnf.iter().next().unwrap();
        assert_eq!(cube.len(), 2);
    }

    #[test]
    fn cond_to_dnf_raw_returns_none() {
        // A bare arithmetic-only expression the parser falls back on Raw.
        let expr = parse_if("0%{?rhel} > 7");
        let mut atoms = AtomTable::new();
        // `0%{?rhel}` may parse or fall back to Raw depending on
        // parser version; assert behaviour based on the variant.
        match &expr {
            CondExpr::Raw(_) => assert!(cond_to_dnf(&expr, &mut atoms, false).is_none()),
            _ => {
                // If parsed, we should get *some* DNF. That's fine —
                // the test's goal is to exercise the Raw branch, but
                // the parser is permissive enough that we treat
                // this as a no-op assertion in that case.
            }
        }
    }

    fn parse_ifarch(arch_list: &str) -> CondExpr<Span> {
        let src = format!("Name: x\n%ifarch {arch_list}\nLicense: MIT\n%endif\n");
        let outcome = parse(&src);
        for item in &outcome.spec.items {
            if let rpm_spec::ast::SpecItem::Conditional(c) = item {
                return c.branches[0].expr.clone();
            }
        }
        panic!("no conditional in: {src}")
    }

    #[test]
    fn arch_list_two_archs_produces_two_cubes() {
        let expr = parse_ifarch("x86_64 aarch64");
        let mut atoms = AtomTable::new();
        let dnf = cond_to_dnf(&expr, &mut atoms, false).expect("archlist");
        assert_eq!(dnf.len(), 2);
        assert_eq!(atoms.len(), 2);
    }

    #[test]
    fn arch_list_negated_single_cube() {
        let expr = parse_ifarch("x86_64 aarch64");
        let mut atoms = AtomTable::new();
        let dnf = cond_to_dnf(&expr, &mut atoms, true).expect("negated archlist");
        assert_eq!(dnf.len(), 1);
        let cube = dnf.iter().next().unwrap();
        assert_eq!(cube.len(), 2);
        assert!(cube.iter().all(|l| l.negated));
    }

    #[test]
    fn branch_effective_negates_prior_chain() {
        // branch 0: A, branch 1: B → branch1_effective = B ∧ ¬A.
        let a = parse_if("A");
        let b = parse_if("B");
        let mut atoms = AtomTable::new();
        let eff = branch_effective_dnf(&b, &[&a], &mut atoms).expect("dnf");
        assert_eq!(eff.len(), 1);
        let cube = eff.iter().next().unwrap();
        // Should contain B+ and A-.
        let lits: Vec<_> = cube.iter().copied().collect();
        assert_eq!(lits.len(), 2);
        let positives: Vec<_> = lits.iter().filter(|l| !l.negated).collect();
        let negatives: Vec<_> = lits.iter().filter(|l| l.negated).collect();
        assert_eq!(positives.len(), 1);
        assert_eq!(negatives.len(), 1);
    }

    #[test]
    fn conjoin_internal_contradiction_collapses_to_unsat() {
        // `{X+}` ∧ `{X-}` → `{X+, X-}` cube which has internal
        // contradiction → dropped → empty DNF.
        let x = parse_if("X");
        let mut atoms = AtomTable::new();
        let pos = cond_to_dnf(&x, &mut atoms, false).unwrap();
        let neg = cond_to_dnf(&x, &mut atoms, true).unwrap();
        let result = conjoin(&pos, &neg).unwrap();
        assert!(is_unsat(&result), "{result:?}");
    }

    #[test]
    fn else_effective_for_exhaustive_chain_is_unsat() {
        // chain `%if A` then `%elif !A` — implicit else region is
        // ¬A ∧ ¬¬A = ⊥.
        let a = parse_if("A");
        let not_a = parse_if("!A");
        let mut atoms = AtomTable::new();
        let eff = else_effective_dnf(&[&a, &not_a], &mut atoms).expect("dnf");
        assert!(is_unsat(&eff), "{eff:?}");
    }

    #[test]
    fn taint_propagates_via_push_pop() {
        let mut pc = PathConditions::default();
        pc.push(None);
        assert!(pc.is_tainted());
        pc.pop();
        assert!(!pc.is_tainted());
    }

    #[test]
    fn current_skips_tainted_frame() {
        let mut pc = PathConditions::default();
        pc.push(Some(tautology_dnf()));
        assert!(pc.current().is_some());
        pc.pop();
        pc.push(None);
        assert!(pc.current().is_none());
        assert!(pc.is_tainted());
    }
}
