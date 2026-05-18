//! Cross-profile branch coverage for `%if` / `%ifarch` / `%ifos`
//! conditionals.
//!
//! Walks every conditional in the parsed spec, evaluates each branch
//! against every member profile of a [`ResolvedTargetSet`], and
//! reports which profiles see the branch active. The report drives
//! `matrix coverage` — release engineers use it to spot dead
//! branches, distro-only branches, and conditions the analyzer
//! couldn't evaluate.
//!
//! Phase 1 evaluator scope:
//!
//! * `%ifarch` / `%ifnarch` — strict equality of any list entry
//!   against [`ArchInfo::build_arch`]. Handles bare names and
//!   `%{macro}` arches (the macro is expanded via the profile's
//!   registry before comparison).
//! * `%ifos` / `%ifnos` — same, against [`ArchInfo::build_os`].
//! * `%if EXPR` — best-effort numeric evaluation. Macros are
//!   expanded through [`MacroRegistry::expand_to_literal`]; the
//!   resulting text is parsed as `i64` (non-zero ⇒ active). When
//!   any sub-expression can't be resolved (undefined macro,
//!   non-numeric body, string compare, …) the branch is reported
//!   as [`BranchActivity::Indeterminate`] rather than guessing.
//!
//! Limitations are intentional: a full RPM expression evaluator
//! is a separate project. The "indeterminate" classification is
//! load-bearing — it tells the user "this needs human review", not
//! "this branch is dead".

use std::collections::{BTreeMap, BTreeSet, HashSet};

use rpm_spec::ast::{
    CondBranch, CondExpr, CondKind, Conditional, ExprAst, FilesContent, MacroRef, PreambleContent,
    Span, SpecFile, SpecItem, Text, TextSegment,
};
use rpm_spec_profile::{MacroRegistry, Profile, ResolvedTargetSet};
use serde::{Serialize, Serializer};

use crate::visit::{
    Visit, walk_files_conditional, walk_preamble_conditional, walk_top_conditional,
};

/// Recursion depth for [`MacroRegistry::expand_to_literal`] inside
/// the evaluator. Matches the depth used by other analyzer modules
/// that resolve macros via the same registry.
const EXPAND_DEPTH: u8 = 8;

/// Reasons the evaluator could not produce a concrete `Active` /
/// `Inactive` verdict. Surfaces through
/// [`BranchCoverage::indeterminate_reasons`] so users can act on
/// the diagnosis instead of "indeterminate, ¯\\_(ツ)_/¯".
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum EvalError {
    /// `%{name}` or `%name` referenced an unconditional macro that
    /// the profile's registry does not define.
    #[error("undefined macro: {0}")]
    UndefinedMacro(String),
    /// Condition body contained non-ASCII bytes. The byte-cursor
    /// scanner in `expand_raw_string` would emit mojibake; bail
    /// instead. Matches the contract of `MacroRegistry::expand_body`.
    #[error("non-ASCII content in condition body")]
    NonAscii,
    /// `%(shell)` expansion appears in the condition. We don't
    /// run shell commands at lint time.
    #[error("shell expansion `%(...)` is not analysed")]
    ShellExpansion,
    /// `%[expr]` arithmetic expression. RPM evaluates these at
    /// build time; we don't.
    #[error("arithmetic expression `%[...]` is not analysed")]
    ArithmeticExpr,
    /// `%{name:default}` or `%{!?name:default}` — we don't model
    /// the default body. Treating it as empty would change
    /// activation semantics in subtle ways, so we bail.
    #[error("default-value macro form `%{{name:default}}` is not analysed")]
    UnmodelledDefault,
    /// Binary operator applied to operands of different shape
    /// (e.g. integer compared with string) — RPM would coerce in
    /// confusing ways, so we surface for human review.
    #[error("string vs numeric type mismatch in binary operator")]
    TypeMismatch,
    /// `<`/`>`/`<=`/`>=`/`&&`/`||` applied to string operands.
    /// Only `==` / `!=` are modelled for strings.
    #[error("string ordering (<, >) is not analysed")]
    StringOrdering,
    /// `%ifarch` / `%ifnarch` / `%elifarch` against a profile that
    /// doesn't have `build_arch` populated.
    #[error("profile has no `build_arch` set")]
    MissingBuildArch,
    /// `%ifos` / `%ifnos` / `%elifos` against a profile that doesn't
    /// have `build_os` populated.
    #[error("profile has no `build_os` set")]
    MissingBuildOs,
    /// Bare identifier in a parsed condition (`%if foo`) — RPM
    /// rejects these at build time; we report them rather than
    /// coercing.
    #[error("bare identifier `{0}` in condition")]
    IdentifierUnsupported(String),
    /// Catch-all for parser corner-cases we haven't modelled yet
    /// (e.g. a future `CondExpr` variant that the
    /// `#[non_exhaustive]` upstream enum may grow). The inner
    /// `&'static str` is operator-friendly when produced by the
    /// canonical call sites; [`Self::code`] maps it to a stable
    /// tag so reports group cleanly even when the inner wording
    /// drifts.
    #[error("not analysed: {0}")]
    Unsupported(&'static str),
}

/// Categorisation of an [`EvalError`] for the renderer's triage
/// view. The split exists because operators face two different
/// failure modes when a coverage run reports `indeterminate`:
///
/// * [`Self::Config`] — operator can resolve by editing
///   `.rpmspec.toml` (declaring `[macros.NAME]` variants, fleshing
///   out a profile's `build_arch`/`build_os`, etc.). Actionable.
/// * [`Self::Tool`] — the evaluator doesn't model this construct.
///   Operator can't fix it without a code change (or a tool bug
///   filing). Coverage degrades gracefully but reachability for
///   these branches is genuinely unknown.
///
/// The renderer surfaces this category inline (`[config]` /
/// `[tool]`) so the operator can scan the indeterminate list and
/// know which branches are theirs to fix versus which require a
/// project-side change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum EvalErrorCategory {
    /// Project-config gap. Fix by editing `.rpmspec.toml` or the
    /// profile data this evaluator reads.
    Config,
    /// Tool-side limitation. The evaluator doesn't model the
    /// construct yet; reachability for the affected branch is
    /// unknown until the evaluator grows support.
    Tool,
}

impl EvalError {
    /// Whether the error reflects a missing piece of project
    /// configuration (operator-fixable) or a tool-side limitation
    /// (requires a code change). See [`EvalErrorCategory`].
    #[must_use]
    pub fn category(&self) -> EvalErrorCategory {
        match self {
            Self::UndefinedMacro(_)
            | Self::MissingBuildArch
            | Self::MissingBuildOs
            | Self::IdentifierUnsupported(_) => EvalErrorCategory::Config,
            Self::NonAscii
            | Self::ShellExpansion
            | Self::ArithmeticExpr
            | Self::UnmodelledDefault
            | Self::TypeMismatch
            | Self::StringOrdering
            | Self::Unsupported(_) => EvalErrorCategory::Tool,
        }
    }

    /// Stable short identifier for this error class. Renderer uses
    /// the code to group/filter and to produce a fixed tag like
    /// `[E-ARITH-RAW]` that survives future wording tweaks to the
    /// Display string. Codes are kebab-or-shouty-case ASCII so
    /// they're greppable in CI logs.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::UndefinedMacro(_) => "undefined-macro",
            Self::NonAscii => "non-ascii",
            Self::ShellExpansion => "shell-expansion",
            Self::ArithmeticExpr => "arith-expr",
            Self::UnmodelledDefault => "default-value-form",
            Self::TypeMismatch => "type-mismatch",
            Self::StringOrdering => "string-ordering",
            Self::MissingBuildArch => "missing-build-arch",
            Self::MissingBuildOs => "missing-build-os",
            Self::IdentifierUnsupported(_) => "identifier-unsupported",
            Self::Unsupported(text) => unsupported_code_from_text(text),
        }
    }
}

/// Map the inner `Unsupported(&'static str)` message back to a
/// stable code. The set of inner messages is small and grows
/// rarely; matching on substrings rather than the full string
/// gives us code stability even if the wording is polished.
fn unsupported_code_from_text(text: &str) -> &'static str {
    if text.contains("arithmetic") {
        "arith-raw"
    } else if text.contains("non-arch") {
        "non-arch-in-ifarch"
    } else if text.contains("non-expression") {
        "non-expression-in-if"
    } else if text.contains("CondKind") {
        "unknown-cond-kind"
    } else if text.contains("CondExpr") {
        "unknown-cond-expr"
    } else if text.contains("ExprAst") {
        "unknown-expr-ast"
    } else if text.contains("TextSegment") {
        "unknown-text-segment"
    } else if text.contains("macro ref") || text.contains("malformed") {
        "malformed-macro-ref"
    } else {
        "unsupported-other"
    }
}

// Serialize as the Display string so the JSON shape stays "string per
// reason" — `coverage_json_includes_indeterminate_reasons` and any
// downstream dashboards keep working unchanged. Hand-rolled rather
// than `#[derive(Serialize)]` so the wire format matches the
// `thiserror`-generated Display message rather than an enum tag.
impl Serialize for EvalError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

/// One `%if…%endif` block as collected from the spec.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CollectedConditional {
    /// Span of the full block (head through `%endif`).
    pub span: Span,
    /// Branches in source order: index `0` is the `%if`/`%ifarch`/`%ifos`
    /// head; subsequent entries are `%elif*` clauses.
    pub branches: Vec<CollectedBranch>,
    /// `true` iff the source had an explicit `%else`. Coverage uses
    /// this to report "no branch active for any profile" as dead vs.
    /// "the implicit else is what runs".
    pub has_else: bool,
}

/// One branch (`%if`/`%elif`-style head) of a conditional.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CollectedBranch {
    pub kind: CondKind,
    pub expr: CondExpr<Span>,
    /// Span of just this branch's head line.
    pub span: Span,
    /// Human-readable rendering of the condition for diagnostics
    /// (e.g. `%if 0%{?rhel} >= 9`). Built once at collection time so
    /// renderers don't reformat per profile.
    pub display: String,
}

/// Result of evaluating one branch against one profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BranchActivity {
    /// The branch's condition evaluates to "true" for this profile.
    Active,
    /// The branch's condition evaluates to "false".
    Inactive,
    /// Evaluation requires data the analyzer doesn't have
    /// (undefined macro, opaque expression, string compare with a
    /// non-literal side, …). The user must judge whether the spec
    /// is correct for this profile.
    Indeterminate,
}

/// Per-branch coverage row.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BranchCoverage {
    pub branch: CollectedBranch,
    /// Sorted profile IDs that activate the branch.
    pub active_on: Vec<String>,
    /// Sorted profile IDs where the branch is inactive.
    pub inactive_on: Vec<String>,
    /// Sorted profile IDs where evaluation was indeterminate.
    pub indeterminate_on: Vec<String>,
    /// `profile_id → EvalError` for each entry in
    /// [`Self::indeterminate_on`]. Populated at evaluation time so
    /// renderers can surface "indeterminate because <X>" without
    /// re-running the evaluator, and downstream tooling can match on
    /// the variant rather than parsing the Display string. Empty
    /// when no profile produced an indeterminate verdict.
    pub indeterminate_reasons: BTreeMap<String, EvalError>,
    /// Sorted profile IDs where the branch is inactive under the
    /// current build but becomes active under at least one declared
    /// macro-variant value combination. See `doc/matrix.md` § "Macro
    /// variants". Empty when no `[macros.*]` config is loaded or
    /// when the branch's condition doesn't reference any declared
    /// variant macros. Profiles listed here are NOT also in
    /// [`Self::active_on`] — that's the whole point: they're
    /// conditional on a different build configuration.
    pub conditional_on: Vec<String>,
    /// `macro_name → set of values` recording which variant values
    /// contributed to making the branch reachable on at least one
    /// profile. Surfaced in the `[CONDITIONAL: macro=v1,v2]` tag and
    /// the `reachable when` line of the human renderer. Only
    /// populated when [`Self::conditional_on`] is non-empty.
    pub reachable_under: BTreeMap<String, BTreeSet<String>>,
}

impl BranchCoverage {
    /// True iff no profile activates the branch (under the current
    /// build or any declared variant) and no profile is
    /// indeterminate — the branch is dead across the whole target
    /// set. The most actionable signal for cleanup.
    ///
    /// Branches reachable only under a non-current variant value
    /// (e.g. `%if "%{edition}" == "1c"` while building with
    /// `-D edition ent`) are NOT dead: they fire in another build
    /// configuration the project explicitly declared.
    pub fn is_dead(&self) -> bool {
        self.active_on.is_empty()
            && self.indeterminate_on.is_empty()
            && self.conditional_on.is_empty()
    }

    /// True iff every profile activates the branch. Such branches
    /// usually warrant inlining (the conditional has no effect).
    /// Derived from the invariant `active + inactive + indeterminate
    /// == total_profiles`, so the caller doesn't have to plumb the
    /// matrix size in (avoiding the misuse of passing an unrelated
    /// number).
    pub fn is_universally_active(&self) -> bool {
        !self.active_on.is_empty()
            && self.inactive_on.is_empty()
            && self.indeterminate_on.is_empty()
    }

    /// True iff the branch is reachable under at least one declared
    /// variant value combination but inactive under the current
    /// build. Mutually exclusive with [`Self::is_dead`] — a branch
    /// is either genuinely dead or build-conditional, never both.
    pub fn is_conditional(&self) -> bool {
        !self.conditional_on.is_empty()
    }
}

/// Whole-spec coverage report.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CoverageReport {
    pub conditionals: Vec<CoverageEntry>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CoverageEntry {
    pub span: Span,
    pub has_else: bool,
    pub branches: Vec<BranchCoverage>,
}

/// Cap on the cartesian-product combination count any single branch
/// is allowed to enumerate when classifying it against declared macro
/// variants. Branches whose declared variants exceed this product are
/// left at the current-build verdict rather than spinning the analyser
/// for minutes — operators see a single `tracing::warn` line and can
/// trim the variant set if they really need the broader analysis.
///
/// 64 covers the realistic shapes (≤4 variant macros × ≤4 values each;
/// 2⁶) without making CI hosts pay for exponential blow-up.
pub const MAX_VARIANT_COMBINATIONS: usize = 64;

impl CoverageReport {
    /// Compute coverage for `spec` against `target_set`. One pass to
    /// collect conditionals, then per-branch × per-profile evaluation.
    /// Compute the coverage report for `spec` evaluated against
    /// every profile in `target_set`. `overrides` flips bcond states
    /// from their `%bcond_with` / `%bcond_without` defaults — pass
    /// [`crate::bcond::BcondOverrides::default()`] when the CLI did
    /// not supply any `--with` / `--without` arguments.
    ///
    /// Equivalent to [`Self::compute_with_variants`] with an empty
    /// variant map — preserved as the existing surface so callers that
    /// don't care about variant-aware classification stay unchanged.
    pub fn compute(
        spec: &SpecFile<Span>,
        target_set: &ResolvedTargetSet,
        overrides: &crate::bcond::BcondOverrides,
    ) -> Self {
        let empty: BTreeMap<String, rpm_spec_profile::MacroVariants> = BTreeMap::new();
        Self::compute_with_variants(spec, target_set, overrides, &empty)
    }

    /// Variant-aware [`Self::compute`]. After the standard
    /// active/inactive/indeterminate classification, any branch that
    /// is currently inactive on every profile is re-evaluated against
    /// every cartesian combination of the declared `macros` variants
    /// that appear in its condition. A combination that activates the
    /// branch on at least one profile promotes that profile into
    /// `BranchCoverage::conditional_on` (and records the contributing
    /// variant values in `reachable_under`), preventing the renderer
    /// from tagging it `[DEAD]`.
    ///
    /// The variant search is bounded by [`MAX_VARIANT_COMBINATIONS`].
    /// When the declared variant set for a branch exceeds the cap the
    /// branch keeps its non-variant verdict and a `tracing::warn`
    /// surfaces the skipped condition.
    pub fn compute_with_variants(
        spec: &SpecFile<Span>,
        target_set: &ResolvedTargetSet,
        overrides: &crate::bcond::BcondOverrides,
        macro_variants: &BTreeMap<String, rpm_spec_profile::MacroVariants>,
    ) -> Self {
        let bcond = crate::bcond::BcondMap::from_spec(spec, overrides);
        let mut collector = ConditionalCollector::default();
        collector.visit_spec(spec);

        let mut conditionals: Vec<CoverageEntry> = collector
            .collected
            .into_iter()
            .map(|c| evaluate_conditional(c, target_set, &bcond))
            .collect();

        if !macro_variants.is_empty() {
            for c in &mut conditionals {
                for b in &mut c.branches {
                    augment_with_variants(b, target_set, &bcond, macro_variants);
                }
            }
        }

        Self { conditionals }
    }

    /// Number of branches dead across the whole target set.
    pub fn dead_branches(&self) -> usize {
        self.conditionals
            .iter()
            .flat_map(|c| c.branches.iter())
            .filter(|b| b.is_dead())
            .count()
    }

    /// Number of branches where evaluation was indeterminate on at
    /// least one profile.
    pub fn indeterminate_branches(&self) -> usize {
        self.conditionals
            .iter()
            .flat_map(|c| c.branches.iter())
            .filter(|b| !b.indeterminate_on.is_empty())
            .count()
    }

    /// Total number of branches considered (excludes the implicit
    /// `%else` body).
    pub fn total_branches(&self) -> usize {
        self.conditionals.iter().map(|c| c.branches.len()).sum()
    }
}

fn evaluate_conditional(
    c: CollectedConditional,
    target_set: &ResolvedTargetSet,
    bcond: &crate::bcond::BcondMap,
) -> CoverageEntry {
    let branches = c
        .branches
        .into_iter()
        .map(|b| {
            let mut active = Vec::new();
            let mut inactive = Vec::new();
            let mut indeterminate = Vec::new();
            let mut reasons: BTreeMap<String, EvalError> = BTreeMap::new();
            for rt in &target_set.targets {
                match evaluate_branch(b.kind, &b.expr, &rt.profile, bcond) {
                    Ok(true) => active.push(rt.profile_id.clone()),
                    Ok(false) => inactive.push(rt.profile_id.clone()),
                    Err(e) => {
                        // Library-level tracing — `RUST_LOG=trace`
                        // surfaces which profile produced which
                        // reason, the most common ask when debugging
                        // unexpected `Indeterminate` rows.
                        tracing::trace!(
                            profile = %rt.profile_id,
                            kind = ?b.kind,
                            condition = %b.display,
                            reason = %e,
                            "branch indeterminate"
                        );
                        reasons.insert(rt.profile_id.clone(), e);
                        indeterminate.push(rt.profile_id.clone());
                    }
                }
            }
            active.sort();
            inactive.sort();
            indeterminate.sort();
            BranchCoverage {
                branch: b,
                active_on: active,
                inactive_on: inactive,
                indeterminate_on: indeterminate,
                indeterminate_reasons: reasons,
                conditional_on: Vec::new(),
                reachable_under: BTreeMap::new(),
            }
        })
        .collect();
    CoverageEntry {
        span: c.span,
        has_else: c.has_else,
        branches,
    }
}

// ---------------------------------------------------------------------------
// Variant-aware augmentation
// ---------------------------------------------------------------------------

/// Re-evaluate `b` under each cartesian combination of the declared
/// variant values that appear in its condition, then promote profiles
/// into `conditional_on` / `reachable_under` if any combination flips
/// them from inactive to active.
///
/// Short-circuits when:
/// * the branch is already active or indeterminate everywhere — no
///   point asking "could it activate?" when the answer is already yes
///   or unknowable;
/// * no variant macros from `macro_variants` appear in the condition —
///   variants can't influence the verdict;
/// * the cartesian product exceeds [`MAX_VARIANT_COMBINATIONS`] — we
///   refuse to spin the analyser on exponential input.
fn augment_with_variants(
    b: &mut BranchCoverage,
    target_set: &ResolvedTargetSet,
    bcond: &crate::bcond::BcondMap,
    macro_variants: &BTreeMap<String, rpm_spec_profile::MacroVariants>,
) {
    // No outer short-circuit on `b.active_on` — mixed branches (e.g.
    // `%if 0%{?rhel} || "%{edition}" == "1c"`) can be ACTIVE on rhel
    // profiles under the current build *and* CONDITIONAL on
    // non-rhel profiles via the variant `edition=1c`. The inner
    // loop skips already-active profile IDs.
    let referenced = scan_macro_names(&b.branch.display);
    let applicable: Vec<(&String, &Vec<String>)> = macro_variants
        .iter()
        .filter(|(name, mv)| referenced.contains(name.as_str()) && !mv.values.is_empty())
        .map(|(name, mv)| (name, &mv.values))
        .collect();
    if applicable.is_empty() {
        return;
    }
    let total: usize = applicable.iter().map(|(_, vs)| vs.len()).product();
    if total > MAX_VARIANT_COMBINATIONS {
        tracing::warn!(
            condition = %b.branch.display,
            combinations = total,
            cap = MAX_VARIANT_COMBINATIONS,
            "skipping variant analysis: cartesian product over capped limit"
        );
        return;
    }

    let mut conditional: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    // Per-profile outcome accumulators across all combos. Used to
    // demote variant-exhausted indeterminate profiles to inactive:
    // when every cartesian combo evaluated to Ok(false) and no
    // combo errored, the branch is provably inactive under the
    // declared variant matrix even though the base evaluation
    // couldn't decide (macro was undefined in the registry).
    let mut had_false: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut had_err: std::collections::HashSet<String> = std::collections::HashSet::new();
    let indices_total: usize = applicable.len();
    for combo_idx in 0..total {
        // Lay the cartesian product out flat — each index picks one
        // value per applicable macro. Skip pre-allocating a Vec of
        // combos to keep memory bounded for the cap-128 cases.
        let mut combo: Vec<(&String, &String)> = Vec::with_capacity(indices_total);
        let mut rem = combo_idx;
        for (name, values) in &applicable {
            let stride = values.len();
            let pick = rem % stride;
            rem /= stride;
            combo.push((name, &values[pick]));
        }
        for rt in &target_set.targets {
            if b.active_on.contains(&rt.profile_id) || conditional.contains_key(&rt.profile_id) {
                // Already active under current build (skipped via the
                // global short-circuit above) or already promoted by
                // an earlier combo — recording the same profile twice
                // adds no information. Note: we don't add to
                // had_false/had_err in this case so the demotion
                // step below correctly leaves already-conditional
                // profiles alone.
                continue;
            }
            // Indeterminate profiles (typically "macro X not defined")
            // are eligible: a declared variant for X effectively
            // supplies a value, so re-evaluation can resolve to
            // active. That's the whole point of `[macros.*]` —
            // promote indeterminate-due-to-undefined-macro into
            // CONDITIONAL when the project has told the tool which
            // values that macro can take.
            let mut profile = rt.profile.clone();
            for (mname, mvalue) in &combo {
                profile.macros.insert(
                    mname.as_str(),
                    rpm_spec_profile::MacroEntry::literal(
                        mvalue.as_str(),
                        rpm_spec_profile::Provenance::Override,
                    ),
                );
            }
            match evaluate_branch(b.branch.kind, &b.branch.expr, &profile, bcond) {
                Ok(true) => {
                    let per_macro = conditional.entry(rt.profile_id.clone()).or_default();
                    for (mname, mvalue) in &combo {
                        per_macro
                            .entry((*mname).clone())
                            .or_default()
                            .insert((*mvalue).clone());
                    }
                }
                Ok(false) => {
                    had_false.insert(rt.profile_id.clone());
                }
                Err(_) => {
                    had_err.insert(rt.profile_id.clone());
                }
            }
        }
    }

    // Collapse per-profile conditional records into the branch-level
    // shape: sorted profile id list + union over which (macro, value)
    // pairs contributed. `inactive_on` and `conditional_on` can
    // overlap (branch inactive under current build AND reachable
    // under a variant), so we don't remove from `inactive_on` here.
    if !conditional.is_empty() {
        let mut profiles: Vec<String> = conditional.keys().cloned().collect();
        profiles.sort();
        let mut union: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for per_macro in conditional.values() {
            for (mname, values) in per_macro {
                union
                    .entry(mname.clone())
                    .or_default()
                    .extend(values.iter().cloned());
            }
        }
        b.conditional_on = profiles;
        b.reachable_under = union;
    }

    // Variant exhaustion → demote indeterminate to inactive. A
    // profile that was base-indeterminate, NOT promoted to
    // conditional, had at least one Ok(false) under some variant
    // combo, and NEVER errored — the declared variant matrix proved
    // the branch is inactive on that profile. Without this step the
    // operator sees `[INDET] undefined macro: pgsql_major` on a
    // branch like `%if %{pgsql_major} < 13` even though the
    // declared variant set {13..18} demonstrates no value can ever
    // satisfy `< 13`. The correct verdict is `inactive` (and,
    // when this strip makes the branch active+indeterminate both
    // empty, `[DEAD]`).
    let demote: Vec<String> = b
        .indeterminate_on
        .iter()
        .filter(|pid| {
            !b.conditional_on.contains(pid)
                && had_false.contains(pid.as_str())
                && !had_err.contains(pid.as_str())
        })
        .cloned()
        .collect();
    if !demote.is_empty() {
        let demote_set: std::collections::HashSet<&str> =
            demote.iter().map(String::as_str).collect();
        b.indeterminate_on
            .retain(|pid| !demote_set.contains(pid.as_str()));
        for pid in &demote {
            b.indeterminate_reasons.remove(pid);
            if !b.inactive_on.contains(pid) {
                b.inactive_on.push(pid.clone());
            }
        }
        b.inactive_on.sort();
        b.inactive_on.dedup();
    }

    if conditional.is_empty() {
        return;
    }

    // Strip indeterminate verdicts that are now subsumed by the
    // CONDITIONAL classification. Only profiles actually rescued
    // into `conditional_on` are affected — a profile whose
    // variant-pass evaluation still errored stays indeterminate.
    //
    // When a rescued profile's base-evaluation reason names a
    // macro the project has declared variants for, the operator
    // has explicitly told the tool "this macro takes one of these
    // values", so reporting it ALSO as undefined contradicts the
    // CONDITIONAL tag the same renderer line carries. Drop the
    // reason (and the now-empty indeterminate entry) for that
    // profile so the operator sees one consistent verdict.
    //
    // A rescued profile with multiple base-evaluation reasons
    // would keep the ones that aren't covered by declared variants
    // — but in practice the base evaluator short-circuits on the
    // first failure, so we only ever see one reason per profile.
    let declared_variant_macros: std::collections::HashSet<&str> =
        macro_variants.keys().map(String::as_str).collect();
    let rescued_profiles: std::collections::HashSet<&str> =
        b.conditional_on.iter().map(String::as_str).collect();
    b.indeterminate_reasons.retain(|pid, reason| {
        if !rescued_profiles.contains(pid.as_str()) {
            return true;
        }
        !matches!(
            reason,
            EvalError::UndefinedMacro(name) if declared_variant_macros.contains(name.as_str())
        )
    });
    // A rescued profile whose only reason was an undefined variant
    // macro now has no recorded reason; drop it from the
    // indeterminate list entirely so it doesn't surface as
    // `(no reason recorded)`. Non-rescued profiles stay untouched.
    b.indeterminate_on
        .retain(|pid| b.indeterminate_reasons.contains_key(pid));
}

/// Collect every macro NAME referenced by a condition's display
/// rendering. The display string is the canonical post-render form
/// `CollectedBranch` carries (built by `render_condition`), so this
/// avoids rebuilding the original `CondExpr`'s scan path twice and
/// keeps the variant matcher cheap even for large specs.
///
/// Returns an empty set when the display has no macro references —
/// callers short-circuit on that to skip the variant work entirely.
fn scan_macro_names(display: &str) -> HashSet<String> {
    use rpm_spec_profile::macro_lexer::scan_macro_ref;
    let mut names = HashSet::new();
    let bytes = display.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        match scan_macro_ref(bytes, i) {
            Some(r) => {
                names.insert(r.name.to_string());
                i = r.full_range.end;
            }
            None => {
                i += 1;
            }
        }
    }
    names
}

// ---------------------------------------------------------------------------
// Collector
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct ConditionalCollector {
    collected: Vec<CollectedConditional>,
}

impl ConditionalCollector {
    fn record<B>(&mut self, c: &Conditional<Span, B>) {
        let branches = c
            .branches
            .iter()
            .map(|b: &CondBranch<Span, B>| CollectedBranch {
                kind: b.kind,
                expr: b.expr.clone(),
                span: b.data,
                display: render_condition(b.kind, &b.expr),
            })
            .collect();
        self.collected.push(CollectedConditional {
            span: c.data,
            branches,
            has_else: c.otherwise.is_some(),
        });
    }
}

impl<'ast> Visit<'ast> for ConditionalCollector {
    fn visit_top_conditional(&mut self, node: &'ast Conditional<Span, SpecItem<Span>>) {
        self.record(node);
        walk_top_conditional(self, node);
    }

    fn visit_preamble_conditional(&mut self, node: &'ast Conditional<Span, PreambleContent<Span>>) {
        self.record(node);
        walk_preamble_conditional(self, node);
    }

    fn visit_files_conditional(&mut self, node: &'ast Conditional<Span, FilesContent<Span>>) {
        self.record(node);
        walk_files_conditional(self, node);
    }
}

// ---------------------------------------------------------------------------
// Display helper
// ---------------------------------------------------------------------------

fn render_condition(kind: CondKind, expr: &CondExpr<Span>) -> String {
    let head = match kind {
        CondKind::If => "%if",
        CondKind::IfArch => "%ifarch",
        CondKind::IfNArch => "%ifnarch",
        CondKind::IfOs => "%ifos",
        CondKind::IfNOs => "%ifnos",
        CondKind::Elif => "%elif",
        CondKind::ElifArch => "%elifarch",
        CondKind::ElifOs => "%elifos",
        _ => "%if",
    };
    let body = match expr {
        CondExpr::Raw(text) => render_text(text),
        CondExpr::Parsed(boxed) => render_expr_ast(boxed),
        CondExpr::ArchList(items) => items.iter().map(render_text).collect::<Vec<_>>().join(" "),
        _ => "?".to_string(),
    };
    format!("{head} {body}")
}

fn render_text(t: &Text) -> String {
    let mut out = String::new();
    for seg in &t.segments {
        match seg {
            TextSegment::Literal(s) => out.push_str(s),
            TextSegment::Macro(mr) => out.push_str(&render_macro_ref(mr)),
            _ => out.push('?'),
        }
    }
    out
}

fn render_macro_ref(mr: &MacroRef) -> String {
    let mut s = String::from("%{");
    match mr.conditional {
        rpm_spec::ast::ConditionalMacro::IfDefined => s.push('?'),
        rpm_spec::ast::ConditionalMacro::IfNotDefined => s.push_str("!?"),
        _ => {}
    }
    s.push_str(&mr.name);
    s.push('}');
    s
}

fn render_expr_ast(expr: &ExprAst<Span>) -> String {
    match expr {
        ExprAst::Integer { value, .. } => value.to_string(),
        ExprAst::String { value, .. } => format!("\"{value}\""),
        ExprAst::Macro { text, .. } => text.clone(),
        ExprAst::Identifier { name, .. } => name.clone(),
        ExprAst::Paren { inner, .. } => format!("({})", render_expr_ast(inner)),
        ExprAst::Not { inner, .. } => format!("!{}", render_expr_ast(inner)),
        ExprAst::Binary { kind, lhs, rhs, .. } => format!(
            "{} {} {}",
            render_expr_ast(lhs),
            kind.as_str(),
            render_expr_ast(rhs)
        ),
        _ => "?".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Per-branch evaluator
// ---------------------------------------------------------------------------

fn evaluate_branch(
    kind: CondKind,
    expr: &CondExpr<Span>,
    profile: &Profile,
    bcond: &crate::bcond::BcondMap,
) -> Result<bool, EvalError> {
    let raw = matches!(kind, CondKind::If | CondKind::Elif);
    let arch_check = matches!(
        kind,
        CondKind::IfArch | CondKind::IfNArch | CondKind::ElifArch
    );
    let os_check = matches!(kind, CondKind::IfOs | CondKind::IfNOs | CondKind::ElifOs);
    let negate = matches!(kind, CondKind::IfNArch | CondKind::IfNOs);

    if arch_check || os_check {
        // Build the membership set for token matching. For arch we
        // use `compatible_archs` if populated (rpm convention: the
        // list contains `build_arch` plus sub-arches and `noarch`,
        // so `%ifarch i386` matches an x86_64 host and
        // `%ifarch x86_64` matches a host running x86_64_v3).
        // OS doesn't have a compatibility chain, so we fall back to
        // the single `build_os` string.
        let candidate_set: HashSet<&str> = if arch_check {
            if profile.arch.compatible_archs.is_empty() {
                let Some(ba) = profile.arch.build_arch.as_deref() else {
                    return Err(EvalError::MissingBuildArch);
                };
                std::iter::once(ba).collect()
            } else {
                profile
                    .arch
                    .compatible_archs
                    .iter()
                    .map(String::as_str)
                    .collect()
            }
        } else {
            let Some(bo) = profile.arch.build_os.as_deref() else {
                return Err(EvalError::MissingBuildOs);
            };
            std::iter::once(bo).collect()
        };

        let arches: &[Text] = match expr {
            CondExpr::ArchList(items) => items,
            // Some parsers may emit Raw for ifarch/ifos when the
            // list is unusually formatted; expand and split.
            CondExpr::Raw(text) => {
                let expanded = expand_text(text, &profile.macros)?;
                let hit = expanded
                    .split_whitespace()
                    .any(|tok| candidate_set.contains(tok));
                return Ok(hit ^ negate);
            }
            _ => return Err(EvalError::Unsupported("non-arch CondExpr in %ifarch")),
        };

        // Order-independent: a match anywhere wins, even if other
        // entries fail to expand. Only when no entry matches AND at
        // least one entry was unevaluable do we surface the
        // expansion error.
        let mut hit = false;
        let mut deferred_error: Option<EvalError> = None;
        for arch_text in arches {
            match expand_text(arch_text, &profile.macros) {
                Ok(expanded) => {
                    if expanded
                        .split_whitespace()
                        .any(|tok| candidate_set.contains(tok))
                    {
                        hit = true;
                        break;
                    }
                }
                Err(e) => {
                    deferred_error.get_or_insert(e);
                }
            }
        }
        if hit {
            return Ok(true ^ negate);
        }
        if let Some(e) = deferred_error {
            return Err(e);
        }
        return Ok(false ^ negate);
    }

    if raw {
        let value = match expr {
            CondExpr::Raw(text) => evaluate_raw(text, &profile.macros, bcond)?,
            CondExpr::Parsed(boxed) => evaluate_expr_ast(boxed, &profile.macros, bcond)?,
            _ => return Err(EvalError::Unsupported("non-expression CondExpr in %if")),
        };
        return Ok(match value {
            EvalValue::Int(n) => n != 0,
            EvalValue::Str(s) => !s.is_empty(),
        });
    }

    Err(EvalError::Unsupported("unknown CondKind"))
}

// ---------------------------------------------------------------------------
// Expression evaluator (best effort)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum EvalValue {
    Int(i64),
    Str(String),
}

/// Resolve a `%{with NAME}` / `%{without NAME}` reference against
/// the per-spec bcond map. The two-stage return shape carries one
/// extra bit of information beyond `Option<i64>`:
///
/// * `None` — `verbatim` isn't a bcond form; caller falls through to
///   the generic macro lookup path.
/// * `Some(Ok(0|1))` — bcond resolved to a concrete state.
/// * `Some(Err(_))` — bcond was declared with a non-literal default
///   AND not overridden via CLI; surface as Indeterminate.
///
/// Both `evaluate_expr_ast` (`%if` expressions) and `expand_raw_string`
/// (Raw `%if` body) call this so the resolution policy lives in one
/// place. The bcond-syntax detection itself lives in
/// [`rpm_spec::ast::parse_bcond_verbatim`] — a single source of truth
/// shared with future consumers (LSP, formatter, lint rules).
fn resolve_bcond(verbatim: &str, bcond: &crate::bcond::BcondMap) -> Option<Result<i64, EvalError>> {
    use rpm_spec::ast::BcondForm;
    let (form, name) = rpm_spec::ast::parse_bcond_verbatim(verbatim)?;
    let state = match form {
        BcondForm::With => bcond.with_state(name),
        BcondForm::Without => bcond.without_state(name),
        // `BcondForm` is `#[non_exhaustive]` upstream. A future
        // variant we don't recognise is most safely treated as
        // "not a known bcond reference" → fall through to the
        // caller's generic macro-lookup path by returning None.
        _ => return None,
    };
    let result = match state {
        Some(b) => Ok(i64::from(b)),
        None => Err(EvalError::Unsupported(
            "bcond default expression cannot be statically evaluated; \
             pass --with/--without to force a state",
        )),
    };
    tracing::trace!(
        target: "bcond::resolved",
        form = ?form,
        name = %name,
        ok = result.is_ok(),
        "bcond resolved"
    );
    Some(result)
}

fn evaluate_expr_ast(
    expr: &ExprAst<Span>,
    macros: &MacroRegistry,
    bcond: &crate::bcond::BcondMap,
) -> Result<EvalValue, EvalError> {
    match expr {
        ExprAst::Integer { value, .. } => Ok(EvalValue::Int(*value)),
        ExprAst::String { value, .. } => {
            // The parser stores the literal source of a `%if`-string,
            // so `"%{edition}"` arrives here as the byte string
            // `%{edition}` rather than its expanded value. Without
            // running it through the same macro lexer the bare
            // `ExprAst::Macro` path uses, a `%if "%{edition}" == "ent"`
            // comparison would test the literal `%{edition}` against
            // `ent` and always be false — turning every legitimate
            // `-D edition ent` build into a spurious `[DEAD]` verdict.
            // `expand_raw_string` is the same expander the `Raw`
            // condition path uses, so behaviour is consistent across
            // both `CondExpr` shapes the parser produces.
            let expanded = expand_raw_string(value, macros, bcond)?;
            Ok(EvalValue::Str(expanded))
        }
        ExprAst::Macro { text, .. } => {
            // Try the bcond syntactic forms first. `%{with FOO}` /
            // `%{without FOO}` are RPM-builtin reads from the per-spec
            // bcond map, not generic registry lookups — the
            // `resolve_bcond` helper keeps the policy in one place
            // (also used in `expand_raw_string`).
            if let Some(result) = resolve_bcond(text, bcond) {
                return result.map(EvalValue::Int);
            }
            let name = crate::macro_usage::macro_name_from_verbatim(text)
                .ok_or(EvalError::Unsupported("macro ref text didn't parse"))?;
            let defined = macros.get(&name).is_some();
            let is_if_defined = text.contains("%{?");
            let is_if_undefined = text.contains("%{!?");
            // ExprAst::Macro stores the raw source token; the `:`
            // for default-value form lives inside `text`.
            let has_default = text.contains(':');
            // `%{?name}` expands to the macro VALUE when defined,
            // empty string when undefined — NOT to 1/0.
            // `%{!?name}` is empty when defined; when undefined
            // it's also empty IF there's no default body. With a
            // default we can't substitute without modelling it.
            if is_if_undefined {
                if defined {
                    return Ok(EvalValue::Str(String::new()));
                }
                if has_default {
                    return Err(EvalError::UnmodelledDefault);
                }
                return Ok(EvalValue::Str(String::new()));
            }
            if is_if_defined {
                if !defined {
                    // `%{?name:default}` would emit `default` when
                    // name is undefined; we don't model the default
                    // body, so surface UnmodelledDefault rather than
                    // silently producing the empty string.
                    if has_default {
                        return Err(EvalError::UnmodelledDefault);
                    }
                    return Ok(EvalValue::Str(String::new()));
                }
                // Fall through to expansion below.
            } else if !defined {
                return Err(EvalError::UndefinedMacro(name));
            }
            let lit = macros
                .expand_to_literal(&name, EXPAND_DEPTH)
                .ok_or_else(|| EvalError::UndefinedMacro(name.clone()))?;
            if let Ok(n) = lit.parse::<i64>() {
                Ok(EvalValue::Int(n))
            } else {
                Ok(EvalValue::Str(lit))
            }
        }
        ExprAst::Identifier { name, .. } => Err(EvalError::IdentifierUnsupported(name.clone())),
        ExprAst::Paren { inner, .. } => evaluate_expr_ast(inner, macros, bcond),
        ExprAst::Not { inner, .. } => {
            let v = evaluate_expr_ast(inner, macros, bcond)?;
            let n = match v {
                EvalValue::Int(n) => i64::from(n == 0),
                EvalValue::Str(s) => i64::from(s.is_empty()),
            };
            Ok(EvalValue::Int(n))
        }
        ExprAst::Binary { kind, lhs, rhs, .. } => {
            let l = evaluate_expr_ast(lhs, macros, bcond)?;
            let r = evaluate_expr_ast(rhs, macros, bcond)?;
            apply_binop(*kind, l, r)
        }
        _ => Err(EvalError::Unsupported("unknown ExprAst variant")),
    }
}

fn apply_binop(
    op: rpm_spec::ast::BinOp,
    l: EvalValue,
    r: EvalValue,
) -> Result<EvalValue, EvalError> {
    use rpm_spec::ast::BinOp;
    match (l, r) {
        (EvalValue::Int(a), EvalValue::Int(b)) => {
            let res: i64 = match op {
                BinOp::LogOr => i64::from(a != 0 || b != 0),
                BinOp::LogAnd => i64::from(a != 0 && b != 0),
                BinOp::Eq => i64::from(a == b),
                BinOp::Ne => i64::from(a != b),
                BinOp::Lt => i64::from(a < b),
                BinOp::Gt => i64::from(a > b),
                BinOp::Le => i64::from(a <= b),
                BinOp::Ge => i64::from(a >= b),
            };
            Ok(EvalValue::Int(res))
        }
        (EvalValue::Str(a), EvalValue::Str(b)) => match op {
            BinOp::Eq => Ok(EvalValue::Int(i64::from(a == b))),
            BinOp::Ne => Ok(EvalValue::Int(i64::from(a != b))),
            _ => Err(EvalError::StringOrdering),
        },
        // Mixed Int + Str — RPM would coerce in confusing ways;
        // we surface the mismatch for human review.
        _ => Err(EvalError::TypeMismatch),
    }
}

fn evaluate_raw(
    text: &Text,
    macros: &MacroRegistry,
    bcond: &crate::bcond::BcondMap,
) -> Result<EvalValue, EvalError> {
    // The parser sometimes stores the body as a single Literal
    // ("0%{?rhel}") and sometimes splits it into Literal + Macro
    // segments — depends on whether the surrounding grammar
    // tokenised the macros. Reconstruct the original source string
    // either way, then expand once via the macro lexer.
    let raw = render_text_to_source(text)?;
    let expanded = expand_raw_string(&raw, macros, bcond)?;
    let trimmed = expanded.trim();
    if let Ok(n) = trimmed.parse::<i64>() {
        return Ok(EvalValue::Int(n));
    }
    // If the expanded text contains arithmetic / comparison / boolean
    // operator characters but doesn't parse as an integer, we'd
    // otherwise wrap it in `EvalValue::Str(..)` and report non-empty
    // strings as Active (since `!s.is_empty()`). That's a false
    // positive: an unresolved `0%{?undefined} >= 9` would look Active.
    // Real arithmetic in `%if` is only modelled through `Parsed`
    // `CondExpr` — surface the gap instead of guessing.
    if trimmed
        .chars()
        .any(|c| matches!(c, '<' | '>' | '=' | '!' | '&' | '|' | '+' | '-' | '*' | '/'))
    {
        return Err(EvalError::Unsupported(
            "arithmetic in Raw condition requires Parsed CondExpr",
        ));
    }
    Ok(EvalValue::Str(trimmed.to_string()))
}

/// Re-render a [`Text`] back into a source-equivalent string so the
/// uniform macro lexer can scan it.
fn render_text_to_source(t: &Text) -> Result<String, EvalError> {
    let mut out = String::new();
    for seg in &t.segments {
        match seg {
            TextSegment::Literal(s) => out.push_str(s),
            TextSegment::Macro(mr) => out.push_str(&macro_ref_to_source(mr)),
            _ => return Err(EvalError::Unsupported("unknown TextSegment variant")),
        }
    }
    Ok(out)
}

fn macro_ref_to_source(mr: &MacroRef) -> String {
    use rpm_spec::ast::{BuiltinMacro, ConditionalMacro, MacroKind, TextSegment};
    // Re-renders a `MacroRef` back to its surface form so the macro
    // lexer in `expand_raw_string` can scan it again. Pre-Phase 12
    // this dropped args/body for every kind beyond Plain/Braced,
    // truncating `%{shrink:body}` → `%{shrink}` and
    // `%{with foo}` → `%{with}` — both produced spurious
    // "macro undefined" errors downstream. We now render each kind
    // with its payload so round-tripping inside an `%if` body works
    // uniformly.
    fn flatten_text(t: &rpm_spec::ast::Text) -> String {
        // Lossy renderer good enough for re-scanning: macro segments
        // get re-prefixed with `%{name}` (we don't need to round-trip
        // perfectly, just produce a re-scannable byte sequence). For
        // depth-1 args this is equivalent to literal_str when there
        // are no nested macros.
        let mut out = String::new();
        for seg in &t.segments {
            match seg {
                TextSegment::Literal(s) => out.push_str(s),
                TextSegment::Macro(inner) => out.push_str(&macro_ref_to_source(inner)),
                _ => {}
            }
        }
        out
    }
    let prefix = match mr.conditional {
        ConditionalMacro::IfDefined => "?",
        ConditionalMacro::IfNotDefined => "!?",
        ConditionalMacro::None => "",
        _ => "",
    };
    match &mr.kind {
        MacroKind::Plain => format!("%{}", mr.name),
        MacroKind::Braced => {
            // `%{name}` / `%{?name}` / `%{!?name}`; optional with_value
            // body `%{?name:default}` rebuilt from `with_value` Text.
            match &mr.with_value {
                Some(body) => format!("%{{{prefix}{}:{}}}", mr.name, flatten_text(body)),
                None => format!("%{{{prefix}{}}}", mr.name),
            }
        }
        MacroKind::Parametric => {
            // `%{name arg1 arg2 …}`; whitespace-separated args.
            let args: Vec<String> = mr.args.iter().map(flatten_text).collect();
            if args.is_empty() {
                format!("%{{{prefix}{}}}", mr.name)
            } else {
                format!("%{{{prefix}{} {}}}", mr.name, args.join(" "))
            }
        }
        MacroKind::Shell => {
            // `%(body)` — `name` is empty, body is `args[0]`.
            let body = mr.args.first().map(flatten_text).unwrap_or_default();
            format!("%({body})")
        }
        MacroKind::Expr => {
            // Two surface forms: `%[expr]` and `%{expr:body}`. The
            // parser uses the latter when there is a name; the
            // former when name is empty (the `%[…]` shape stores no
            // keyword).
            let body = mr.args.first().map(flatten_text).unwrap_or_default();
            if mr.name.is_empty() {
                format!("%[{body}]")
            } else {
                format!("%{{{}:{body}}}", mr.name)
            }
        }
        MacroKind::Lua => {
            let body = mr.args.first().map(flatten_text).unwrap_or_default();
            let kw = if mr.name.is_empty() {
                "lua"
            } else {
                mr.name.as_str()
            };
            format!("%{{{kw}:{body}}}")
        }
        // `With` / `Without` use a SPACE separator and store the
        // feature name in `args[0]`. The conditional prefix is
        // included for symmetry — even though the parser does not
        // (yet) promote prefixed forms, anyone constructing
        // `Builtin(With)` programmatically with `conditional != None`
        // needs the right round-trip.
        MacroKind::Builtin(BuiltinMacro::With) => {
            let name = mr.args.first().map(flatten_text).unwrap_or_default();
            format!("%{{{prefix}with {name}}}")
        }
        MacroKind::Builtin(BuiltinMacro::Without) => {
            let name = mr.args.first().map(flatten_text).unwrap_or_default();
            format!("%{{{prefix}without {name}}}")
        }
        MacroKind::Builtin(_) => {
            // Generic builtins: `%{keyword:body}`. Name carries the
            // keyword (e.g. "shrink" / "quote" / "gsub").
            let body = mr.args.first().map(flatten_text).unwrap_or_default();
            format!("%{{{}:{body}}}", mr.name)
        }
        // `MacroKind` is `#[non_exhaustive]` upstream; force the
        // compiler to remind us when a new variant lands.
        _ => format!("%{{{}}}", mr.name),
    }
}

/// Scan-and-substitute macro references in a raw `%if` body. Uses
/// the profile crate's macro lexer so the rules (`%{name}` vs
/// `%{?name}` vs `%name`, default-value form) match what rpm does.
fn expand_raw_string(
    text: &str,
    macros: &MacroRegistry,
    bcond: &crate::bcond::BcondMap,
) -> Result<String, EvalError> {
    use rpm_spec_profile::macro_lexer::{Conditional, MacroKind as LexKind, scan_macro_ref};
    if !text.is_ascii() {
        return Err(EvalError::NonAscii);
    }
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(char::from(bytes[i]));
            i += 1;
            continue;
        }
        let r =
            scan_macro_ref(bytes, i).ok_or(EvalError::Unsupported("malformed macro reference"))?;
        // Bcond fast-path: `%{with NAME}` / `%{without NAME}` are
        // RPM built-ins resolving to 0/1 from the spec's BcondMap,
        // NOT regular profile-macro lookups. Detect from the full
        // source range so we see the arg the macro lexer dropped.
        // `resolve_bcond` shares the policy with the Parsed-AST path.
        let full = &text[r.full_range.clone()];
        if let Some(result) = resolve_bcond(full, bcond) {
            let val = result?;
            out.push_str(&val.to_string());
            i = r.full_range.end;
            continue;
        }
        match r.kind {
            LexKind::LiteralPercent => out.push('%'),
            LexKind::Plain => {
                let v = macros
                    .expand_to_literal(r.name, EXPAND_DEPTH)
                    .ok_or_else(|| EvalError::UndefinedMacro(r.name.to_string()))?;
                out.push_str(&v);
            }
            LexKind::Braced {
                conditional: None,
                has_default: false,
            } => {
                let v = macros
                    .expand_to_literal(r.name, EXPAND_DEPTH)
                    .ok_or_else(|| EvalError::UndefinedMacro(r.name.to_string()))?;
                out.push_str(&v);
            }
            LexKind::Braced {
                conditional: Some(Conditional::IfDefined),
                has_default,
            } => {
                if macros.get(r.name).is_some() {
                    let v = macros
                        .expand_to_literal(r.name, EXPAND_DEPTH)
                        .ok_or_else(|| EvalError::UndefinedMacro(r.name.to_string()))?;
                    out.push_str(&v);
                } else if has_default {
                    // `%{?name:default}` would substitute the default
                    // body when name is undefined; we don't model
                    // default bodies, so surface the gap rather than
                    // emitting empty.
                    return Err(EvalError::UnmodelledDefault);
                }
                // Undefined without default → contributes empty string.
            }
            LexKind::Braced {
                conditional: Some(Conditional::IfUndefined),
                has_default,
            } => {
                if has_default {
                    return Err(EvalError::UnmodelledDefault);
                }
                // Bare `%{!?name}` — rpm emits the empty string in
                // both branches when there's no default body.
                let _ = macros.get(r.name);
            }
            LexKind::Braced {
                conditional: None,
                has_default: true,
            } => {
                return Err(EvalError::UnmodelledDefault);
            }
            LexKind::ShellExpansion => return Err(EvalError::ShellExpansion),
            LexKind::ArithmeticExpr => return Err(EvalError::ArithmeticExpr),
        }
        i = r.full_range.end;
    }
    Ok(out)
}

fn expand_text(t: &Text, macros: &MacroRegistry) -> Result<String, EvalError> {
    let mut out = String::new();
    for seg in &t.segments {
        match seg {
            TextSegment::Literal(s) => out.push_str(s),
            TextSegment::Macro(mr) => {
                let defined = macros.get(&mr.name).is_some();
                use rpm_spec::ast::ConditionalMacro;
                match mr.conditional {
                    ConditionalMacro::IfDefined => {
                        if defined {
                            let v = macros
                                .expand_to_literal(&mr.name, EXPAND_DEPTH)
                                .ok_or_else(|| EvalError::UndefinedMacro(mr.name.clone()))?;
                            out.push_str(&v);
                        }
                    }
                    ConditionalMacro::IfNotDefined => {
                        if !defined {
                            if let Some(wv) = mr.with_value.as_ref() {
                                let expanded = expand_text(wv, macros)?;
                                out.push_str(&expanded);
                            }
                        }
                    }
                    ConditionalMacro::None => {
                        let v = macros
                            .expand_to_literal(&mr.name, EXPAND_DEPTH)
                            .ok_or_else(|| EvalError::UndefinedMacro(mr.name.clone()))?;
                        out.push_str(&v);
                    }
                    // `ConditionalMacro` is `#[non_exhaustive]`
                    // upstream — surface unmodelled variants rather
                    // than guessing semantics.
                    _ => {
                        return Err(EvalError::Unsupported("unknown ConditionalMacro variant"));
                    }
                }
            }
            _ => return Err(EvalError::Unsupported("unknown TextSegment variant")),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_profile::{ProfileSection, ResolveOptions, TargetEntry, resolve_target_set};
    use std::path::Path;

    fn resolved(profiles: &[&str]) -> ResolvedTargetSet {
        let section = ProfileSection::default();
        let target =
            TargetEntry::from_profiles(profiles.iter().map(|s| (*s).to_string()).collect());
        resolve_target_set(
            &section,
            "T",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap()
    }

    fn parse(src: &str) -> rpm_spec::ast::SpecFile<Span> {
        crate::session::parse(src).spec
    }

    /// Spec template — preamble + body + changelog. Conditionals
    /// placed BETWEEN preamble end and `%description` are top-level
    /// (parser yields `SpecItem::Conditional`).
    fn spec_with_conditional(condition: &str) -> rpm_spec::ast::SpecFile<Span> {
        let src = format!(
            "Name: foo\n\
Version: 1\n\
Release: 1\n\
Summary: S\n\
License: MIT\n\
\n\
{condition}\n\
%global flag 1\n\
%endif\n\
\n\
%description\n\
B\n\
\n\
%changelog\n\
* Mon Jan 01 2024 a <a@b> - 1-1\n\
- init\n"
        );
        parse(&src)
    }

    #[test]
    fn ifarch_matches_target_build_arch() {
        let set = resolved(&["rhel-9-x86_64", "rhel-9-aarch64"]);
        let spec = spec_with_conditional("%ifarch x86_64");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        assert_eq!(report.conditionals.len(), 1, "got {report:#?}");
        let b = &report.conditionals[0].branches[0];
        assert_eq!(b.active_on, vec!["rhel-9-x86_64"]);
        assert_eq!(b.inactive_on, vec!["rhel-9-aarch64"]);
        assert!(b.indeterminate_on.is_empty());
    }

    #[test]
    fn ifnarch_negates_match() {
        let set = resolved(&["rhel-9-x86_64", "rhel-9-aarch64"]);
        let spec = spec_with_conditional("%ifnarch x86_64");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        assert_eq!(b.active_on, vec!["rhel-9-aarch64"]);
        assert_eq!(b.inactive_on, vec!["rhel-9-x86_64"]);
    }

    #[test]
    fn if_with_conditional_macro_ref_evaluates_to_active_on_rhel() {
        // `%if 0%{?rhel}` — true when rhel macro defined, false
        // otherwise. rhel-9-x86_64 has it via the bundled showrc;
        // generic profile (no showrc) does not.
        let set = resolved(&["generic", "rhel-9-x86_64"]);
        let spec = spec_with_conditional("%if 0%{?rhel}");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        assert!(
            b.active_on.contains(&"rhel-9-x86_64".to_string()),
            "expected active on rhel; got {b:?}"
        );
        assert!(
            b.inactive_on.contains(&"generic".to_string()),
            "expected inactive on generic; got {b:?}"
        );
    }

    #[test]
    fn unevaluable_condition_reports_indeterminate() {
        // `%if %{this_macro_is_undefined_xyz}` — undefined macro
        // can't be expanded unconditionally; evaluator must report
        // indeterminate, not silently false.
        let set = resolved(&["generic"]);
        let spec = spec_with_conditional("%if %{this_macro_is_undefined_xyz}");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        assert!(
            b.indeterminate_on.contains(&"generic".to_string()),
            "expected indeterminate; got {b:?}"
        );
    }

    #[test]
    fn dead_branch_marked() {
        let set = resolved(&["generic"]);
        let spec = spec_with_conditional("%if 0");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        assert!(report.conditionals[0].branches[0].is_dead());
        assert_eq!(report.dead_branches(), 1);
    }

    #[test]
    fn elif_chain_evaluates_each_branch_independently() {
        // `%if a … %elif b … %else …`: collector records both head
        // and elif as separate branches in `branches`. Coverage
        // reports the per-branch activity without modelling "earlier
        // branch was already active" — that's intentional, mirrors
        // how rpm itself evaluates each elif.
        let set = resolved(&["rhel-9-x86_64"]);
        let src = "Name: foo\n\
Version: 1\n\
Release: 1\n\
Summary: S\n\
License: MIT\n\
\n\
%if 0\n\
%global a 1\n\
%elif 1\n\
%global b 2\n\
%else\n\
%global c 3\n\
%endif\n\
\n\
%description\n\
B\n\
\n\
%changelog\n\
* Mon Jan 01 2024 a <a@b> - 1-1\n\
- init\n";
        let spec = parse(src);
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        assert_eq!(report.conditionals.len(), 1);
        // Two branches: `%if 0` (dead) and `%elif 1` (active).
        assert_eq!(report.conditionals[0].branches.len(), 2);
        assert!(report.conditionals[0].branches[0].is_dead());
        assert_eq!(
            report.conditionals[0].branches[1].active_on,
            vec!["rhel-9-x86_64"]
        );
    }

    #[test]
    fn ifos_branch_categorised_per_profile() {
        // `%ifos linux` is plumbed through the same evaluator path
        // as `%ifarch`. For profiles whose bundled showrc didn't
        // expose `build_os` the evaluator must return Indeterminate
        // — never silently false. We assert classification only,
        // not the active set, because not all bundled profiles
        // populate `build_os` and the test would otherwise be brittle.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional("%ifos linux");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        let total = b.active_on.len() + b.inactive_on.len() + b.indeterminate_on.len();
        assert_eq!(
            total, 1,
            "every profile must be classified exactly once: {b:?}"
        );
    }

    #[test]
    fn binary_ops_evaluate_correctly() {
        // Exercise all numeric BinOp variants (Eq, Ne, Lt, Gt, Le,
        // Ge, LogAnd, LogOr) through `%if` expressions. Each is
        // wrapped in a separate %if so we get one branch per op.
        let set = resolved(&["rhel-9-x86_64"]);
        let src = "Name: foo\n\
Version: 1\n\
Release: 1\n\
Summary: S\n\
License: MIT\n\
\n\
%if 1 == 1\n\
%global eq 1\n\
%endif\n\
%if 1 != 2\n\
%global ne 1\n\
%endif\n\
%if 1 < 2\n\
%global lt 1\n\
%endif\n\
%if 2 > 1\n\
%global gt 1\n\
%endif\n\
%if 2 <= 2\n\
%global le 1\n\
%endif\n\
%if 2 >= 2\n\
%global ge 1\n\
%endif\n\
%if 1 && 1\n\
%global and 1\n\
%endif\n\
%if 0 || 1\n\
%global or 1\n\
%endif\n\
\n\
%description\n\
B\n\
\n\
%changelog\n\
* Mon Jan 01 2024 a <a@b> - 1-1\n\
- init\n";
        let spec = parse(src);
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        assert_eq!(report.conditionals.len(), 8, "got {report:#?}");
        for c in &report.conditionals {
            assert_eq!(
                c.branches[0].active_on,
                vec!["rhel-9-x86_64"],
                "branch should be active: {}",
                c.branches[0].branch.display
            );
        }
    }

    #[test]
    fn bare_if_not_defined_macro_resolves_to_empty() {
        // Regression: previously `%if 0%{!?undefined_xyz}` was
        // reported as Indeterminate because expand_raw_string
        // bailed on undefined. After the fix, bare `%{!?name}`
        // (no default body) emits the empty string regardless of
        // definedness — so the condition is `"0"` → Inactive.
        let set = resolved(&["generic"]);
        let spec = spec_with_conditional("%if 0%{!?surely_undefined_xyz}");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        assert_eq!(
            b.inactive_on,
            vec!["generic"],
            "bare %{{!?undefined}} must resolve to empty, leaving `0` ⇒ Inactive; got {b:?}"
        );
        assert!(b.indeterminate_on.is_empty());
    }

    #[test]
    fn collector_records_else_presence() {
        let set = resolved(&["generic"]);
        let src = "Name: foo\n\
Version: 1\n\
Release: 1\n\
Summary: S\n\
License: MIT\n\
\n\
%if 0\n\
%global a 1\n\
%else\n\
%global b 2\n\
%endif\n\
\n\
%description\n\
B\n\
\n\
%changelog\n\
* Mon Jan 01 2024 a <a@b> - 1-1\n\
- init\n";
        let spec = parse(src);
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        assert!(report.conditionals[0].has_else);
    }

    #[test]
    fn parsed_expr_macro_returns_real_value_not_zero_one() {
        // Regression: earlier the evaluator returned 0/1 for
        // `%{?rhel}` instead of the macro value, so `%if %{?rhel}
        // >= 9` mis-evaluated as `1 >= 9 = false` on RHEL profiles.
        // After the fix, %{?rhel} expands to its value when defined.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional("%if %{?rhel} >= 9");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        assert_eq!(
            report.conditionals[0].branches[0].active_on,
            vec!["rhel-9-x86_64"],
            "expected `%if %{{?rhel}} >= 9` active on RHEL 9"
        );
    }

    fn resolved_with_defines(profiles: &[&str], defines: &[&str]) -> ResolvedTargetSet {
        let section = ProfileSection::default();
        let target =
            TargetEntry::from_profiles(profiles.iter().map(|s| (*s).to_string()).collect());
        let defines_owned: Vec<String> = defines.iter().map(|s| (*s).to_string()).collect();
        resolve_target_set(
            &section,
            "T",
            &target,
            Path::new("."),
            ResolveOptions::default().with_defines(&defines_owned),
        )
        .unwrap()
    }

    #[test]
    fn string_literal_with_macro_ref_is_expanded_before_comparison() {
        // Regression for a real-world false-positive: `%if "%{flavour}"
        // == "ent"` with `-D flavour ent` used to mis-evaluate to DEAD
        // because `ExprAst::String` returned the raw source
        // `%{flavour}` rather than its expanded value. Cover braced
        // (%{flavour}) and unbraced (%flavour) plus a disjunction — the
        // parser emits the same `ExprAst::String` shape for each, so a
        // single fix covers them all.
        for cond in [
            r#"%if "%{flavour}" == "ent""#,
            r#"%if "%flavour" == "ent""#,
            r#"%if "%{flavour}" == "ent" || "%{flavour}" == "premium""#,
        ] {
            let set = resolved_with_defines(&["rhel-9-x86_64"], &["flavour ent"]);
            let spec = spec_with_conditional(cond);
            let report =
                CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
            assert_eq!(
                report.conditionals[0].branches[0].active_on,
                vec!["rhel-9-x86_64"],
                "expected `{cond}` active on rhel-9 when flavour=ent"
            );
        }
    }

    #[test]
    fn string_literal_with_optional_macro_ref_compares_empty_when_undefined() {
        // `%{?flavour}` is the conditional-defined form: empty when
        // undefined, expanded value when defined. `%if "%{?flavour}"
        // == "ent"` with no `-D flavour` must therefore compare an
        // empty string against `"ent"` and be Inactive — NOT
        // Indeterminate. A future refactor that made `expand_raw_string`
        // error on undefined `%{?name}` (instead of producing `""`)
        // would silently flip every realistic spec to indeterminate.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional(r#"%if "%{?flavour}" == "ent""#);
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        assert_eq!(
            b.inactive_on,
            vec!["rhel-9-x86_64"],
            "expected `%if \"%{{?flavour}}\" == \"ent\"` inactive when flavour undefined; got {b:?}"
        );
        assert!(
            b.indeterminate_on.is_empty(),
            "undefined `%{{?}}` form must NOT surface as indeterminate; got {b:?}"
        );
    }

    #[test]
    fn string_literal_with_malformed_macro_ref_is_indeterminate() {
        // A literal `"%{flavour"` (no closing brace) cannot be parsed
        // as a macro reference. The evaluator falls into
        // `EvalError::Unsupported("malformed macro reference")` and the
        // branch surfaces as indeterminate rather than crashing. Pin
        // both the classification and the fact that evaluation does
        // not panic — a regression here would take down `matrix
        // coverage` on any spec with a typo.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional(r#"%if "%{flavour" == "ent""#);
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        assert_eq!(
            b.indeterminate_on,
            vec!["rhel-9-x86_64"],
            "malformed macro inside %if string must be indeterminate; got {b:?}"
        );
    }

    #[test]
    fn ifarch_match_is_order_independent() {
        // Regression: an unevaluable macro before the matching arch
        // used to short-circuit to Indeterminate. After the fix,
        // any literal match wins regardless of position.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec_before = spec_with_conditional("%ifarch %{undefined_arch_xyz} x86_64");
        let spec_after = spec_with_conditional("%ifarch x86_64 %{undefined_arch_xyz}");
        let r1 =
            CoverageReport::compute(&spec_before, &set, &crate::bcond::BcondOverrides::default());
        let r2 =
            CoverageReport::compute(&spec_after, &set, &crate::bcond::BcondOverrides::default());
        assert_eq!(
            r1.conditionals[0].branches[0].active_on, r2.conditionals[0].branches[0].active_on,
            "ifarch evaluation must not depend on token order"
        );
        // And both should resolve to Active (x86_64 matches).
        assert_eq!(
            r1.conditionals[0].branches[0].active_on,
            vec!["rhel-9-x86_64"]
        );
    }

    #[test]
    fn ifarch_matches_subarch_via_compatible_archs() {
        // altlinux-10-e2k has `build_arch=e2kv4` (the strictest)
        // and `compatible_archs=[e2kv4, e2k, noarch]`. With the
        // pre-fix evaluator `%ifarch e2k` was Inactive (build_arch
        // didn't equal e2k). After the switch to compatible_archs,
        // e2k matches because the host can also build subarch e2k.
        let set = resolved(&["altlinux-10-e2k"]);
        let spec = spec_with_conditional("%ifarch e2k");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        assert_eq!(
            b.active_on,
            vec!["altlinux-10-e2k"],
            "e2k must match via compatible_archs on e2kv4 host; got {b:?}"
        );
    }

    #[test]
    fn ifarch_noarch_matches_every_distro_profile() {
        // `noarch` is in every distro profile's compatible_archs.
        let set = resolved(&["rhel-9-x86_64", "rhel-9-aarch64"]);
        let spec = spec_with_conditional("%ifarch noarch");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        assert_eq!(
            b.active_on.len(),
            2,
            "noarch must match both profiles: {b:?}"
        );
    }

    #[test]
    fn ifarch_falls_back_to_build_arch_when_compat_empty() {
        // `generic` profile has no showrc bundle, so compatible_archs
        // is empty — must fall back to build_arch (also empty).
        // Result: MissingArchOrOs error → Indeterminate with
        // diagnostic reason.
        let set = resolved(&["generic"]);
        let spec = spec_with_conditional("%ifarch x86_64");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        assert_eq!(b.indeterminate_on, vec!["generic"]);
        let reason = b
            .indeterminate_reasons
            .get("generic")
            .expect("reason recorded");
        // Variant check belt-and-braces with the Display assertion so
        // a future Display-string tweak doesn't silently change the
        // surface meaning.
        assert!(
            matches!(reason, EvalError::MissingBuildArch),
            "expected MissingBuildArch, got {reason:?}"
        );
        let rendered = reason.to_string();
        assert!(
            rendered.contains("build_arch"),
            "reason should mention missing build_arch: {rendered}"
        );
    }

    #[test]
    fn indeterminate_reasons_populated_for_undefined_unconditional() {
        // `%if %{undefined_macro}` (no `?` prefix) — evaluator must
        // record UndefinedMacro reason and surface it through
        // indeterminate_reasons.
        let set = resolved(&["generic"]);
        let spec = spec_with_conditional("%if %{this_macro_is_not_defined}");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        assert_eq!(b.indeterminate_on, vec!["generic"]);
        let reason = b.indeterminate_reasons.get("generic").unwrap();
        assert!(
            matches!(reason, EvalError::UndefinedMacro(n) if n == "this_macro_is_not_defined"),
            "expected UndefinedMacro(this_macro_is_not_defined), got {reason:?}"
        );
        let rendered = reason.to_string();
        assert!(
            rendered.contains("undefined macro") && rendered.contains("this_macro_is_not_defined"),
            "reason should name the undefined macro: {rendered}"
        );
        assert_eq!(reason.category(), EvalErrorCategory::Config);
        assert_eq!(reason.code(), "undefined-macro");
    }

    #[test]
    fn indeterminate_reasons_record_unmodelled_default() {
        // `%{!?name:fallback}` is unmodelled — evaluator surfaces
        // `UnmodelledDefault` rather than guessing.
        let set = resolved(&["generic"]);
        let spec = spec_with_conditional("%if 0%{!?undefined:fallback}");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        let reason = b
            .indeterminate_reasons
            .get("generic")
            .expect("reason recorded");
        assert!(
            matches!(reason, EvalError::UnmodelledDefault),
            "expected UnmodelledDefault, got {reason:?}"
        );
        let rendered = reason.to_string();
        assert!(
            rendered.contains("default-value"),
            "reason should mention default-value: {rendered}"
        );
        assert_eq!(reason.category(), EvalErrorCategory::Tool);
        assert_eq!(reason.code(), "default-value-form");
    }

    #[test]
    fn is_universally_active_self_contained() {
        // No total_profiles parameter needed — the invariant
        // `active + inactive + indeterminate == total` makes the
        // check derivable from the BranchCoverage alone.
        let set = resolved(&["rhel-9-x86_64", "rhel-9-aarch64"]);
        let spec = spec_with_conditional("%if 1");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        assert!(report.conditionals[0].branches[0].is_universally_active());
    }

    #[test]
    fn totals_helper_methods() {
        let set = resolved(&["generic", "rhel-9-x86_64"]);
        let src = "Name: foo\n\
Version: 1\n\
Release: 1\n\
Summary: S\n\
License: MIT\n\
\n\
%if 0\n\
%global dead 1\n\
%endif\n\
\n\
%if %{undefined_xyz}\n\
%global indet 1\n\
%endif\n\
\n\
%ifarch x86_64\n\
%global arch 1\n\
%endif\n\
\n\
%description\n\
B\n\
\n\
%changelog\n\
* Mon Jan 01 2024 a <a@b> - 1-1\n\
- init\n";
        let spec = parse(src);
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        assert_eq!(report.total_branches(), 3);
        // `%if 0` is dead on every profile.
        assert_eq!(report.dead_branches(), 1);
        // `%if %{undefined_xyz}` is indeterminate everywhere;
        // `%ifarch x86_64` is indeterminate on `generic` (no
        // build_arch). Both contribute.
        assert_eq!(report.indeterminate_branches(), 2);
    }

    // -----------------------------------------------------------------
    // Macro variants (Phase B) — `compute_with_variants` exercises the
    // CONDITIONAL classification on declared `[macros.NAME]` value sets.
    // -----------------------------------------------------------------

    fn variants(pairs: &[(&str, &[&str])]) -> BTreeMap<String, rpm_spec_profile::MacroVariants> {
        pairs
            .iter()
            .map(|(name, values)| {
                (
                    (*name).to_string(),
                    rpm_spec_profile::MacroVariants::new(values.iter().copied().map(String::from)),
                )
            })
            .collect()
    }

    #[test]
    fn conditional_when_some_variant_value_activates_branch() {
        // `%if "%{flavour}" == "1c"` is inactive on every profile
        // under the default macro registry (no `flavour` defined).
        // Declaring `flavour ∈ {ent, std, 1c}` lets the analyzer
        // re-evaluate with `flavour=1c` and mark the branch
        // CONDITIONAL — not DEAD — because the project explicitly
        // declared 1c as a buildable variant.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional(r#"%if "%{flavour}" == "1c""#);
        let vmap = variants(&[("flavour", &["ent", "std", "1c"])]);
        let report = CoverageReport::compute_with_variants(
            &spec,
            &set,
            &crate::bcond::BcondOverrides::default(),
            &vmap,
        );
        let b = &report.conditionals[0].branches[0];
        assert!(!b.is_dead(), "must not be DEAD when a variant activates");
        assert!(b.is_conditional(), "should classify as conditional");
        assert_eq!(b.conditional_on, vec!["rhel-9-x86_64"]);
        let flavour_values = b.reachable_under.get("flavour").expect("flavour key");
        assert!(
            flavour_values.contains("1c"),
            "expected 1c in reachable_under; got {flavour_values:?}"
        );
    }

    #[test]
    fn conditional_strips_indeterminate_for_macros_with_declared_variants() {
        // A profile-level indeterminacy whose root cause is "the
        // variant macro itself is undefined" gets resolved by the
        // variant declaration — the operator told the tool which
        // values the macro takes, so reporting the macro as
        // undefined is now incorrect. The renderer used to show
        // both `[CONDITIONAL: pgsql_major=13]` AND `indeterminate:
        // undefined macro: pgsql_major`, contradicting each other.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional("%if %{pgsql_major} >= 12");
        let vmap = variants(&[("pgsql_major", &["13", "14", "15"])]);
        let report = CoverageReport::compute_with_variants(
            &spec,
            &set,
            &crate::bcond::BcondOverrides::default(),
            &vmap,
        );
        let b = &report.conditionals[0].branches[0];
        assert!(b.is_conditional(), "should classify as conditional");
        assert!(
            b.indeterminate_on.is_empty(),
            "indeterminate verdict must be stripped once the variant resolves it; got {:?}",
            b.indeterminate_on
        );
        assert!(
            b.indeterminate_reasons.is_empty(),
            "indeterminate reasons must be stripped for declared-variant macros; got {:?}",
            b.indeterminate_reasons
        );
    }

    #[test]
    fn variant_exhaustion_demotes_indeterminate_to_inactive_or_dead() {
        // `%if %{pgsql_major} < 13` with `pgsql_major ∈ {13,...,18}`
        // declared: NO declared value makes the comparison true. The
        // base evaluator says `undefined macro: pgsql_major` →
        // indeterminate; the variant pass exhausts all 6 values and
        // finds Ok(false) for every one. The branch is provably
        // dead under the declared variant matrix and must be tagged
        // [DEAD], not [INDET] (which would tell the operator
        // "declare a variant" — they already did).
        //
        // This is the user-reported fix: `undefined-macro` reason
        // was scary on a macro that's actually declared in
        // `[macros.pgsql_major]`.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional("%if %{pgsql_major} < 13");
        let vmap = variants(&[("pgsql_major", &["13", "14", "15", "16", "17", "18"])]);
        let report = CoverageReport::compute_with_variants(
            &spec,
            &set,
            &crate::bcond::BcondOverrides::default(),
            &vmap,
        );
        let b = &report.conditionals[0].branches[0];
        assert!(
            b.is_dead(),
            "branch must be DEAD: every declared variant value gives false; got active={:?} \
             inactive={:?} indeterminate={:?} conditional={:?}",
            b.active_on,
            b.inactive_on,
            b.indeterminate_on,
            b.conditional_on
        );
        assert_eq!(b.inactive_on, vec!["rhel-9-x86_64"], "profile must be inactive");
        assert!(
            b.indeterminate_on.is_empty(),
            "indeterminate must be empty after variant exhaustion"
        );
        assert!(
            b.indeterminate_reasons.is_empty(),
            "indeterminate_reasons must be cleared for demoted profiles"
        );
    }

    #[test]
    fn variant_exhaustion_does_not_demote_when_some_combo_errors() {
        // `%if %{declared} < 13 && %{undeclared}`: variant pass
        // tries declared values 13..18 — each gives Ok(false) for
        // `< 13`, but the `&& %{undeclared}` half makes the overall
        // eval error out (UndefinedMacro on `undeclared`). At least
        // one Err blocks the demotion: profile stays indeterminate
        // because the truth value of the branch under any variant
        // remains unknowable (we know `< 13` is false on each combo,
        // but the && wins false short-circuit on the left, so
        // evaluator never reaches the right side and returns false?
        // No — actually `&&` short-circuits, so `false && undefined`
        // is Ok(false). So in this case had_err stays empty and
        // demotion fires. To genuinely block demotion we need an
        // OR: `(declared >= 13) || undeclared` where left is true
        // hits short-circuit, but the alt path uses `pgsql > 13`
        // and `||` requires reaching the right side. Use that form.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional("%if %{declared} > 13 || %{undeclared}");
        let vmap = variants(&[("declared", &["13"])]);
        let report = CoverageReport::compute_with_variants(
            &spec,
            &set,
            &crate::bcond::BcondOverrides::default(),
            &vmap,
        );
        let b = &report.conditionals[0].branches[0];
        // `declared=13` → `13 > 13` = false → `||` reaches
        // `%{undeclared}` → Err(UndefinedMacro). Had_err set, no
        // demotion. Profile stays indeterminate.
        assert!(
            !b.is_dead(),
            "branch must not be DEAD when at least one variant combo errors"
        );
        assert_eq!(
            b.indeterminate_on,
            vec!["rhel-9-x86_64"],
            "profile must stay indeterminate"
        );
    }

    #[test]
    fn conditional_keeps_indeterminate_for_non_rescued_profiles() {
        // When the variant-rescue pass fails to activate the branch
        // on some profile (because another undeclared macro blocks
        // resolution), that profile must KEEP its base-evaluation
        // indeterminate verdict — including the reason naming the
        // declared-variant macro, because the strip step only runs
        // for profiles actually promoted into `conditional_on`.
        //
        // Without this guarantee, a stray variant declaration could
        // silently hide indeterminacy on unrelated branches.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional("%if %{declared_macro} && %{undeclared_macro}");
        let vmap = variants(&[("declared_macro", &["1"])]);
        let report = CoverageReport::compute_with_variants(
            &spec,
            &set,
            &crate::bcond::BcondOverrides::default(),
            &vmap,
        );
        let b = &report.conditionals[0].branches[0];
        // Even with declared_macro=1, `undeclared_macro` is still
        // undefined → variant pass produces Err → branch is not
        // conditional. Profile stays in indeterminate_on with its
        // original short-circuit reason intact.
        assert!(
            !b.is_conditional(),
            "branch must not be conditional when an undeclared macro blocks resolution; got {:?}",
            b.reachable_under
        );
        assert_eq!(
            b.indeterminate_on,
            vec!["rhel-9-x86_64"],
            "non-rescued profile must stay indeterminate"
        );
        // The evaluator short-circuits on the first failure
        // (`declared_macro`), so that's the recorded reason. Strip
        // step skipped it because the profile wasn't promoted. The
        // invariant: strip only touches reasons on rescued
        // profiles, never on non-rescued ones.
        let reason = b.indeterminate_reasons.get("rhel-9-x86_64").unwrap();
        assert!(
            matches!(reason, EvalError::UndefinedMacro(_)),
            "expected reason preserved verbatim; got {reason:?}"
        );
    }

    #[test]
    fn dead_when_no_declared_variant_activates_branch() {
        // `%if "%{?flavour}" == "spices"` is inactive under every
        // declared variant value (ent/std/1c, no "spices") AND
        // inactive under the base profile (the `?` form makes
        // undefined flavour expand to empty string). Verifies the
        // analyzer doesn't promote every unrecognised branch to
        // CONDITIONAL — only those that ACTUALLY activate under
        // some declared variant.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional(r#"%if "%{?flavour}" == "spices""#);
        let vmap = variants(&[("flavour", &["ent", "std", "1c"])]);
        let report = CoverageReport::compute_with_variants(
            &spec,
            &set,
            &crate::bcond::BcondOverrides::default(),
            &vmap,
        );
        let b = &report.conditionals[0].branches[0];
        assert!(b.is_dead(), "must stay DEAD: no variant value activates `spices`");
        assert!(!b.is_conditional());
        assert!(b.conditional_on.is_empty());
    }

    #[test]
    fn conditional_rescues_indeterminate_undefined_macro() {
        // `%if %{pgsql_major} == 13` is indeterminate under a profile
        // where `pgsql_major` is undefined. Declaring variants for
        // it (effectively saying "the project builds for pgsql_major
        // ∈ {13, 14, 15}") promotes the branch to CONDITIONAL with
        // value 13 — without variants, operators would see
        // "indeterminate" forever and not know whether the spec is
        // healthy or broken.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional("%if %{pgsql_major} == 13");
        let vmap = variants(&[("pgsql_major", &["13", "14", "15"])]);
        let report = CoverageReport::compute_with_variants(
            &spec,
            &set,
            &crate::bcond::BcondOverrides::default(),
            &vmap,
        );
        let b = &report.conditionals[0].branches[0];
        assert!(b.is_conditional(), "indeterminate → conditional with variants");
        let values = b.reachable_under.get("pgsql_major").expect("pgsql_major key");
        assert!(values.contains("13"), "expected 13 in values; got {values:?}");
    }

    #[test]
    fn no_variants_loaded_means_classic_dead_classification() {
        // Empty variant map → no augmentation. Verifies the
        // augmentation is opt-in (consumers that don't care about
        // CONDITIONAL pay nothing for it) and the existing dead-code
        // semantics still hold. Use `%{?flavour}` (conditional-defined)
        // so the base evaluator produces a clean Inactive verdict
        // (empty string) rather than Indeterminate (UndefinedMacro).
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional(r#"%if "%{?flavour}" == "1c""#);
        let empty = variants(&[]);
        let report = CoverageReport::compute_with_variants(
            &spec,
            &set,
            &crate::bcond::BcondOverrides::default(),
            &empty,
        );
        let b = &report.conditionals[0].branches[0];
        assert!(b.is_dead());
        assert!(!b.is_conditional());
    }

    #[test]
    fn variant_cartesian_product_respects_cap() {
        // Declare more variant combinations than MAX_VARIANT_COMBINATIONS
        // and confirm the augmenter bails (leaving classic DEAD)
        // rather than spinning. 4 macros × 4 values each = 256 > 64.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional(
            r#"%if "%{a}" == "x" || "%{b}" == "x" || "%{c}" == "x" || "%{d}" == "x""#,
        );
        let values: &[&str] = &["1", "2", "3", "4"];
        let vmap = variants(&[("a", values), ("b", values), ("c", values), ("d", values)]);
        let report = CoverageReport::compute_with_variants(
            &spec,
            &set,
            &crate::bcond::BcondOverrides::default(),
            &vmap,
        );
        let b = &report.conditionals[0].branches[0];
        assert!(
            !b.is_conditional(),
            "cartesian cap must prevent variant augmentation; got conditional with {:?}",
            b.reachable_under
        );
    }

    #[test]
    fn variant_irrelevant_to_condition_is_ignored() {
        // Declaring variants for a macro NOT referenced by the
        // branch's condition must not trigger augmentation — keeps
        // the cartesian product scoped to the relevant variables and
        // prevents output noise. `flavour` is declared; the branch
        // tests `other_macro`.
        let set = resolved(&["rhel-9-x86_64"]);
        let spec = spec_with_conditional(r#"%if "%{other_macro}" == "x""#);
        let vmap = variants(&[("flavour", &["ent", "std", "1c"])]);
        let report = CoverageReport::compute_with_variants(
            &spec,
            &set,
            &crate::bcond::BcondOverrides::default(),
            &vmap,
        );
        let b = &report.conditionals[0].branches[0];
        assert!(
            !b.is_conditional(),
            "irrelevant variant must not augment; got {:?}",
            b.reachable_under
        );
    }

    // -----------------------------------------------------------------
    // Per-variant EvalError coverage
    // -----------------------------------------------------------------

    /// Non-ASCII bytes in the condition body force the byte-cursor
    /// scanner to bail (it would otherwise emit mojibake).
    #[test]
    fn evalerror_nonascii_recorded() {
        let set = resolved(&["generic"]);
        let spec = spec_with_conditional("%if 0%{?\u{0442}\u{0435}\u{0441}\u{0442}}");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        let reason = b
            .indeterminate_reasons
            .get("generic")
            .expect("non-ASCII must produce an indeterminate reason");
        assert!(
            matches!(reason, EvalError::NonAscii),
            "expected NonAscii, got {reason:?}"
        );
    }

    /// `%(shell)` expansion is unsupported by design — we don't run
    /// shell at lint time.
    #[test]
    fn evalerror_shell_expansion() {
        let set = resolved(&["generic"]);
        let spec = spec_with_conditional("%if %(echo 1)");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        let reason = b
            .indeterminate_reasons
            .get("generic")
            .expect("shell expansion must produce an indeterminate reason");
        assert!(
            matches!(reason, EvalError::ShellExpansion),
            "expected ShellExpansion, got {reason:?}"
        );
    }

    /// `%[expr]` arithmetic is evaluated at build time by rpm; we
    /// surface the gap rather than guess.
    #[test]
    fn evalerror_arithmetic_expr() {
        let set = resolved(&["generic"]);
        let spec = spec_with_conditional("%if %[1+1]");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        let reason = b
            .indeterminate_reasons
            .get("generic")
            .expect("arithmetic expr must produce an indeterminate reason");
        assert!(
            matches!(reason, EvalError::ArithmeticExpr),
            "expected ArithmeticExpr, got {reason:?}"
        );
    }

    /// Regression for L1: `%{?name:default}` with `name` undefined
    /// must NOT silently expand to empty — it should surface
    /// `UnmodelledDefault` so the user knows the default body is in
    /// play. Previously the evaluator returned an empty string,
    /// which then mis-classified the branch.
    #[test]
    fn evalerror_unmodelled_default_if_defined() {
        let set = resolved(&["generic"]);
        let spec = spec_with_conditional("%if 0%{?undefined_xyz:fallback}");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        // The macro reference might be evaluated via either the
        // Raw expand_raw_string path or the Parsed evaluate_expr_ast
        // path depending on parser fallback. Both must surface
        // UnmodelledDefault now.
        let reason = b
            .indeterminate_reasons
            .get("generic")
            .expect("`%{?undefined:default}` must surface UnmodelledDefault");
        assert!(
            matches!(reason, EvalError::UnmodelledDefault),
            "expected UnmodelledDefault, got {reason:?}"
        );
    }

    /// Regression for L2: a Raw `%if` whose expansion still contains
    /// arithmetic operators (because the parser kept the body as
    /// Raw rather than promoting to a Parsed CondExpr) must NOT be
    /// classified Active just because the resulting non-empty string
    /// "looks truthy". Previously `0%{?undefined} >= 9` expanded to
    /// `"0  >= 9"` which the Str-fallback wrapped in `EvalValue::Str`
    /// — non-empty ⇒ Active, a false positive.
    #[test]
    fn evalerror_raw_arithmetic_not_silent_active() {
        let set = resolved(&["generic"]);
        let spec = spec_with_conditional("%if 0%{?undefined_xyz} >= 9");
        let report = CoverageReport::compute(&spec, &set, &crate::bcond::BcondOverrides::default());
        let b = &report.conditionals[0].branches[0];
        assert!(
            !b.active_on.contains(&"generic".to_string()),
            "Raw `%if 0%{{?undefined}} >= 9` must NOT be Active (false positive); got {b:?}"
        );
        // Should land in either Inactive (if Parsed path resolves)
        // or Indeterminate (if Raw path bails with Unsupported).
        let total = b.active_on.len() + b.inactive_on.len() + b.indeterminate_on.len();
        assert_eq!(total, 1, "every profile classified exactly once: {b:?}");
    }
}
