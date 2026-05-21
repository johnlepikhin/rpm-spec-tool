//! Build-condition (`%bcond_with` / `%bcond_without`) resolution.
//!
//! RPM spec files declare optional build-time features via `%bcond_with`
//! (defaults to "without") and `%bcond_without` (defaults to "with").
//! At build time the user flips them with `--with FEATURE` / `--without
//! FEATURE`. Inside the spec, branches gate on these via `%{with FOO}`
//! (1 if active, 0 otherwise) and `%{without FOO}` (the inverse).
//!
//! Real-world survey: 27 of 45 conditionals in `systemd.spec` use this
//! pattern. Before Phase 10 the evaluator returned `EvalError::Undefined`
//! for every one (the macro `with` is not in any profile registry), so
//! `matrix coverage`, `matrix diff`, and `matrix verify-contract` all
//! collapsed to Indeterminate on production specs. This module fixes that
//! by:
//!
//! 1. Walking the spec's `BuildCondition` AST nodes to discover declared
//!    bconds and their default states.
//! 2. Applying CLI overrides (`--with FOO` / `--without FOO`) on top.
//! 3. Producing a [`BcondMap`] consumed by [`crate::branch_coverage`]
//!    when it sees a `%{with X}` / `%{without X}` reference.
//!
//! Three declaration styles are supported:
//!
//! * `%bcond_with NAME` — default OFF, enable via `--with NAME`.
//! * `%bcond_without NAME` — default ON, disable via `--without NAME`.
//! * `%bcond NAME DEFAULT` (rpm ≥ 4.17.1) — default is an expression.
//!   Literal `0`/`1` defaults are honoured. Non-literal defaults
//!   (`%[expr]`, conditional macros, plain text) are reported as
//!   *Unevaluated* (`BcondEntry::with_active == None`); the evaluator
//!   then surfaces `%{with NAME}` references on such bconds as
//!   `EvalError::Unsupported`, not silently 0 — load-bearing on real
//!   Fedora specs (httpd, python3.13).
//!
//! The map is per-spec (bcond declarations live in the spec, not the
//! profile) but profile-agnostic — RPM's bcond model is global to the
//! build, not per-target. Per-profile bcond overrides via
//! `.rpmspec.toml` are a future extension; the on-disk schema would
//! be additive.

use std::collections::{BTreeMap, BTreeSet};

use rpm_spec::ast::{BuildCondStyle, BuildCondition, Conditional, Span, SpecFile, SpecItem};

/// User-supplied overrides from CLI flags. `with` set means "treat
/// these bconds as `--with FEATURE`"; `without` set means "treat
/// these as `--without FEATURE`". A name appearing in both is a
/// usage error; the resolver flags it as such.
#[derive(Debug, Default, Clone)]
#[non_exhaustive]
pub struct BcondOverrides {
    pub with: BTreeSet<String>,
    pub without: BTreeSet<String>,
}

impl BcondOverrides {
    /// Build from raw CLI input (multi-occurrence `--with` /
    /// `--without`). Trims whitespace; empty names are silently
    /// ignored (a degenerate `--with ""` is a typo, not a real
    /// override).
    pub fn from_cli(with: &[String], without: &[String]) -> Self {
        let trim = |slice: &[String]| -> BTreeSet<String> {
            slice
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        Self {
            with: trim(with),
            without: trim(without),
        }
    }

    /// `true` if the user passed conflicting `--with FOO` and
    /// `--without FOO`. The resolver does NOT fail on this — it
    /// follows a "with wins" rule for predictability — but callers
    /// who want to surface a CLI error should check before resolving.
    #[must_use]
    pub fn conflicts(&self) -> BTreeSet<&str> {
        self.with
            .intersection(&self.without)
            .map(String::as_str)
            .collect()
    }
}

/// Per-bcond resolution: declared by the spec, possibly flipped by
/// CLI overrides.
///
/// `with_active` is an `Option<bool>` to model the rpm ≥ 4.17.1
/// `%bcond NAME DEFAULT` form where the default is an expression
/// the static analyzer can't evaluate (e.g. `%bcond pcre2
/// %[0%{?fedora} > 35]`). In that case `with_active = None` and the
/// evaluator surfaces `%{with NAME}` as `EvalError::Unsupported`
/// rather than silently guessing false — load-bearing on real
/// Fedora specs.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BcondEntry {
    /// `Some(true)` if `%{with NAME}` resolves to 1, `Some(false)`
    /// if 0, `None` if the default expression couldn't be evaluated
    /// statically AND no CLI override was applied.
    pub with_active: Option<bool>,
    /// `true` if a CLI override produced this entry's `with_active`
    /// value — either flipping a declared default, or registering a
    /// bcond name the spec did not declare. Diagnostics surface this
    /// to distinguish "spec said so" from "user said so".
    pub overridden: bool,
}

/// Per-spec resolved state for every declared bcond, plus a
/// reverse-lookup for CLI overrides naming bconds the spec doesn't
/// declare. Built once per spec via [`BcondMap::from_spec`].
///
/// The evaluator consumes this map to resolve `%{with X}` /
/// `%{without X}` references without consulting the profile macro
/// registry (bcond state is spec-level, not profile-level).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct BcondMap {
    entries: BTreeMap<String, BcondEntry>,
    /// CLI override names that didn't match any spec bcond
    /// declaration. Diagnostics-only: an operator who passed
    /// `--with feature-i-thought-was-declared` learns about the
    /// typo via this list.
    unmatched_overrides: BTreeSet<String>,
}

impl BcondMap {
    /// Walk the spec collecting every [`BuildCondition`] node,
    /// resolve its default state from the `%bcond_with` /
    /// `%bcond_without` style, then apply user overrides.
    ///
    /// Bcond names are extracted verbatim — case-sensitive, no
    /// trimming (the parser already produces clean tokens).
    ///
    /// The walker descends into every `Conditional` body
    /// unconditionally: bconds inside `%if` blocks are still
    /// collected because RPM evaluates `%bcond_*` macros at parse
    /// time, before any `%if` resolution.
    #[must_use]
    pub fn from_spec(spec: &SpecFile<Span>, overrides: &BcondOverrides) -> Self {
        let mut entries: BTreeMap<String, BcondEntry> = BTreeMap::new();
        for item in &spec.items {
            collect_from_spec_item(item, &mut entries);
        }

        // Apply CLI overrides. `--with FOO` sets `with_active` to
        // Some(true) regardless of declared default OR Unevaluated
        // marker; `--without FOO` sets Some(false). Overrides always
        // produce a concrete state because the user has explicitly
        // committed to one — the Unevaluated state only applies when
        // the spec said "default is an expression" AND the CLI was
        // silent on the bcond.
        let mut unmatched_overrides = BTreeSet::new();
        for name in &overrides.with {
            apply_override(&mut entries, &mut unmatched_overrides, name, true);
        }
        for name in &overrides.without {
            // `--without` wins only when not also in `--with`. If
            // both are passed, `--with` takes precedence (already
            // applied above) and `--without` is a documented no-op.
            if overrides.with.contains(name) {
                continue;
            }
            apply_override(&mut entries, &mut unmatched_overrides, name, false);
        }

        Self {
            entries,
            unmatched_overrides,
        }
    }

    /// Resolve `%{with NAME}` to its boolean state:
    /// * `Some(true)` — `%{with NAME}` evaluates to 1.
    /// * `Some(false)` — `%{with NAME}` evaluates to 0.
    /// * `None` — the spec declared `%bcond NAME DEFAULT` with a
    ///   non-literal default expression AND the CLI did not flip it.
    ///   Caller (the evaluator) must surface this as Indeterminate
    ///   rather than guessing.
    ///
    /// Names not declared by the spec and not overridden by the CLI
    /// resolve to `Some(false)` — the conservative RPM default (an
    /// undefined `_with_NAME` macro expands to "0").
    #[must_use]
    pub fn with_state(&self, name: &str) -> Option<bool> {
        match self.entries.get(name) {
            Some(e) => e.with_active,
            None => Some(false),
        }
    }

    /// Resolve `%{without NAME}` — inverse of [`Self::with_state`].
    /// `None` propagates from the `with_state` result.
    #[must_use]
    pub fn without_state(&self, name: &str) -> Option<bool> {
        self.with_state(name).map(|b| !b)
    }

    /// Iterate every declared bcond with its resolved state.
    /// Diagnostics use this to render "5 bconds: 3 with, 2 without".
    pub fn entries(&self) -> impl Iterator<Item = (&str, &BcondEntry)> {
        self.entries.iter().map(|(n, e)| (n.as_str(), e))
    }

    /// CLI override names that did not match any spec declaration.
    /// Empty iff every `--with` / `--without` mentioned a bcond the
    /// spec actually declared.
    pub fn unmatched_overrides(&self) -> impl Iterator<Item = &str> {
        self.unmatched_overrides.iter().map(String::as_str)
    }

    /// `true` if the spec declared no bconds at all. Used by
    /// matrix commands to skip the BcondMap path entirely on legacy
    /// specs that pre-date `%bcond_*`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Internals: AST walking
// ---------------------------------------------------------------------------

fn record_declaration(decl: &BuildCondition<Span>, entries: &mut BTreeMap<String, BcondEntry>) {
    let with_active: Option<bool> = match decl.style {
        // `%bcond_with FOO` — default OFF (user must pass `--with FOO`
        // to enable). RPM does this by not defining `_with_FOO` until
        // the override is set.
        BuildCondStyle::BcondWith => Some(false),
        // `%bcond_without FOO` — default ON (user must pass
        // `--without FOO` to disable). RPM defines `_with_FOO` until
        // the override clears it.
        BuildCondStyle::BcondWithout => Some(true),
        // `%bcond NAME DEFAULT` (rpm ≥ 4.17.1). Try the common
        // literal-default fast path: `%bcond foo 0` / `%bcond foo 1`.
        // Anything else (macro expression, arithmetic) we cannot
        // evaluate at lint time → `None` signals "indeterminate" so
        // the evaluator surfaces a structured error rather than
        // silently picking false.
        _ => parse_literal_default(decl.default.as_ref()),
    };
    // First declaration wins. A spec that redeclares the same bcond
    // is malformed; we don't try to merge. Emit a tracing event so
    // operators running with -v see the silent loss.
    if entries.contains_key(&decl.name) {
        tracing::warn!(
            target: "bcond::duplicate_declaration",
            name = %decl.name,
            "duplicate %bcond declaration; later one ignored"
        );
        return;
    }
    entries.insert(
        decl.name.clone(),
        BcondEntry {
            with_active,
            overridden: false,
        },
    );
}

/// Apply one CLI override (`--with NAME` if `target=true`,
/// `--without NAME` if `target=false`). Promotes an undeclared bcond
/// to a synthetic entry, marks every touched entry as `overridden`,
/// and records undeclared overrides in `unmatched_overrides` for
/// diagnostics.
fn apply_override(
    entries: &mut BTreeMap<String, BcondEntry>,
    unmatched: &mut BTreeSet<String>,
    name: &str,
    target: bool,
) {
    match entries.get_mut(name) {
        #[allow(clippy::collapsible_match)]
        Some(entry) => {
            if entry.with_active != Some(target) {
                entry.with_active = Some(target);
                entry.overridden = true;
            }
        }
        None => {
            unmatched.insert(name.to_string());
            entries.insert(
                name.to_string(),
                BcondEntry {
                    with_active: Some(target),
                    overridden: true,
                },
            );
        }
    }
}

/// Parse the `Text` default of a `%bcond NAME DEFAULT` declaration.
/// `Some(true)` / `Some(false)` when the default is a literal
/// `"1"` / `"0"`; `None` when the default is non-literal (a macro
/// reference, an arithmetic expression, etc.) — caller treats `None`
/// as "indeterminate, cannot statically resolve".
fn parse_literal_default(default: Option<&rpm_spec::ast::Text>) -> Option<bool> {
    let lit = default?.literal_str()?.trim();
    match lit {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

fn collect_from_spec_item(item: &SpecItem<Span>, entries: &mut BTreeMap<String, BcondEntry>) {
    match item {
        SpecItem::BuildCondition(b) => record_declaration(b, entries),
        SpecItem::Conditional(c) => collect_from_top_conditional(c, entries),
        // Sub-package (`%package`) preambles cannot host
        // `%bcond_*` declarations: the AST puts `BuildCondition`
        // inside `SpecItem`, not `PreambleContent`. Skipping
        // `Section` here is a deliberate no-op, not an omission.
        _ => {}
    }
}

fn collect_from_top_conditional(
    c: &Conditional<Span, SpecItem<Span>>,
    entries: &mut BTreeMap<String, BcondEntry>,
) {
    // Descend into every branch + otherwise — RPM evaluates `%bcond`
    // at parse time, before %if resolution, so all declarations
    // contribute regardless of which branch would run.
    for branch in &c.branches {
        for sub in &branch.body {
            collect_from_spec_item(sub, entries);
        }
    }
    if let Some(else_body) = &c.otherwise {
        for sub in else_body {
            collect_from_spec_item(sub, entries);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn map_for(src: &str, with: &[&str], without: &[&str]) -> BcondMap {
        let parsed = parse(src);
        let overrides = BcondOverrides::from_cli(
            &with.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &without.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        );
        BcondMap::from_spec(&parsed.spec, &overrides)
    }

    const SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
%bcond_with bootstrap
%bcond_without docs

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn bcond_with_defaults_to_zero() {
        // `%bcond_with bootstrap` — default OFF.
        let m = map_for(SPEC, &[], &[]);
        assert_eq!(m.with_state("bootstrap"), Some(false));
        assert_eq!(m.without_state("bootstrap"), Some(true));
    }

    #[test]
    fn bcond_without_defaults_to_one() {
        // `%bcond_without docs` — default ON.
        let m = map_for(SPEC, &[], &[]);
        assert_eq!(m.with_state("docs"), Some(true));
        assert_eq!(m.without_state("docs"), Some(false));
    }

    #[test]
    fn cli_with_enables_disabled_bcond() {
        let m = map_for(SPEC, &["bootstrap"], &[]);
        assert_eq!(m.with_state("bootstrap"), Some(true));
        let (_, entry) = m
            .entries()
            .find(|(n, _)| *n == "bootstrap")
            .expect("present");
        assert!(entry.overridden);
    }

    #[test]
    fn cli_without_disables_enabled_bcond() {
        let m = map_for(SPEC, &[], &["docs"]);
        assert_eq!(m.with_state("docs"), Some(false));
        assert_eq!(m.without_state("docs"), Some(true));
    }

    #[test]
    fn undeclared_bcond_defaults_to_zero() {
        // Spec doesn't declare `foo` — `%{with foo}` resolves to 0
        // (RPM default for undefined `_with_foo` macro).
        let m = map_for(SPEC, &[], &[]);
        assert_eq!(m.with_state("never-declared"), Some(false));
    }

    #[test]
    fn cli_with_for_undeclared_bcond_registers_unmatched() {
        let m = map_for(SPEC, &["never-declared"], &[]);
        // Override applies (operator may know about a bcond
        // synthesised by an included `.spec` file or a macro
        // package), but it surfaces as unmatched for diagnostics.
        assert_eq!(m.with_state("never-declared"), Some(true));
        let unmatched: Vec<&str> = m.unmatched_overrides().collect();
        assert_eq!(unmatched, vec!["never-declared"]);
    }

    #[test]
    fn conflict_with_wins_over_without() {
        // `--with FOO --without FOO` is a usage error; resolver
        // picks "with" so output is at least predictable.
        let m = map_for(SPEC, &["bootstrap"], &["bootstrap"]);
        assert_eq!(m.with_state("bootstrap"), Some(true));
    }

    #[test]
    fn overrides_conflicts_lists_intersection() {
        let ovr = BcondOverrides::from_cli(&["a".into(), "b".into()], &["b".into(), "c".into()]);
        let conflicts: Vec<&str> = ovr.conflicts().into_iter().collect();
        assert_eq!(conflicts, vec!["b"]);
    }

    #[test]
    fn empty_input_yields_empty_map() {
        let m = map_for(
            "Name: foo\nVersion: 1\nRelease: 1\nSummary: t\nLicense: MIT\n\n%description\nB\n\n%files\n\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1.0-1\n- init\n",
            &[],
            &[],
        );
        assert!(m.is_empty());
    }

    #[test]
    fn bcond_two_arg_with_literal_default_one_is_active() {
        // `%bcond NAME 1` (rpm ≥ 4.17.1, 2-arg form) — fast-path
        // literal default. `%{with NAME}` must resolve to active.
        // Real Fedora specs use this form (python3.13.spec, httpd
        // etc.).
        const SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
%bcond docs 1
%bcond bootstrap 0

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let m = map_for(SPEC, &[], &[]);
        assert_eq!(m.with_state("docs"), Some(true));
        assert_eq!(m.with_state("bootstrap"), Some(false));
    }

    #[test]
    fn bcond_two_arg_with_expression_default_is_unevaluated() {
        // `%bcond NAME %[expr]` — default is an expression we
        // cannot statically evaluate. State must be `None` so the
        // evaluator surfaces Indeterminate, not silently false.
        const SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
%bcond pcre2 %[0%{?fedora} > 35]

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let m = map_for(SPEC, &[], &[]);
        assert_eq!(
            m.with_state("pcre2"),
            None,
            "expression default must produce Unevaluated state"
        );
    }

    #[test]
    fn bcond_two_arg_cli_override_collapses_unevaluated() {
        // Same spec as above but `--with pcre2` forces the state.
        // After override the entry must be Some(true) — CLI commits
        // to a concrete value the operator chose.
        const SPEC: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
%bcond pcre2 %[0%{?fedora} > 35]

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let m = map_for(SPEC, &["pcre2"], &[]);
        assert_eq!(m.with_state("pcre2"), Some(true));
    }

    #[test]
    fn bcond_inside_conditional_still_collected() {
        // RPM evaluates `%bcond_*` at parse time, before %if
        // resolution. Bconds declared inside an `%if` body must
        // still appear in the map.
        const NESTED: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
%if 0%{?fedora}
%bcond_with experimental
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let m = map_for(NESTED, &[], &[]);
        assert!(
            m.entries().any(|(n, _)| n == "experimental"),
            "bconds inside %if must be visible"
        );
    }
}
