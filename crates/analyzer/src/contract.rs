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
//! The verifier is **branch-aware**: a `BuildRequires:` line inside
//! `%if 0%{?fedora}` does NOT satisfy a contract that asks for the
//! dep on a RHEL profile, because the evaluator marks that branch
//! Inactive on RHEL and the body is pruned during collection.
//!
//! ## Two-walk semantics
//!
//! Each profile gets two collections via [`crate::branch_aware`]:
//!
//! * **required-side** uses [`IndeterminatePolicy::Skip`]: a dep
//!   inside an indeterminate branch does NOT count toward
//!   `must_have_buildrequires`. Conservative — prefer reporting
//!   "missing required" only when the dep is definitely missing.
//! * **forbidden-side** uses [`IndeterminatePolicy::Include`]: a dep
//!   inside an indeterminate branch DOES count toward
//!   `must_not_have_buildrequires`. Conservative the other way —
//!   prefer reporting "forbidden present" if the dep might be there.
//!
//! Net effect: the verifier never overlooks a forbidden dep that
//! could reach the build, and never spuriously flags a required dep
//! that is only declared inside an undecidable branch. Indeterminate
//! branches degrade gracefully toward "fail loud, not silent".
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

use rpm_spec::ast::{PreambleItem, Span, SpecFile, Tag, TagValue, Text};
use rpm_spec_profile::ResolvedTargetSet;
use serde::Serialize;

use crate::branch_aware::{IndeterminatePolicy, ProfileBranchSelection, walk_active_preamble};
use crate::branch_coverage::CoverageReport;
use crate::dep_walk::{for_each_dep_atom, render_text_with_macros};

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
    /// Walks the spec branch-aware per profile, with two independent
    /// projections: `Skip`-policy for the required-side check and
    /// `Include`-policy for the forbidden-side check. See the module
    /// doc for the rationale.
    ///
    /// `target_set` supplies both the profile-ID order (mirrored into
    /// the report's `per_profile` for stable column alignment) and
    /// the macro registries the [`CoverageReport`] needs to evaluate
    /// each branch's condition.
    pub fn compute(
        spec: &SpecFile<Span>,
        contract: &Contract,
        target_set: &ResolvedTargetSet,
        bcond_overrides: &crate::bcond::BcondOverrides,
    ) -> Self {
        // CoverageReport is profile-agnostic per spec — one pass over
        // the AST per call site. Sharing it across the per-profile
        // loop is the whole point of the report's existence. The
        // bcond overrides are forwarded so contract verdicts honour
        // `--with FOO` / `--without FOO` on the CLI.
        let coverage = CoverageReport::compute(spec, target_set, bcond_overrides);

        let per_profile = target_set
            .targets
            .iter()
            .map(|rt| {
                let profile_id = rt.profile_id.as_str();
                let status = match contract.profiles.get(profile_id) {
                    None => ContractProfileStatus::NoContract,
                    Some(pc) => {
                        let required_found = collect_branch_aware(
                            spec,
                            &coverage,
                            profile_id,
                            IndeterminatePolicy::Skip,
                        );
                        let forbidden_found = collect_branch_aware(
                            spec,
                            &coverage,
                            profile_id,
                            IndeterminatePolicy::Include,
                        );
                        verify_one(pc, &required_found, &forbidden_found)
                    }
                };
                ProfileContractReport {
                    profile_id: rt.profile_id.clone(),
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

/// Collect every `BuildRequires` atom active on `profile_id` under
/// `policy`. Reuses the [`BuildRequiresCollector`] logic but feeds
/// it only items inside active branches via [`walk_active_preamble`].
fn collect_branch_aware(
    spec: &SpecFile<Span>,
    coverage: &CoverageReport,
    profile_id: &str,
    policy: IndeterminatePolicy,
) -> Vec<FoundBuildRequire> {
    let selection = ProfileBranchSelection::compute(coverage, profile_id, policy);
    let mut collector = BuildRequiresCollector::default();
    walk_active_preamble(spec, &selection, |item| {
        collector.consume_preamble_item(item)
    });
    collector.found
}

fn verify_one(
    pc: &ProfileContract,
    required_found: &[FoundBuildRequire],
    forbidden_found: &[FoundBuildRequire],
) -> ContractProfileStatus {
    // Two independent HashMaps so the required-check sees only deps
    // collected under Skip policy and the forbidden-check sees the
    // permissive Include-policy set. See module doc for the rationale.
    let required_index: HashMap<&str, &FoundBuildRequire> = required_found.iter().fold(
        HashMap::with_capacity(required_found.len()),
        |mut acc, f| {
            acc.entry(f.canonical.as_str()).or_insert(f);
            acc
        },
    );
    let forbidden_index: HashMap<&str, &FoundBuildRequire> = forbidden_found.iter().fold(
        HashMap::with_capacity(forbidden_found.len()),
        |mut acc, f| {
            acc.entry(f.canonical.as_str()).or_insert(f);
            acc
        },
    );

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
        if !required_index.contains_key(key) {
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
        if let Some(found_match) = forbidden_index.get(key) {
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
    /// Entry point called by [`walk_active_preamble`] for each
    /// preamble item that survives branch projection. Filters on
    /// `Tag::BuildRequires` and records the dep atoms.
    fn consume_preamble_item(&mut self, item: &PreambleItem<Span>) {
        if matches!(item.tag, Tag::BuildRequires) {
            if let TagValue::Dep(dep) = &item.value {
                // Delegate rich-dep traversal to the shared walker
                // in `crate::dep_walk` so the And/Or flattening
                // and If/Unless/Not skip policy stay aligned with
                // `matrix diff` and any future consumer.
                for_each_dep_atom(dep, |name| self.record_atom_name(name));
            }
        }
    }

    fn record_atom_name(&mut self, name: &Text) {
        // Prefer the pure-literal projection when the dep name has
        // no macro segments — that's the canonical key contracts
        // match against. For macro-bearing names (`%{name}-devel`)
        // we fall back to the with-macros rendering so the canonical
        // and the surface form coincide; the contract author can
        // still match it but must write the verbatim form.
        let surface = render_text_with_macros(name);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn target_set_with(profiles: &[&str]) -> ResolvedTargetSet {
        use rpm_spec_profile::{ProfileSection, ResolveOptions, TargetEntry, resolve_target_set};
        let section = ProfileSection::new(None, std::collections::BTreeMap::new());
        let target = TargetEntry::from_profiles(profiles.iter().map(|s| s.to_string()).collect());
        resolve_target_set(
            &section,
            "test",
            &target,
            std::path::Path::new("/tmp"),
            ResolveOptions::default(),
        )
        .expect("resolve")
    }

    fn run_report(spec_src: &str, contract_toml: &str, profile_ids: &[&str]) -> ContractReport {
        let parsed = parse(spec_src);
        let contract = Contract::from_toml_str(contract_toml).expect("parse contract");
        let ts = target_set_with(profile_ids);
        ContractReport::compute(
            &parsed.spec,
            &contract,
            &ts,
            &crate::bcond::BcondOverrides::default(),
        )
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
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc", "make"]
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["rhel-9-x86_64"]);
        assert!(!r.has_violations());
        match &r.per_profile[0].status {
            ContractProfileStatus::Pass => {}
            other => panic!("expected Pass, got {other:?}"),
        }
    }

    #[test]
    fn contract_fails_on_missing_required() {
        let contract = r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc", "missing-pkg"]
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["rhel-9-x86_64"]);
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
[profiles."rhel-9-x86_64"]
must_not_have_buildrequires = ["gcc"]
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["rhel-9-x86_64"]);
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
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc"]
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["altlinux-10-x86_64"]);
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
[profiles."rhel-9-x86_64"]
must_have_buildrequires = []
must_not_have_buildrequires = []
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["rhel-9-x86_64"]);
        assert!(!r.has_violations());
        assert!(matches!(
            r.per_profile[0].status,
            ContractProfileStatus::Pass
        ));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let contract = r#"
not_a_real_field = true

[profiles."rhel-9-x86_64"]
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
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc", "make"]
"#;
        let r = run_report(SPEC, contract, &["rhel-9-x86_64"]);
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
[profiles."rhel-9-x86_64"]
must_not_have_buildrequires = ["gcc"]
"#;
        let r = run_report(SPEC, contract, &["rhel-9-x86_64"]);
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
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["  gcc  ", "make"]
"#;
        let r = run_report(SPEC_WITH_GCC, contract, &["rhel-9-x86_64"]);
        assert!(!r.has_violations(), "trimmed entry must match");
    }

    #[test]
    fn buildrequires_in_rhel_branch_invisible_to_alt_profile() {
        // Branch-aware semantics: `BuildRequires: gcc` inside
        // `%if 0%{?rhel}` is Active only on rhel-* profiles. An
        // altlinux profile that asks for gcc as required must see
        // it as MissingRequired — the dep is gated away on that
        // profile. The old MVP swallowed this; the upgrade flips
        // it to the honest answer.
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
[profiles."altlinux-10-x86_64"]
must_have_buildrequires = ["gcc"]
"#;
        let r = run_report(SPEC, contract, &["altlinux-10-x86_64"]);
        assert!(
            r.has_violations(),
            "alt profile must see gcc as missing — it is gated on %if 0%{{?rhel}}"
        );
        match &r.per_profile[0].status {
            ContractProfileStatus::Violations { violations } => {
                assert!(
                    violations.iter().any(|v| matches!(
                        v,
                        ContractViolation::MissingRequired { package } if package == "gcc"
                    )),
                    "expected MissingRequired(gcc); got {violations:?}"
                );
            }
            other => panic!("expected Violations, got {other:?}"),
        }
    }

    #[test]
    fn buildrequires_in_rhel_branch_visible_on_rhel_profile() {
        // Mirror image of the above: gcc IS active on rhel, so the
        // same contract passes there.
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
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc"]
"#;
        let r = run_report(SPEC, contract, &["rhel-9-x86_64"]);
        assert!(!r.has_violations(), "rhel profile must see gcc as active");
        assert!(matches!(
            r.per_profile[0].status,
            ContractProfileStatus::Pass
        ));
    }

    #[test]
    fn forbidden_dep_in_indeterminate_branch_still_flagged() {
        // Indeterminate branches under Include policy (forbidden-side):
        // a forbidden dep that COULD reach the build must surface as a
        // violation. Use `%if 1 + 2 == 3` arithmetic Raw → always
        // Indeterminate. The forbidden check uses Include policy so
        // the dep DOES count.
        const SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
%if 1 + 2 == 3
BuildRequires: banned-pkg
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let contract = r#"
[profiles."rhel-9-x86_64"]
must_not_have_buildrequires = ["banned-pkg"]
"#;
        let r = run_report(SPEC, contract, &["rhel-9-x86_64"]);
        assert!(
            r.has_violations(),
            "forbidden dep in an indeterminate branch must be flagged"
        );
    }

    #[test]
    fn else_branch_supplies_required_dep_on_inactive_profile() {
        // Reciprocal of `buildrequires_in_rhel_branch_invisible_to_alt_profile`:
        // when `%if 0%{?rhel}` is Inactive on alt, the `%else` body
        // runs → its BR satisfies a contract that asks for the dep
        // on alt. Pins SelectedBody::Otherwise correctness.
        const SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
%if 0%{?rhel}
BuildRequires: rhel-pkg
%else
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
[profiles."altlinux-10-x86_64"]
must_have_buildrequires = ["gcc"]
"#;
        let r = run_report(SPEC, contract, &["altlinux-10-x86_64"]);
        assert!(
            !r.has_violations(),
            "alt profile must see gcc via %else body; got {:?}",
            r.per_profile[0].status
        );
    }

    #[test]
    fn skip_and_include_binding_is_correct() {
        // Catch a Skip↔Include swap bug in `ContractReport::compute`
        // by exercising BOTH semantics in one test: a forbidden dep
        // in an indeterminate branch (must surface via Include path)
        // AND a required dep in another indeterminate branch (must
        // surface as missing via Skip path).
        const SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
%if 1 + 2 == 3
BuildRequires: banned-pkg
%endif
%if 4 + 5 == 9
BuildRequires: maybe-required
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let contract = r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["maybe-required"]
must_not_have_buildrequires = ["banned-pkg"]
"#;
        let r = run_report(SPEC, contract, &["rhel-9-x86_64"]);
        match &r.per_profile[0].status {
            ContractProfileStatus::Violations { violations } => {
                let has_missing = violations.iter().any(|v| matches!(
                    v, ContractViolation::MissingRequired { package } if package == "maybe-required"
                ));
                let has_forbidden = violations.iter().any(|v| matches!(
                    v, ContractViolation::ForbiddenPresent { package, .. } if package == "banned-pkg"
                ));
                assert!(
                    has_missing,
                    "required-side Skip must hide indeterminate dep → MissingRequired; got {violations:?}"
                );
                assert!(
                    has_forbidden,
                    "forbidden-side Include must surface indeterminate dep → ForbiddenPresent; got {violations:?}"
                );
            }
            other => panic!("expected Violations, got {other:?}"),
        }
    }

    #[test]
    fn required_dep_only_in_indeterminate_branch_is_missing() {
        // Mirror of the above: required-side uses Skip policy, so a
        // dep that ONLY appears inside an indeterminate branch is NOT
        // collected for required-check → reports as MissingRequired.
        // Conservative: prefer false positive ("missing") over false
        // negative ("ok") when we genuinely don't know.
        const SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
%if 1 + 2 == 3
BuildRequires: maybe-gcc
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let contract = r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["maybe-gcc"]
"#;
        let r = run_report(SPEC, contract, &["rhel-9-x86_64"]);
        assert!(
            r.has_violations(),
            "required-side Skip policy must hide indeterminate dep"
        );
    }
}
