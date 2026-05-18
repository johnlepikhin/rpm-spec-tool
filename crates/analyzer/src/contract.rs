//! Spec contract verification.
//!
//! A *contract* is a separate TOML file declaring per-profile
//! expectations about what the spec must produce — Phase 7 ships
//! `must_have_buildrequires` and `must_not_have_buildrequires` only.
//! Future phases can add binary-package, file-list, or arch
//! assertions without breaking the on-disk format (the TOML uses
//! `#[serde(deny_unknown_fields)]` so typos surface up-front; new
//! fields land as additive `#[serde(default)]`).
//!
//! The verifier is conditional-unaware in this phase: it collects
//! every `BuildRequires:` line in the spec, regardless of enclosing
//! `%if`/`%ifarch`. That is the right semantic for CI gating ("did
//! someone forget to declare a required build dep, even behind a
//! guard?") but it does mean a `BuildRequires: foo` inside
//! `%if 0%{?fedora}` will satisfy a contract that asks for `foo` on
//! a RHEL profile. Document the limitation; the branch-aware variant
//! is a follow-up.
//!
//! Rich/boolean deps in the source spec are flattened conservatively:
//! `And` / `Or` clauses register every named atom (so
//! `BuildRequires: (gcc and make)` satisfies both `gcc` and `make`),
//! while `If` / `Unless` / `Not` arms are skipped — the conditional
//! semantics there are too policy-laden to interpret at lint time
//! ("does `foo` count when it's only required if `cond`?"). The skip
//! means `BuildRequires: (libfoo if systemd)` is invisible to
//! `must_have_buildrequires = ["libfoo"]` and will fail the gate.
//! See [`BuildRequiresCollector::record_bool_dep`] for the exact rule.

use std::collections::{BTreeMap, HashMap};

use rpm_spec::ast::{
    BoolDep, Conditional, DepExpr, FilesContent, MacroKind, PreambleContent, PreambleItem, Span,
    SpecFile, SpecItem, Tag, TagValue, Text, TextSegment,
};
use serde::Serialize;

use crate::visit::{self, Visit};

/// Render a [`Text`] preserving macro surface form. `%name` / `%{name}`
/// segments come back as-typed so the contract author can match
/// macro-bearing dep names verbatim (e.g. `%{?systemd_requires}`).
fn render_text(text: &Text) -> String {
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
            // `TextSegment` is `#[non_exhaustive]` — unknown variants
            // contribute nothing so we don't mis-render an unfamiliar
            // shape into a misleading surface form.
            _ => {}
        }
    }
    out
}

/// Top-level shape of a contract TOML document.
///
/// ```toml
/// [profiles."rhel-9-x86_64"]
/// must_have_buildrequires = ["gcc", "make"]
/// must_not_have_buildrequires = ["egrep"]
///
/// [profiles."altlinux-10-x86_64"]
/// must_have_buildrequires = ["rpm-build"]
/// ```
///
/// Keys under `profiles` are profile identifiers exactly as listed
/// in `[targets.<name>.profiles]` (or passed via `--profiles`).
/// Profiles absent from the contract are silently skipped during
/// verification — they produce a [`ContractProfileStatus::NoContract`]
/// report entry so the operator can see them, but contribute no
/// violations.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Contract {
    /// Per-profile expectation blocks. `BTreeMap` keeps the on-disk
    /// reading order stable across runs of the same contract file.
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileContract>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ProfileContract {
    /// Build deps that MUST appear (by name) in the spec for this
    /// profile. Version constraints (`gcc >= 10`) are stripped on the
    /// spec side, so list bare names here.
    #[serde(default)]
    pub must_have_buildrequires: Vec<String>,
    /// Build deps that MUST NOT appear. Use to ban legacy or vendor-
    /// specific packages.
    #[serde(default)]
    pub must_not_have_buildrequires: Vec<String>,
}

impl Contract {
    /// Parse a TOML document. The reader does not validate that
    /// profile names exist in any target set — that join happens at
    /// verification time so the contract file can be reused across
    /// target sets (e.g. a stable "must have a C compiler" rule).
    pub fn from_toml_str(s: &str) -> Result<Self, ContractError> {
        toml::from_str(s).map_err(ContractError::Toml)
    }
}

/// Error returned by [`Contract::from_toml_str`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ContractError {
    /// TOML parsing or `#[serde(deny_unknown_fields)]` violation —
    /// the inner `toml::de::Error` carries line/column for the
    /// surface error message.
    #[error("contract TOML parse error: {0}")]
    Toml(#[source] toml::de::Error),
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// Per-profile contract verdict aggregated across one spec.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ContractReport {
    /// One entry per profile passed in to
    /// [`ContractReport::compute`], in input order. Profiles not
    /// declared in the contract surface as
    /// [`ContractProfileStatus::NoContract`] rather than being silently
    /// dropped — the operator can see exactly which profiles were
    /// gated and which were not.
    pub per_profile: Vec<ProfileContractReport>,
}

#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ProfileContractReport {
    pub profile_id: String,
    pub status: ContractProfileStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContractProfileStatus {
    /// No `[profiles."<id>"]` block in the contract — silently
    /// skipped. Listed so the operator can spot a profile that
    /// should have had a contract block.
    NoContract,
    /// Every `must_have` was found and no `must_not_have` was
    /// present.
    Pass,
    /// One or more contract clauses failed. Order matches the
    /// contract's declared order: every `must_have_buildrequires`
    /// entry is checked first (in declaration order), then every
    /// `must_not_have_buildrequires` entry. Declaration-order is
    /// stable across runs because the on-disk `Vec` is preserved by
    /// serde and `verify_one` iterates it as-is.
    Violations { violations: Vec<ContractViolation> },
}

/// Concrete contract violation.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContractViolation {
    /// `must_have_buildrequires` entry that was not seen in the
    /// spec. Carries the canonical name (as written in the contract).
    MissingRequired { package: String },
    /// `must_not_have_buildrequires` entry that WAS seen. Carries
    /// the canonical name plus the surface form found in the spec
    /// (which may include macros for diagnostic context).
    ForbiddenPresent { package: String, found_as: String },
}

impl ContractReport {
    /// Compute the per-profile verdict against `spec`.
    ///
    /// `profile_ids` is the declared order from the matrix run; the
    /// report's `per_profile` mirrors it so CLI renderers can pair
    /// rows with the matrix's profile column ordering without an
    /// extra sort step.
    pub fn compute(spec: &SpecFile<Span>, contract: &Contract, profile_ids: &[String]) -> Self {
        // Collect once, share across profiles. BuildRequires
        // extraction is profile-agnostic in this phase (see module
        // doc), so the cost is paid once per spec rather than per
        // profile.
        let mut collector = BuildRequiresCollector::default();
        visit::walk_spec(&mut collector, spec);
        let found = collector.found;

        let per_profile = profile_ids
            .iter()
            .map(|pid| {
                let status = match contract.profiles.get(pid) {
                    None => ContractProfileStatus::NoContract,
                    Some(pc) => verify_one(pc, &found),
                };
                ProfileContractReport {
                    profile_id: pid.clone(),
                    status,
                }
            })
            .collect();
        Self { per_profile }
    }

    /// `true` iff any profile produced [`ContractProfileStatus::Violations`].
    /// CLI exit-code policy hangs off this predicate.
    pub fn has_violations(&self) -> bool {
        self.per_profile
            .iter()
            .any(|r| matches!(r.status, ContractProfileStatus::Violations { .. }))
    }
}

fn verify_one(pc: &ProfileContract, found: &[FoundBuildRequire]) -> ContractProfileStatus {
    // Index the spec's BuildRequires once: HashMap by canonical
    // name → first-occurrence surface form. O(F) build, O(1) lookup
    // per contract entry — turns the verifier from O(F·(R+N)) into
    // O(F + R + N) where F is found count, R is must_have count,
    // N is must_not_have count. The "first occurrence" choice
    // matches the previous Vec::find behaviour so output stays
    // identical when a forbidden dep appears in multiple places.
    let index: HashMap<&str, &FoundBuildRequire> =
        found
            .iter()
            .fold(HashMap::with_capacity(found.len()), |mut acc, f| {
                acc.entry(f.canonical.as_str()).or_insert(f);
                acc
            });

    let mut violations = Vec::new();

    // Both sides are trimmed symmetrically. `record_atom_name`
    // already trims `canonical`/`surface` from the spec side; we
    // trim contract entries here so a `"  gcc  "` typo in the TOML
    // matches the same way `gcc` does. Empty trimmed entries are
    // skipped silently (an empty `must_have` element is a contract
    // typo, not a check the verifier should run).
    for required in &pc.must_have_buildrequires {
        let key = required.trim();
        if key.is_empty() {
            continue;
        }
        if !index.contains_key(key) {
            violations.push(ContractViolation::MissingRequired {
                package: required.clone(),
            });
        }
    }

    // Forbidden: every entry must be absent. When present, attach
    // the surface form to help the operator find the offending line.
    for forbidden in &pc.must_not_have_buildrequires {
        let key = forbidden.trim();
        if key.is_empty() {
            continue;
        }
        if let Some(found_match) = index.get(key) {
            violations.push(ContractViolation::ForbiddenPresent {
                package: forbidden.clone(),
                found_as: found_match.surface.clone(),
            });
        }
    }

    if violations.is_empty() {
        ContractProfileStatus::Pass
    } else {
        ContractProfileStatus::Violations { violations }
    }
}

// ---------------------------------------------------------------------------
// BuildRequires collector
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct FoundBuildRequire {
    /// Literal-segment projection (when the AST text has no macro
    /// segments) or rendered surface form (when it does). Used
    /// **case-sensitively** for equality against contract entries —
    /// `verify_one` treats `gcc` and `GCC` as distinct, matching the
    /// RPM convention for dep names.
    canonical: String,
    /// Surface form with macros preserved (`%{name}-devel`). Used
    /// only in `ForbiddenPresent` for diagnostic context.
    surface: String,
}

#[derive(Debug, Default)]
struct BuildRequiresCollector {
    found: Vec<FoundBuildRequire>,
}

impl BuildRequiresCollector {
    fn record_atom_name(&mut self, name: &Text) {
        // Prefer the pure-literal projection when the dep name has
        // no macro segments — that's the canonical key contracts
        // match against. For macro-bearing names (`%{name}-devel`)
        // we fall back to the with-macros rendering so the canonical
        // and the surface form coincide; the contract author can
        // still match it but must write the verbatim form.
        let surface = render_text(name);
        let canonical = name
            .literal_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| surface.clone());
        let trimmed_canonical = canonical.trim().to_string();
        let trimmed_surface = surface.trim().to_string();
        if trimmed_canonical.is_empty() {
            return;
        }
        self.found.push(FoundBuildRequire {
            canonical: trimmed_canonical,
            surface: trimmed_surface,
        });
    }

    fn record_dep(&mut self, dep: &DepExpr) {
        match dep {
            DepExpr::Atom(atom) => self.record_atom_name(&atom.name),
            DepExpr::Rich(boxed) => self.record_bool_dep(boxed),
            // Defensive: the AST is `#[non_exhaustive]`. Future
            // variants are silently skipped — the contract verifier
            // is conservative on unknown shapes (no false-positive
            // missing-required when we couldn't read the spec).
            _ => {}
        }
    }

    fn record_bool_dep(&mut self, b: &BoolDep) {
        // Rich-dep handling for MVP: flatten And/Or so a clause like
        // `(gcc and make)` registers both `gcc` and `make`. `If`/
        // `Unless`/`Not` arms are too policy-laden to interpret here
        // (e.g. "must `foo` count if it's only required when `cond`?")
        // — we skip them and document the gap.
        match b {
            BoolDep::And(items) | BoolDep::Or(items) => {
                for d in items {
                    self.record_dep(d);
                }
            }
            // Conservative skip for conditional/negation rich deps.
            _ => {}
        }
    }
}

impl<'ast> Visit<'ast> for BuildRequiresCollector {
    fn visit_preamble(&mut self, item: &'ast PreambleItem<Span>) {
        if matches!(item.tag, Tag::BuildRequires) {
            if let TagValue::Dep(dep) = &item.value {
                self.record_dep(dep);
            }
        }
        visit::walk_preamble(self, item);
    }

    // Conditional bodies recurse normally so BuildRequires inside
    // `%if` / `%ifarch` are still collected. The module doc records
    // that this is the intended MVP behaviour.
    fn visit_top_conditional(&mut self, c: &'ast Conditional<Span, SpecItem<Span>>) {
        visit::walk_top_conditional(self, c);
    }
    fn visit_preamble_conditional(
        &mut self,
        c: &'ast Conditional<Span, PreambleContent<Span>>,
    ) {
        visit::walk_preamble_conditional(self, c);
    }
    fn visit_files_conditional(&mut self, c: &'ast Conditional<Span, FilesContent<Span>>) {
        visit::walk_files_conditional(self, c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run_report(spec_src: &str, contract_toml: &str, profile_ids: &[&str]) -> ContractReport {
        let parsed = parse(spec_src);
        let contract = Contract::from_toml_str(contract_toml).expect("parse contract");
        let ids: Vec<String> = profile_ids.iter().map(|s| s.to_string()).collect();
        ContractReport::compute(&parsed.spec, &contract, &ids)
    }

    const SPEC_WITH_GCC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: make

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn contract_pass_when_all_required_present() {
        let contract = r#"
[profiles."rhel-9"]
must_have_buildrequires = ["gcc", "make"]
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["rhel-9"]);
        assert!(!r.has_violations());
        match &r.per_profile[0].status {
            ContractProfileStatus::Pass => {}
            other => panic!("expected Pass, got {other:?}"),
        }
    }

    #[test]
    fn contract_fails_on_missing_required() {
        let contract = r#"
[profiles."rhel-9"]
must_have_buildrequires = ["gcc", "missing-pkg"]
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["rhel-9"]);
        assert!(r.has_violations());
        match &r.per_profile[0].status {
            ContractProfileStatus::Violations { violations } => {
                assert_eq!(violations.len(), 1);
                assert!(matches!(
                    violations[0],
                    ContractViolation::MissingRequired { ref package } if package == "missing-pkg"
                ));
            }
            other => panic!("expected Violations, got {other:?}"),
        }
    }

    #[test]
    fn contract_fails_on_forbidden_present() {
        let contract = r#"
[profiles."rhel-9"]
must_not_have_buildrequires = ["gcc"]
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["rhel-9"]);
        assert!(r.has_violations());
        match &r.per_profile[0].status {
            ContractProfileStatus::Violations { violations } => {
                assert_eq!(violations.len(), 1);
                assert!(matches!(
                    violations[0],
                    ContractViolation::ForbiddenPresent { ref package, .. } if package == "gcc"
                ));
            }
            other => panic!("expected Violations, got {other:?}"),
        }
    }

    #[test]
    fn profile_without_contract_is_skipped() {
        let contract = r#"
[profiles."rhel-9"]
must_have_buildrequires = ["gcc"]
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["altlinux-10"]);
        assert!(!r.has_violations());
        match &r.per_profile[0].status {
            ContractProfileStatus::NoContract => {}
            other => panic!("expected NoContract, got {other:?}"),
        }
    }

    #[test]
    fn empty_contract_passes_every_profile() {
        // No must-have / must-not-have lists → vacuously Pass for
        // any profile that is declared. Sanity for contracts in
        // early bootstrap.
        let contract = r#"
[profiles."rhel-9"]
must_have_buildrequires = []
must_not_have_buildrequires = []
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["rhel-9"]);
        assert!(!r.has_violations());
        assert!(matches!(r.per_profile[0].status, ContractProfileStatus::Pass));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let contract = r#"
not_a_real_field = true

[profiles."rhel-9"]
must_have_buildrequires = ["gcc"]
"#;
        let err = Contract::from_toml_str(contract).unwrap_err();
        assert!(matches!(err, ContractError::Toml(_)));
    }

    #[test]
    fn rich_and_dep_flattens_to_atoms() {
        // `(gcc and make)` registers both gcc and make so a contract
        // asking for either is satisfied.
        const SPEC: &str = "\
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
        let contract = r#"
[profiles."rhel-9"]
must_have_buildrequires = ["gcc", "make"]
"#;
        let r = run_report(SPEC, contract, &["rhel-9"]);
        assert!(!r.has_violations());
    }

    #[test]
    fn forbidden_present_reports_first_occurrence_surface() {
        // Duplicate `gcc` lines: first via literal, second via macro
        // alias. `verify_one`'s HashMap index must keep the *first*
        // surface form (`gcc`) so the `ForbiddenPresent.found_as`
        // field is stable across runs and operator-friendly.
        const SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: %{some_macro}-gcc

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let contract = r#"
[profiles."rhel-9"]
must_not_have_buildrequires = ["gcc"]
"#;
        let r = run_report(SPEC, contract, &["rhel-9"]);
        match &r.per_profile[0].status {
            ContractProfileStatus::Violations { violations } => {
                assert_eq!(violations.len(), 1);
                let v = &violations[0];
                let ContractViolation::ForbiddenPresent { found_as, .. } = v else {
                    panic!("expected ForbiddenPresent");
                };
                assert_eq!(
                    found_as, "gcc",
                    "first occurrence must surface — visitor order is contractual"
                );
            }
            other => panic!("expected Violations, got {other:?}"),
        }
    }

    #[test]
    fn whitespace_around_contract_entry_does_not_break_match() {
        // `"  gcc  "` in the contract must match the spec's `gcc`.
        // `verify_one` trims contract entries to keep symmetry with
        // the spec-side `record_atom_name` trimming.
        let contract = r#"
[profiles."rhel-9"]
must_have_buildrequires = ["  gcc  ", "make"]
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["rhel-9"]);
        assert!(!r.has_violations(), "trimmed entry must match");
    }

    #[test]
    fn buildrequires_inside_conditional_still_collected() {
        // MVP semantics: conditional-unaware. BuildRequires inside
        // `%if 0%{?rhel}` is found even for non-RHEL profiles.
        const SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
%if 0%{?rhel}
BuildRequires: gcc
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let contract = r#"
[profiles."altlinux-10"]
must_have_buildrequires = ["gcc"]
"#;
        let r = run_report(SPEC, contract, &["altlinux-10"]);
        // Conditional-unaware: contract passes even on alt because
        // the `BuildRequires` exists in source. Document via test.
        assert!(!r.has_violations());
    }
}
