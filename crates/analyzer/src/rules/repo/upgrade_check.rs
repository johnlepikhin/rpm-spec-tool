//! RPM-REPO-030 `new-evr-not-greater-than-repo` +
//! RPM-REPO-031 `epoch-dropped`.
//!
//! Both rules ask the same question: "is the binary we're about to
//! publish a clean upgrade of what's already in the configured repo?".
//! 030 fires when the new EVR is not strictly greater than the latest
//! published binary; 031 fires when the spec drops an explicit `Epoch:`
//! that the published binary carries. Either one would silently regress
//! consumers on `dnf upgrade`.
//!
//! Both keyed on the main package only (`Name:`) — subpackages inherit
//! the main EVR/Epoch by default, and walking them creates noisy
//! duplicate diagnostics for the same root issue.
//!
//! Arch filter: each profile carries a `build_arch` (e.g. `x86_64`);
//! we compare against repo binaries whose arch matches the profile OR
//! `noarch` (yum-compat). Cross-arch repos (e.g. mixed i686 + x86_64)
//! must not yield a false REGRESS against the wrong-arch binary.
//!
//! Both diagnostics are emitted from a single visitor pass — they
//! share the (expensive) macro-registry clone and SQLite source-name
//! lookup, so doing them separately would double the cost per spec
//! for zero clarity gain. The `additional_metadata()` hook in
//! [`Lint`] surfaces RPM-REPO-031 alongside the primary
//! RPM-REPO-030 metadata in lint listings.

use rpm_spec::ast::{Span, SpecFile};
use rpm_spec_profile::Profile;
use rpm_spec_repo_core::{NEVRA, RepoUniverse};

use crate::diagnostic::{Diagnostic, LintCategory, RepoContext, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::spec_nevr::{ArchFilter, SpecMainNevr, enriched_macros_with_spec_locals};
use crate::visit::Visit;

use super::shared::RepoRule;

pub static METADATA_030: LintMetadata = LintMetadata {
    id: "RPM-REPO-030",
    name: "new-evr-not-greater-than-repo",
    description: "The spec's EVR is not strictly greater than the highest binary already \
                  published in the configured repos. Releases would silently regress for \
                  consumers running `dnf upgrade`.",
    default_severity: Severity::Deny,
    category: LintCategory::Correctness,
};

pub static METADATA_031: LintMetadata = LintMetadata {
    id: "RPM-REPO-031",
    name: "epoch-dropped",
    description: "The spec omits an `Epoch:` value that the currently published binary \
                  carries. Dropping epoch silently demotes the package (rpm treats absent \
                  epoch as 0) and breaks `dnf upgrade`.",
    default_severity: Severity::Deny,
    category: LintCategory::Correctness,
};

static EXTRA_METADATA: &[&LintMetadata] = &[&METADATA_031];

/// Combined visitor: one walk per spec emits both RPM-REPO-030 and
/// RPM-REPO-031 diagnostics from the same `binaries_built_from` query
/// plus macro-registry clone. The two flavours diverge only at the
/// final EVR-vs-published comparison.
#[derive(Debug, Default)]
pub struct UpgradeEvrCheck {
    base: RepoRule,
}

impl UpgradeEvrCheck {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl<'ast> Visit<'ast> for UpgradeEvrCheck {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some(state) = self.base.state.as_ref() else {
            return;
        };
        let Some(profile) = self.base.profile.as_ref() else {
            return;
        };
        // Enrich the profile's macro registry with spec-local
        // `%global` / `%define` so `Name: %{prog_name}-%{edition}`
        // resolves. The spec carries its own macro definitions which
        // aren't in `profile.macros` (that's profile defaults + `-D`
        // overrides only).
        let macros = enriched_macros_with_spec_locals(spec, profile);
        let Some(spec_info) = SpecMainNevr::extract(spec, &macros) else {
            // Parser already flags missing Name/Version/Release — we
            // can't say anything useful without them.
            return;
        };
        let arch_filter = ArchFilter::from_profile(profile);

        let Some(best) = best_published(&state.universe, &spec_info.name, &arch_filter) else {
            // No published binary for this name+arch combo — this is a
            // new package. Neither lint fires.
            return;
        };

        let spec_evr = spec_info.to_evr();
        let profile_name = state.universe.profile_name.as_str();
        // `EVR` doesn't expose `Ord` (see `EVR` doc-comment for the
        // Eq/Ord-contract reasoning); compare via `compare_rpm`
        // directly. `Ordering::Greater` is the only "good" outcome
        // for an upgrade; `Less`/`Equal` both fire RPM-REPO-030.
        let evr_ord = spec_evr.compare_rpm(&best.evr());

        if !matches!(evr_ord, std::cmp::Ordering::Greater) {
            let relation = if matches!(evr_ord, std::cmp::Ordering::Equal) {
                "equal to"
            } else {
                "less than"
            };
            self.base.diagnostics.push(
                Diagnostic::new(
                    &METADATA_030,
                    METADATA_030.default_severity,
                    format!(
                        "spec EVR {spec_evr} is {relation} the latest published binary \
                         `{best}` in the configured repos; `dnf upgrade` would not advance",
                    ),
                    spec_info.evr_span,
                )
                .with_repo_context(
                    RepoContext::for_profile(profile_name).with_nevra(best.to_string()),
                ),
            );
        }

        if best.epoch > 0 {
            // Two flavours of epoch regression: omitting (spec has no
            // `Epoch:`) versus lowering (spec has explicit `Epoch: N`
            // with N < published epoch). Both demote the package.
            let msg = match spec_info.epoch {
                None => Some(format!(
                    "spec omits `Epoch:` but the published binary `{best}` has \
                     `Epoch: {}`; rpm treats absent epoch as 0, demoting the package",
                    best.epoch
                )),
                Some(n) if n < best.epoch => Some(format!(
                    "spec sets `Epoch: {n}` but the published binary `{best}` \
                     has `Epoch: {}`; lowering epoch demotes the package",
                    best.epoch
                )),
                Some(_) => None,
            };
            if let Some(msg) = msg {
                self.base.diagnostics.push(
                    Diagnostic::new(
                        &METADATA_031,
                        METADATA_031.default_severity,
                        msg,
                        spec_info.evr_span,
                    )
                    .with_repo_context(
                        RepoContext::for_profile(profile_name).with_nevra(best.to_string()),
                    ),
                );
            }
        }
    }
}

impl Lint for UpgradeEvrCheck {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA_030
    }
    fn additional_metadata(&self) -> &'static [&'static LintMetadata] {
        EXTRA_METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        self.base.take_diagnostics()
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.base.set_profile(profile);
    }
    fn set_repo_universe(
        &mut self,
        universe: Option<std::sync::Arc<RepoUniverse>>,
    ) {
        self.base.set_repo_universe(universe);
    }
}

/// Walk the (binary candidates × arch filter) and return the highest-
/// EVR binary whose name matches `spec_name`. The source_name index
/// already narrowed `candidates` to "binaries built from this spec",
/// but the repo can include subpackages with the same source_name —
/// we only compare the main binary against the main spec NEVR.
fn best_published(
    universe: &RepoUniverse,
    spec_name: &str,
    arch_filter: &ArchFilter,
) -> Option<NEVRA> {
    let candidates = match universe.binaries_built_from(spec_name) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                error = %e,
                name = spec_name,
                "repo source-name lookup failed; skipping upgrade-check",
            );
            return None;
        }
    };
    candidates
        .into_iter()
        .filter(|(_pref, n)| n.name.as_ref() == spec_name && arch_filter.matches(&n.arch))
        .map(|(_pref, n)| n)
        // `cmp_strict` — picking the highest provider EVR is a
        // candidate-vs-candidate sort; the dnf-compatible
        // `compare_rpm` short-circuits would collapse byte-different
        // providers that share a version and we'd lose precision.
        .max_by(|a, b| a.evr().cmp_strict(&b.evr()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::repo::test_fixtures::{redos_profile, tiny_universe};
    use std::sync::Arc;

    fn run_check(
        src: &str,
        profile: &Profile,
        universe: Arc<RepoUniverse>,
    ) -> Vec<Diagnostic> {
        let outcome = crate::session::parse(src);
        let mut lint = UpgradeEvrCheck::new();
        lint.set_profile(profile);
        lint.set_repo_universe(Some(universe));
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    fn universe_with(packages: Vec<rpm_spec_repo_core::Package>) -> Arc<RepoUniverse> {
        use time::OffsetDateTime;
        let repo_id: Arc<str> = Arc::from("test-repo");
        let index = rpm_spec_repo_core::RepoIndex {
            repo_id,
            revision: "rev0".into(),
            fetched_at: OffsetDateTime::now_utc(),
            packages,
            advisories: Vec::new(),
        };
        Arc::new(
            RepoUniverse::from_indexes_for_tests("test-profile", vec![Arc::new(index)])
                .expect("build in-memory universe"),
        )
    }

    fn pkg(
        name: &str,
        epoch: u32,
        version: &str,
        release: &str,
        arch: &str,
        source_rpm: &str,
    ) -> rpm_spec_repo_core::Package {
        use rpm_spec_repo_core::{NEVRA, Package, PkgChecksum};
        Package {
            nevra: NEVRA {
                name: Arc::from(name),
                epoch,
                version: Arc::from(version),
                release: Arc::from(release),
                arch: Arc::from(arch),
            },
            repo_id: Arc::from("test-repo"),
            provides: Vec::new(),
            requires: Vec::new(),
            conflicts: Vec::new(),
            obsoletes: Vec::new(),
            recommends: Vec::new(),
            suggests: Vec::new(),
            supplements: Vec::new(),
            enhances: Vec::new(),
            source_rpm: Some(Arc::from(source_rpm)),
            summary: Arc::from(""),
            size_installed: 0,
            checksum: PkgChecksum::Sha256(format!("{name}-{version}-{release}")),
            location: Arc::from(""),
            files: Vec::new(),
        }
    }

    fn spec_src(name: &str, version: &str, release: &str, epoch_line: &str) -> String {
        format!(
            "Name: {name}\n{epoch_line}Version: {version}\nRelease: {release}\n\
             Summary: s\nLicense: MIT\n%description\nx\n",
        )
    }

    #[test]
    fn evr_greater_silent() {
        let uni = universe_with(vec![pkg(
            "foo",
            0,
            "1.0",
            "1",
            "x86_64",
            "foo-1.0-1.src.rpm",
        )]);
        let src = spec_src("foo", "1.1", "1", "");
        let diags = run_check(&src, &redos_profile(), uni);
        let d030: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM-REPO-030").collect();
        assert!(d030.is_empty(), "expected no 030, got {d030:?}");
    }

    #[test]
    fn evr_equal_flags() {
        let uni = universe_with(vec![pkg(
            "foo",
            0,
            "1.0",
            "1",
            "x86_64",
            "foo-1.0-1.src.rpm",
        )]);
        let src = spec_src("foo", "1.0", "1", "");
        let diags = run_check(&src, &redos_profile(), uni);
        let d030: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM-REPO-030").collect();
        assert_eq!(d030.len(), 1, "{diags:?}");
        assert!(d030[0].message.contains("equal to"), "{}", d030[0].message);
    }

    #[test]
    fn evr_less_flags_as_regress() {
        let uni = universe_with(vec![pkg(
            "foo",
            0,
            "2.0",
            "1",
            "x86_64",
            "foo-2.0-1.src.rpm",
        )]);
        let src = spec_src("foo", "1.5", "1", "");
        let diags = run_check(&src, &redos_profile(), uni);
        let d030: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM-REPO-030").collect();
        assert_eq!(d030.len(), 1, "{diags:?}");
        assert!(d030[0].message.contains("less than"), "{}", d030[0].message);
    }

    #[test]
    fn new_package_silent() {
        let uni = universe_with(vec![pkg(
            "bar",
            0,
            "1.0",
            "1",
            "x86_64",
            "bar-1.0-1.src.rpm",
        )]);
        let src = spec_src("foo", "1.0", "1", "");
        let diags = run_check(&src, &redos_profile(), uni);
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn wrong_arch_ignored() {
        // Published binary is i686, redos profile builds x86_64. The
        // i686 binary must not count toward the comparison.
        let uni = universe_with(vec![pkg(
            "foo",
            0,
            "9.9",
            "1",
            "i686",
            "foo-9.9-1.src.rpm",
        )]);
        let src = spec_src("foo", "1.0", "1", "");
        let diags = run_check(&src, &redos_profile(), uni);
        assert!(diags.is_empty(), "wrong-arch binary should not count: {diags:?}");
    }

    #[test]
    fn noarch_counted_against_x86_64_profile() {
        let uni = universe_with(vec![pkg(
            "foo",
            0,
            "9.9",
            "1",
            "noarch",
            "foo-9.9-1.src.rpm",
        )]);
        let src = spec_src("foo", "1.0", "1", "");
        let diags = run_check(&src, &redos_profile(), uni);
        let d030: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM-REPO-030").collect();
        assert_eq!(d030.len(), 1, "noarch binary should count: {diags:?}");
    }

    #[test]
    fn epoch_dropped_flags() {
        let uni = universe_with(vec![pkg(
            "foo",
            2,
            "1.0",
            "1",
            "x86_64",
            "foo-1.0-1.src.rpm",
        )]);
        let src = spec_src("foo", "1.0", "2", "");
        let diags = run_check(&src, &redos_profile(), uni);
        let d031: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM-REPO-031").collect();
        assert_eq!(d031.len(), 1, "{diags:?}");
        assert!(d031[0].message.contains("Epoch: 2"), "{}", d031[0].message);
    }

    #[test]
    fn epoch_lowered_flags() {
        let uni = universe_with(vec![pkg(
            "foo",
            3,
            "1.0",
            "1",
            "x86_64",
            "foo-1.0-1.src.rpm",
        )]);
        let src = "Name: foo\nEpoch: 1\nVersion: 1.0\nRelease: 1\n\
             Summary: s\nLicense: MIT\n%description\nx\n";
        let diags = run_check(src, &redos_profile(), uni);
        let d031: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM-REPO-031").collect();
        assert_eq!(d031.len(), 1, "{diags:?}");
        assert!(d031[0].message.contains("lowering epoch"), "{}", d031[0].message);
    }

    #[test]
    fn epoch_match_silent() {
        let uni = universe_with(vec![pkg(
            "foo",
            1,
            "1.0",
            "1",
            "x86_64",
            "foo-1.0-1.src.rpm",
        )]);
        let src = "Name: foo\nEpoch: 1\nVersion: 1.0\nRelease: 2\n\
             Summary: s\nLicense: MIT\n%description\nx\n";
        let diags = run_check(src, &redos_profile(), uni);
        let d031: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM-REPO-031").collect();
        assert!(d031.is_empty(), "{diags:?}");
    }

    #[test]
    fn silent_when_universe_missing() {
        let src = spec_src("foo", "1.0", "1", "");
        let outcome = crate::session::parse(&src);
        let mut lint = UpgradeEvrCheck::new();
        lint.set_profile(&redos_profile());
        lint.set_repo_universe(None);
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }

    #[test]
    fn silent_when_no_name_in_spec() {
        let src = "Version: 1.0\nRelease: 1\nSummary: s\nLicense: MIT\n%description\nx\n";
        let outcome = crate::session::parse(src);
        let mut lint = UpgradeEvrCheck::new();
        lint.set_profile(&redos_profile());
        lint.set_repo_universe(Some(tiny_universe()));
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }

    #[test]
    fn picks_highest_evr_among_multiple_releases() {
        let uni = universe_with(vec![
            pkg("foo", 0, "1.0", "1", "x86_64", "foo-1.0-1.src.rpm"),
            pkg("foo", 0, "1.5", "3", "x86_64", "foo-1.5-3.src.rpm"),
            pkg("foo", 0, "1.2", "2", "x86_64", "foo-1.2-2.src.rpm"),
        ]);
        let src = spec_src("foo", "1.3", "1", "");
        let diags = run_check(&src, &redos_profile(), uni);
        let d030: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM-REPO-030").collect();
        assert_eq!(d030.len(), 1, "{diags:?}");
        assert!(d030[0].message.contains("foo-1.5-3"), "{}", d030[0].message);
    }
}
