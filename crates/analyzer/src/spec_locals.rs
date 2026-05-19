//! Scan a parsed spec for *spec-local* macro definitions whose values
//! should augment a profile's [`rpm_spec_profile::MacroRegistry`] before
//! branch-condition evaluation.
//!
//! Without this pass, a branch like `%if %{ssl}` reports `[INDETERMINATE:
//! undefined macro: ssl]` even when the very same spec opens with
//! `%{!?ssl:%global ssl 1}` — the analyzer was treating the conditional
//! default-set idiom as opaque text instead of recognising that, by the
//! time RPM evaluates the condition, `ssl` is unconditionally `1`.
//!
//! Patterns recognised:
//!
//! * `%global NAME VALUE` / `%define NAME VALUE` — top-level, literal body.
//! * `%{!?NAME:%global NAME VALUE}` — the "set default" idiom (the parser
//!   models the outer `%{!?…}` as a [`MacroRef`] with `with_value`).
//!
//! Values are extracted as **raw literals**. They may still contain
//! `%{other}` macro references; the branch evaluator's
//! [`rpm_spec_profile::MacroRegistry::expand_to_literal`] handles the
//! eventual chain.
//!
//! # Profile-aware scanning
//!
//! Top-level `%if`/`%else` blocks frequently switch a macro between two
//! values based on a distro check (e.g. `%global suse 1` inside
//! `%if "%{_vendor}" == "suse"`, `%global suse 0` in the matching
//! `%else`). To produce the right value per profile, [`scan_spec_locals`]
//! evaluates each top-level branch's condition against the profile and
//! only walks branches that resolve to active. Indeterminate or
//! profile-independent branches are walked as a best-effort fallback
//! (first-write-wins) so we still pick up the common case where the
//! spec author guards a `%global` on something we can't evaluate yet.
//!
//! # Conservatism
//!
//! * `%undefine` is ignored — modelling its "pop one level" semantics
//!   requires knowing the stack depth, which we don't track.
//! * Bodies that contain unresolved macro references are skipped —
//!   the runtime registry's `expand_to_literal` handles those chains
//!   correctly; we'd just stash a half-expanded string.

use std::collections::BTreeMap;

use rpm_spec::ast::{
    ConditionalMacro, MacroDef, MacroDefKind, MacroRef, Span, SpecFile, SpecItem, Text, TextSegment,
};
use rpm_spec_profile::Profile;

/// Scan `spec` for spec-local macro definitions, evaluating top-level
/// `%if`/`%else` against `profile`/`bcond` so the per-profile
/// definitions resolve correctly (`%global suse 1` inside
/// `%if "%{_vendor}" == "suse"` only counts on actual SUSE profiles).
///
/// Returns a `name → raw literal value` map. The map is `BTreeMap` so
/// iteration order is deterministic when callers feed the result into
/// a profile registry.
///
/// The map is *augmentative*: callers should only insert these into a
/// [`rpm_spec_profile::MacroRegistry`] when the name is not already
/// defined — profile values take precedence over spec-local defaults
/// because the operator's explicit `-D` or per-profile `[macros]`
/// declaration is a stronger signal than the spec's "use this unless
/// you've already set it" fallback.
///
/// Prefer [`scan_spec_locals_into`] in hot loops (matrix evaluator)
/// where one `BTreeMap` can be reused across profiles via `clear()` —
/// this thin wrapper allocates a fresh map per call.
#[must_use]
pub fn scan_spec_locals(
    spec: &SpecFile<Span>,
    profile: &Profile,
    bcond: &crate::bcond::BcondMap,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    scan_spec_locals_into(spec, profile, bcond, &mut out);
    out
}

/// Fill-in-place variant of [`scan_spec_locals`].
///
/// Callers iterating over many profiles (e.g. the matrix evaluator)
/// can reuse a single `BTreeMap` across the loop by calling
/// [`BTreeMap::clear`] between profiles, avoiding one full
/// allocation+deallocation cycle per target.
///
/// The map is *augmented*: existing entries are preserved (first
/// write wins, matching the semantics of [`scan_spec_locals`] —
/// `BTreeMap::entry(...).or_insert_with(...)` under the hood).
/// Callers that want a fresh result must `clear()` first.
pub fn scan_spec_locals_into(
    spec: &SpecFile<Span>,
    profile: &Profile,
    bcond: &crate::bcond::BcondMap,
    out: &mut BTreeMap<String, String>,
) {
    for item in &spec.items {
        scan_item(item, profile, bcond, out);
    }
    tracing::trace!(
        profile = %profile.identity.name,
        count = out.len(),
        "scanned spec-locals",
    );
}

fn scan_item(
    item: &SpecItem<Span>,
    profile: &Profile,
    bcond: &crate::bcond::BcondMap,
    out: &mut BTreeMap<String, String>,
) {
    match item {
        SpecItem::MacroDef(def) => extract_from_def(def, out),
        SpecItem::Statement(mr) => extract_from_default_set(mr, out),
        SpecItem::Conditional(c) => {
            scan_conditional(c, profile, bcond, out);
        }
        // BuildCondition (`%bcond_*`) is handled separately by the
        // bcond map — registry entries are inserted via
        // `MacroRegistry::insert` elsewhere with the right
        // with/without semantics. Not our concern here.
        _ => {}
    }
}

/// Walk a top-level `%if`/`%elif`/`%else` block, evaluating each
/// branch's condition against `profile` and recursing only into the
/// branches that resolve active. RPM evaluates conditionals
/// short-circuit (first active branch wins; `%else` runs only when
/// all preceding fail), so we mirror that: we walk until we either
/// take a definitively-active branch or fall through to `%else`.
///
/// When a branch evaluates to `Indeterminate` (e.g. condition
/// references a macro the profile doesn't define yet — chicken and
/// egg with our own output), the conservative path is to walk it AND
/// the next siblings: any `%global` we find may be the spec author's
/// intended definition. First-write-wins keeps the result
/// deterministic.
fn scan_conditional(
    c: &rpm_spec::ast::Conditional<Span, SpecItem<Span>>,
    profile: &Profile,
    bcond: &crate::bcond::BcondMap,
    out: &mut BTreeMap<String, String>,
) {
    let mut any_active = false;
    let mut any_indeterminate = false;
    for branch in &c.branches {
        match crate::branch_coverage::evaluate_branch(branch.kind, &branch.expr, profile, bcond) {
            Ok(true) => {
                // Active branch fires — walk it and stop. RPM's
                // semantics: the first true branch wins.
                for nested in &branch.body {
                    scan_item(nested, profile, bcond, out);
                }
                any_active = true;
                break;
            }
            Ok(false) => {
                // Inactive on this profile — skip the body entirely.
            }
            Err(_) => {
                // Indeterminate — we don't know if this branch fires.
                // Conservative: walk it too (first-write-wins), and
                // continue to consider siblings since RPM might pick
                // any of them. The marker prevents falling through
                // to `%else` (which only fires when all branches are
                // *known* inactive).
                any_indeterminate = true;
                for nested in &branch.body {
                    scan_item(nested, profile, bcond, out);
                }
            }
        }
    }
    if !any_active && !any_indeterminate {
        // All branches inactive — `%else` fires.
        if let Some(els) = &c.otherwise {
            for nested in els {
                scan_item(nested, profile, bcond, out);
            }
        }
    }
}

fn extract_from_def(def: &MacroDef<Span>, out: &mut BTreeMap<String, String>) {
    if def.name.is_empty() {
        return;
    }
    // `%undefine NAME` — semantically pops a definition level; without
    // a stack model we can't faithfully apply it. Ignore so we never
    // *remove* a value the evaluator might still need.
    if matches!(def.kind, MacroDefKind::Undefine) {
        return;
    }
    // Body must be a single literal segment. If it contains nested
    // macros (`%global foo %{prefix}-suffix`), expansion belongs to
    // the runtime registry, not to us — bail rather than store an
    // un-expanded body that lookups will then mis-handle.
    if let Some(value) = def.body.literal_str() {
        out.entry(def.name.clone())
            .or_insert_with(|| value.trim().to_owned());
    }
}

fn extract_from_default_set(mr: &MacroRef, out: &mut BTreeMap<String, String>) {
    // Only `%{!?NAME:BODY}` — the conditional-default-set idiom.
    // `%{?NAME:BODY}` means "if defined, expand to BODY" — that's an
    // accessor, not a definition site. Don't treat it as one.
    if !matches!(mr.conditional, ConditionalMacro::IfNotDefined) {
        return;
    }
    if mr.name.is_empty() {
        return;
    }
    let Some(body) = mr.with_value.as_ref() else {
        return;
    };
    if let Some((name, value)) = parse_default_set_body(body) {
        // Strict match: only register when the inner `%global`'s
        // target name matches the outer `%{!?NAME}` guard. Anything
        // else is a more exotic pattern we don't model.
        if name == mr.name {
            out.entry(name).or_insert(value);
        }
    }
}

/// Parse the body of `%{!?…:body}` looking for an embedded
/// `%global NAME VALUE` (or `%define NAME VALUE`) statement.
///
/// The parser emits this as a [`Text`] of two segments: the first is a
/// [`TextSegment::Macro`] for `%global`/`%define` itself (a `MacroRef`
/// with `name = "global"` and no args/with_value), and the second is a
/// [`TextSegment::Literal`] carrying `" NAME VALUE"`. We extract the
/// name (first whitespace-delimited token) and value (everything after)
/// from that literal tail.
fn parse_default_set_body(body: &Text) -> Option<(String, String)> {
    let mut iter = body.segments.iter().peekable();
    // Skip any leading whitespace-only literal segments (the parser
    // doesn't normally emit those, but we stay defensive).
    while let Some(TextSegment::Literal(s)) = iter.peek() {
        if s.trim().is_empty() {
            iter.next();
        } else {
            break;
        }
    }
    let kw = match iter.next()? {
        TextSegment::Macro(mr) => mr,
        _ => return None,
    };
    if kw.name != "global" && kw.name != "define" {
        return None;
    }
    // Whatever follows must be a single literal segment carrying
    // " NAME VALUE". A second `Macro` segment (e.g. `%global foo
    // %{prefix}`) means the value isn't a literal we can store — bail.
    let tail = match iter.next()? {
        TextSegment::Literal(s) => s.as_str(),
        _ => return None,
    };
    // Any further segments would mean the value contains macros too;
    // we already extracted up to the first macro and treat the rest
    // as opaque — bail.
    if iter.next().is_some() {
        return None;
    }
    let trimmed = tail.trim_start();
    let split = trimmed.find(char::is_whitespace)?;
    let name = trimmed[..split].trim().to_owned();
    let value = trimmed[split..].trim().to_owned();
    if name.is_empty() {
        return None;
    }
    Some((name, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bcond::{BcondMap, BcondOverrides};

    fn parse(src: &str) -> SpecFile<Span> {
        crate::session::parse(src).spec
    }

    fn empty_profile() -> Profile {
        Profile::default()
    }

    fn bcond_for(spec: &SpecFile<Span>) -> BcondMap {
        BcondMap::from_spec(spec, &BcondOverrides::default())
    }

    fn scan(src: &str) -> BTreeMap<String, String> {
        let spec = parse(src);
        let bc = bcond_for(&spec);
        scan_spec_locals(&spec, &empty_profile(), &bc)
    }

    #[test]
    fn extracts_top_level_global() {
        let locals = scan("%global ssl 1\n%global edition std\nName: foo\n");
        assert_eq!(locals.get("ssl").map(String::as_str), Some("1"));
        assert_eq!(locals.get("edition").map(String::as_str), Some("std"));
    }

    #[test]
    fn extracts_top_level_define() {
        let locals = scan("%define foo bar\nName: x\n");
        assert_eq!(locals.get("foo").map(String::as_str), Some("bar"));
    }

    #[test]
    fn extracts_default_set_idiom() {
        let locals = scan("%{!?ssl:%global ssl 1}\n%{!?xml:%global xml 1}\nName: x\n");
        assert_eq!(locals.get("ssl").map(String::as_str), Some("1"));
        assert_eq!(locals.get("xml").map(String::as_str), Some("1"));
    }

    #[test]
    fn always_true_branch_takes_if_body() {
        // `%if 1` is constant-true — evaluator returns Ok(true). We
        // walk only the `%if` body, ignoring `%else`.
        let locals = scan("%if 1\n%global foo first\n%else\n%global foo second\n%endif\nName: x\n");
        assert_eq!(locals.get("foo").map(String::as_str), Some("first"));
    }

    #[test]
    fn always_false_branch_takes_else_body() {
        // `%if 0` is constant-false — `%if` body skipped, `%else` runs.
        let locals = scan("%if 0\n%global foo skipped\n%else\n%global foo elsebody\n%endif\nName: x\n");
        assert_eq!(locals.get("foo").map(String::as_str), Some("elsebody"));
    }

    #[test]
    fn indeterminate_branch_walks_all_first_wins() {
        // `%if %{undefined}` is indeterminate — conservative scan
        // walks the body and prevents %else fallthrough (we don't
        // know whether %if would have fired). First-write-wins.
        let locals =
            scan("%if %{noprof}\n%global foo body1\n%else\n%global foo body2\n%endif\nName: x\n");
        assert_eq!(locals.get("foo").map(String::as_str), Some("body1"));
    }

    #[test]
    fn ignores_undefine() {
        let locals = scan("%global foo 1\n%undefine foo\nName: x\n");
        assert_eq!(locals.get("foo").map(String::as_str), Some("1"));
    }

    #[test]
    fn skips_non_literal_body() {
        let locals = scan("%global foo %{bar}\nName: x\n");
        assert!(!locals.contains_key("foo"));
    }

    #[test]
    fn skips_accessor_form() {
        let locals = scan("%{?ssl:enable}\nName: x\n");
        assert!(!locals.contains_key("ssl"));
    }

    #[test]
    fn skips_mismatched_default_set() {
        let locals = scan("%{!?ssl:%global other 1}\nName: x\n");
        assert!(!locals.contains_key("ssl"));
        assert!(!locals.contains_key("other"));
    }

    #[test]
    fn empty_spec_yields_empty_map() {
        let locals = scan("Name: foo\n");
        assert!(locals.is_empty());
    }

    #[test]
    fn registers_global_with_empty_literal_body() {
        // Edge case: `%global foo` (no value), `%global foo ` (trailing
        // space only), and `%global foo    ` (whitespace tail) all reach
        // `extract_from_def` with a literal body that trims to "".
        // Current behaviour: register `foo` with value `""`. Documenting
        // this so a future change that decides to *skip* empty bodies
        // doesn't silently break callers who happen to test
        // `locals.contains_key("foo")` regardless of value.
        //
        // RPM's runtime treats `%global foo` (no body) as "defined,
        // expands to empty" — matching that semantics here keeps the
        // analyzer aligned with rpm's `expand_to_literal`.
        for src in [
            "%global foo\nName: x\n",
            "%global foo \nName: x\n",
            "%global foo    \nName: x\n",
        ] {
            let locals = scan(src);
            assert_eq!(
                locals.get("foo").map(String::as_str),
                Some(""),
                "expected empty-value registration for: {src:?}",
            );
        }
    }
}
