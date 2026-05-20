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

use std::sync::Arc;

use rpm_spec::ast::{ConditionalMacro, PreambleItem, Span, SpecFile, Tag, TagValue, TextSegment};
use rpm_spec_profile::{MacroEntry, MacroRegistry, Profile, Provenance};
use rpm_spec_repo_core::{EVR, NEVRA, RepoUniverse};

use crate::bcond::{BcondMap, BcondOverrides};
use crate::spec_locals::scan_spec_locals;

use super::shared::MACRO_EXPAND_DEPTH;

use crate::diagnostic::{Diagnostic, LintCategory, RepoContext, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::rules::util::preamble::collect_top_level_preamble;
use crate::visit::Visit;

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

/// Combined struct since both lints walk the same preamble + repo query.
/// Splitting into two visitor passes would double the SQLite work for
/// zero clarity gain.
#[derive(Debug, Default)]
pub struct UpgradeEvrCheck {
    diagnostics: Vec<Diagnostic>,
    profile: Option<Profile>,
    universe: Option<Arc<RepoUniverse>>,
    /// Which of the two lints this instance represents — needed so each
    /// instance can carry its own metadata and the registry can register
    /// two separate entries without a struct-level discriminator at the
    /// visitor's hot path.
    which: WhichRule,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
enum WhichRule {
    #[default]
    EvrNotGreater,
    EpochDropped,
}

impl UpgradeEvrCheck {
    #[must_use]
    pub fn new_evr_not_greater() -> Self {
        Self {
            which: WhichRule::EvrNotGreater,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn new_epoch_dropped() -> Self {
        Self {
            which: WhichRule::EpochDropped,
            ..Self::default()
        }
    }

    fn metadata(&self) -> &'static LintMetadata {
        match self.which {
            WhichRule::EvrNotGreater => &METADATA_030,
            WhichRule::EpochDropped => &METADATA_031,
        }
    }
}

impl<'ast> Visit<'ast> for UpgradeEvrCheck {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        let Some(profile) = &self.profile else {
            return;
        };
        let Some(universe) = &self.universe else {
            return;
        };
        // Enrich the profile's macro registry with spec-local
        // `%global` / `%define` so `Name: %{prog_name}-%{edition}`
        // resolves. The spec carries its own macro definitions which
        // aren't in `profile.macros` (that's profile defaults + `-D`
        // overrides only).
        let macros = enriched_macros_with_spec_locals(spec, profile);
        let Some(spec_info) = SpecMainNevr::from(spec, &macros) else {
            // Parser already flags missing Name/Version/Release — we
            // can't say anything useful without them.
            return;
        };
        let arches = upgrade_arch_filter(profile);

        let candidates = match universe.binaries_built_from(&spec_info.name) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    name = ?spec_info.name,
                    "repo source-name lookup failed; skipping upgrade-check",
                );
                return;
            }
        };
        // Filter to (arch matches profile OR noarch) AND (binary name == spec name).
        // The source_name index already narrows to "binaries built from
        // this spec", but the repo can include subpackages with the same
        // source_name — we only compare the main binary against the
        // main spec NEVR.
        let mut best: Option<NEVRA> = None;
        for (_pref, nevra) in &candidates {
            if nevra.name.as_ref() != spec_info.name {
                continue;
            }
            if !upgrade_arch_matches(&nevra.arch, &arches) {
                continue;
            }
            if best
                .as_ref()
                .is_none_or(|cur| nevra.evr() > cur.evr())
            {
                best = Some(nevra.clone());
            }
        }
        let Some(best) = best else {
            // No published binary for this name+arch combo — this is a
            // new package. Neither lint fires.
            return;
        };

        let spec_evr = spec_info.to_evr();
        match self.which {
            WhichRule::EvrNotGreater => {
                if spec_evr > best.evr() {
                    return;
                }
                let relation = if spec_evr == best.evr() { "equal to" } else { "less than" };
                self.diagnostics.push(
                    Diagnostic::new(
                        self.metadata(),
                        self.metadata().default_severity,
                        format!(
                            "spec EVR {spec_evr} is {relation} the latest published binary \
                             `{best}` in the configured repos; `dnf upgrade` would not advance",
                        ),
                        spec_info.evr_span,
                    )
                    .with_repo_context(
                        RepoContext::for_profile(&universe.profile_name)
                            .with_nevra(best.to_string()),
                    ),
                );
            }
            WhichRule::EpochDropped => {
                if best.epoch == 0 {
                    return;
                }
                // Two flavours of epoch regression: omitting (spec has
                // no `Epoch:`) versus lowering (spec has explicit
                // `Epoch: N` with N < published epoch). `Epoch: 0`
                // explicit is also "lower than" published `Epoch: N>0`
                // and fires the lowered branch — same hazard.
                let (msg, span) = match spec_info.epoch {
                    None => (
                        format!(
                            "spec omits `Epoch:` but the published binary `{best}` has \
                             `Epoch: {}`; rpm treats absent epoch as 0, demoting the package",
                            best.epoch
                        ),
                        spec_info.evr_span,
                    ),
                    Some(n) if n < best.epoch => (
                        format!(
                            "spec sets `Epoch: {n}` but the published binary `{best}` \
                             has `Epoch: {}`; lowering epoch demotes the package",
                            best.epoch
                        ),
                        spec_info.evr_span,
                    ),
                    Some(_) => return,
                };
                self.diagnostics.push(
                    Diagnostic::new(
                        self.metadata(),
                        self.metadata().default_severity,
                        msg,
                        span,
                    )
                    .with_repo_context(
                        RepoContext::for_profile(&universe.profile_name)
                            .with_nevra(best.to_string()),
                    ),
                );
            }
        }
    }
}

impl Lint for UpgradeEvrCheck {
    fn metadata(&self) -> &'static LintMetadata {
        UpgradeEvrCheck::metadata(self)
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
    fn set_profile(&mut self, profile: &Profile) {
        self.profile = Some(profile.clone());
    }
    fn set_repo_universe(&mut self, universe: Option<Arc<RepoUniverse>>) {
        self.universe = universe;
    }
}

/// Main-package NEVR projected from the top-level preamble.
///
/// Public so `matrix upgrade-sim` (in the cli crate) can use the
/// exact same extraction logic — both the lints and the CLI must
/// agree on "what is the spec's main NEVR" or operators see confusing
/// disagreements between `matrix check` and `matrix upgrade-sim`.
///
/// `epoch` is `Option<u32>`: `None` when the spec omits `Epoch:`,
/// `Some(0)` when the spec writes `Epoch: 0` explicitly. rpm treats
/// these equivalently for *ordering*, but the 031 lint cares whether
/// the user wrote it (the diagnostic phrases "omits `Epoch:`"
/// differently from "has `Epoch: 0`"). [`Self::epoch_for_ordering`]
/// collapses both forms to `0` for callers that just want EVR cmp.
#[derive(Debug)]
#[non_exhaustive]
pub struct SpecMainNevr {
    pub name: String,
    pub epoch: Option<u32>,
    pub version: String,
    pub release: String,
    /// Anchor for diagnostics — points at the spec's `Version:` line so
    /// both 030 and 031 underline the same place the user authored.
    pub evr_span: Span,
}

impl SpecMainNevr {
    /// Extract the main NEVR from `spec`'s top-level preamble, expanding
    /// any `%{macro}` references via `macros`. Real-world specs almost
    /// always parameterise `Version:` / `Release:` through macros
    /// (`Version: %{krb5_version}`), so the literal-only path of the
    /// previous implementation made this function useless on every
    /// shipping spec; the [`MacroRegistry`] arg is therefore mandatory.
    ///
    /// Returns `None` when `Name:` / `Version:` / `Release:` are
    /// missing OR cannot be resolved to literal strings (e.g. macro
    /// chain failed to terminate at a literal).
    pub fn from(spec: &SpecFile<Span>, macros: &MacroRegistry) -> Option<Self> {
        let mut name: Option<String> = None;
        let mut version: Option<(String, Span)> = None;
        let mut release: Option<String> = None;
        let mut epoch: Option<u32> = None;
        for item in collect_top_level_preamble(spec) {
            match item.tag {
                Tag::Name => {
                    if let Some(t) = item_text(item, macros) {
                        name = Some(t);
                    }
                }
                Tag::Version => {
                    if let Some(t) = item_text(item, macros) {
                        version = Some((t, item.data));
                    }
                }
                Tag::Release => {
                    if let Some(t) = item_text(item, macros) {
                        release = Some(t);
                    }
                }
                Tag::Epoch => {
                    // `TagValue::Number` covers the common `Epoch: 1`
                    // form; macro-valued epochs flow through
                    // `TagValue::Text` and get a single macro-expansion
                    // shot via `item_text`. Unparseable values → still
                    // `None` (matches rpm's "epoch absent" fallback for
                    // syntactically broken declarations).
                    epoch = match &item.value {
                        TagValue::Number(n) => Some(*n),
                        TagValue::Text(_) => item_text(item, macros)
                            .and_then(|t| t.trim().parse::<u32>().ok()),
                        _ => None,
                    };
                }
                _ => {}
            }
        }
        let (version_text, evr_span) = version?;
        Some(Self {
            name: name?,
            epoch,
            version: version_text,
            release: release?,
            evr_span,
        })
    }

    /// Epoch value to use when comparing this NEVR via rpm vercmp.
    /// `None`/absent collapses to `0` — same convention rpm itself
    /// uses (`rpm -q --qf '%{EPOCH}'` prints `(none)` but ordering
    /// treats it as zero).
    #[must_use]
    pub fn epoch_for_ordering(&self) -> u32 {
        self.epoch.unwrap_or(0)
    }

    #[must_use]
    pub fn to_evr(&self) -> EVR {
        EVR::new(Some(self.epoch_for_ordering()), &self.version, &self.release)
    }
}

/// Clone the profile's macro registry and overlay every `%global` /
/// `%define` declared in the spec's active branches. Without this,
/// every real-world spec fails `SpecMainNevr::from` because `Name:
/// %{prog_name}-%{edition}` (typical postgres-style spec) refers to
/// macros declared via `%global` at the top of the spec — which the
/// profile registry doesn't know about.
///
/// Profile / `-D`-override values win over spec defaults (same
/// precedence policy [`scan_spec_locals`] uses internally), so an
/// operator overriding `pgsql_major` from the CLI keeps control.
pub fn enriched_macros_with_spec_locals(spec: &SpecFile<Span>, profile: &Profile) -> MacroRegistry {
    let bcond = BcondMap::from_spec(spec, &BcondOverrides::default());
    let locals = scan_spec_locals(spec, profile, &bcond);
    let mut macros = profile.macros.clone();
    for (name, value) in locals {
        if macros.get(&name).is_some() {
            continue;
        }
        macros.insert(name, MacroEntry::literal(value, Provenance::Override));
    }
    macros
}

fn item_text(item: &PreambleItem<Span>, macros: &MacroRegistry) -> Option<String> {
    match &item.value {
        TagValue::Text(t) => resolve_text_segments(&t.segments, macros),
        TagValue::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Walk a `Text`'s structured segments and concatenate them into a
/// single resolved `String`. Each segment is either:
/// * `Literal(s)` — pushed verbatim.
/// * `Macro(MacroRef { name, conditional: None, .. })` — expanded via
///   the registry; returns `None` on unresolvable.
/// * `Macro(MacroRef { name, conditional: IfDefined, .. })` —
///   `%{?dist}`: value if defined, empty if not.
/// * `Macro(MacroRef { name, conditional: IfNotDefined, .. })` —
///   `%{!?dist}`: value if NOT defined, empty if defined (rare for
///   tag text, supported for completeness).
///
/// Returns `None` if any unconditional macro fails to resolve to a
/// literal — same conservative-skip policy as the rest of the
/// RPM-REPO-* family. The structured walk replaces the old
/// `literal_str()` shortcut which returned `None` the moment a
/// segment was anything other than `Literal`, making the function
/// useless on every real-world `Name: %{a}-%{b}` declaration.
fn resolve_text_segments(segments: &[TextSegment], macros: &MacroRegistry) -> Option<String> {
    let mut out = String::new();
    for seg in segments {
        match seg {
            TextSegment::Literal(s) => out.push_str(s),
            TextSegment::Macro(m) => match m.conditional {
                ConditionalMacro::None => {
                    let value = macros.expand_to_literal(&m.name, MACRO_EXPAND_DEPTH)?;
                    out.push_str(&value);
                }
                ConditionalMacro::IfDefined => {
                    if macros.get(&m.name).is_some()
                        && let Some(value) =
                            macros.expand_to_literal(&m.name, MACRO_EXPAND_DEPTH)
                    {
                        out.push_str(&value);
                    }
                    // Undefined → empty, same as rpm `%{?dist}` on a
                    // host where `dist` was never `%define`d.
                }
                ConditionalMacro::IfNotDefined => {
                    if macros.get(&m.name).is_none() {
                        // The macro is undefined; `%{!?foo}` traditionally
                        // expands to nothing in rpm too (the body form
                        // `%{!?foo:literal}` is more common but uses a
                        // different parse). Keep empty here.
                    }
                }
                // ConditionalMacro is non-exhaustive in upstream
                // rpm-spec; any new variant defaults to skip so we
                // don't silently inject wrong output.
                _ => return None,
            },
            // Any other segment kind (e.g. interpolated expressions
            // future-added to TextSegment) — conservative skip.
            _ => return None,
        }
    }
    Some(out)
}

/// Architectures we consider a "match" for repo lookups, given a
/// `Profile`: the profile's `build_arch` plus the always-installable
/// `noarch`. Empty `build_arch` → single-entry `["noarch"]`; callers
/// must pair this with [`upgrade_arch_matches`] which interprets the
/// single-entry shape as "match everything" (don't false-skip the
/// upgrade check just because the profile didn't declare its arch).
///
/// Public so the CLI's `matrix upgrade-sim` runs the same arch
/// filter as the 030/031 lints (preventing one verdict from
/// disagreeing with the other on the exact same input).
#[must_use]
pub fn upgrade_arch_filter(profile: &Profile) -> Vec<String> {
    let mut out = Vec::with_capacity(2);
    if let Some(arch) = profile.arch.build_arch.as_deref() {
        out.push(arch.to_string());
    }
    out.push("noarch".to_string());
    out
}

/// `true` iff `arch` matches the filter built by
/// [`upgrade_arch_filter`]. Single-entry filters (no `build_arch`
/// declared on the profile) match everything.
#[must_use]
pub fn upgrade_arch_matches(arch: &str, filter: &[String]) -> bool {
    if filter.len() == 1 {
        return true;
    }
    filter.iter().any(|a| a == arch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::repo::test_fixtures::{redos_profile, tiny_universe};

    /// Both rules share one visitor pattern but expose two metadata
    /// entries, so the test helper runs them in parallel and merges
    /// diagnostics. Mirrors the way the analyzer session itself
    /// invokes both rule instances per spec.
    fn run_both(
        src: &str,
        profile: &Profile,
        universe: Arc<RepoUniverse>,
    ) -> Vec<Diagnostic> {
        let outcome = crate::session::parse(src);
        let mut diags = Vec::new();
        for mut lint in [
            UpgradeEvrCheck::new_evr_not_greater(),
            UpgradeEvrCheck::new_epoch_dropped(),
        ] {
            lint.set_profile(profile);
            lint.set_repo_universe(Some(universe.clone()));
            lint.visit_spec(&outcome.spec);
            diags.extend(lint.take_diagnostics());
        }
        diags
    }

    /// Build a tiny universe with a single published binary so the test
    /// covers the upgrade vs regress branches without touching the
    /// per-profile redos fixture (which doesn't pin specific NEVRAs).
    /// Returns an `Arc<RepoUniverse>` ready for `set_repo_universe`.
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
        let diags = run_both(&src, &redos_profile(), uni);
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
        let diags = run_both(&src, &redos_profile(), uni);
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
        let diags = run_both(&src, &redos_profile(), uni);
        let d030: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM-REPO-030").collect();
        assert_eq!(d030.len(), 1, "{diags:?}");
        assert!(d030[0].message.contains("less than"), "{}", d030[0].message);
    }

    #[test]
    fn new_package_silent() {
        // foo has never been published — both lints quiet.
        let uni = universe_with(vec![pkg(
            "bar",
            0,
            "1.0",
            "1",
            "x86_64",
            "bar-1.0-1.src.rpm",
        )]);
        let src = spec_src("foo", "1.0", "1", "");
        let diags = run_both(&src, &redos_profile(), uni);
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
        let diags = run_both(&src, &redos_profile(), uni);
        assert!(diags.is_empty(), "wrong-arch binary should not count: {diags:?}");
    }

    #[test]
    fn noarch_counted_against_x86_64_profile() {
        // noarch is yum-installable on any arch — the profile MUST
        // honour it. Without that, a noarch binary published at higher
        // version slips past the lint.
        let uni = universe_with(vec![pkg(
            "foo",
            0,
            "9.9",
            "1",
            "noarch",
            "foo-9.9-1.src.rpm",
        )]);
        let src = spec_src("foo", "1.0", "1", "");
        let diags = run_both(&src, &redos_profile(), uni);
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
        let src = spec_src("foo", "1.0", "2", ""); // no Epoch, higher release
        let diags = run_both(&src, &redos_profile(), uni);
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
        let diags = run_both(src, &redos_profile(), uni);
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
        let diags = run_both(src, &redos_profile(), uni);
        let d031: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM-REPO-031").collect();
        assert!(d031.is_empty(), "{diags:?}");
    }

    #[test]
    fn silent_when_universe_missing() {
        // No universe wired up — both lints must be silent.
        let src = spec_src("foo", "1.0", "1", "");
        let outcome = crate::session::parse(&src);
        let mut lint = UpgradeEvrCheck::new_evr_not_greater();
        lint.set_profile(&redos_profile());
        lint.set_repo_universe(None);
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }

    #[test]
    fn silent_when_no_name_in_spec() {
        // Missing Name: → parser already complains, RPM-REPO-030/031 stay quiet.
        let src = "Version: 1.0\nRelease: 1\nSummary: s\nLicense: MIT\n%description\nx\n";
        let outcome = crate::session::parse(src);
        let mut lint = UpgradeEvrCheck::new_evr_not_greater();
        lint.set_profile(&redos_profile());
        lint.set_repo_universe(Some(tiny_universe()));
        lint.visit_spec(&outcome.spec);
        assert!(lint.take_diagnostics().is_empty());
    }

    #[test]
    fn picks_highest_evr_among_multiple_releases() {
        // Two published binaries; the lint must pick the highest EVR.
        let uni = universe_with(vec![
            pkg("foo", 0, "1.0", "1", "x86_64", "foo-1.0-1.src.rpm"),
            pkg("foo", 0, "1.5", "3", "x86_64", "foo-1.5-3.src.rpm"),
            pkg("foo", 0, "1.2", "2", "x86_64", "foo-1.2-2.src.rpm"),
        ]);
        let src = spec_src("foo", "1.3", "1", "");
        let diags = run_both(&src, &redos_profile(), uni);
        let d030: Vec<_> = diags.iter().filter(|d| d.lint_id == "RPM-REPO-030").collect();
        assert_eq!(d030.len(), 1, "{diags:?}");
        assert!(d030[0].message.contains("foo-1.5-3"), "{}", d030[0].message);
    }
}
