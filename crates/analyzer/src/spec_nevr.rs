//! Spec-side NEVR extraction shared by RPM-REPO-030/031 lints and the
//! `matrix upgrade-sim` CLI subcommand.
//!
//! Both consumers must agree on:
//! * how the main package's NEVR is recovered from the preamble (with
//!   macro expansion via the profile registry overlaid by spec-local
//!   `%global` / `%define`);
//! * which architectures count as "match" when comparing against repo
//!   binaries (build_arch + noarch);
//! * how the spec-side EVR maps to a [`EVR`] for rpm vercmp ordering.
//!
//! Splitting this module out of `rules/repo/upgrade_check.rs` keeps
//! the dependency arrow correct: the CLI imports stable, named entry
//! points (`SpecMainNevr`, `enriched_macros_with_spec_locals`,
//! `ArchFilter`) from `analyzer::spec_nevr` instead of reaching into
//! the internal lint-rules layout. Moving the lint file is then a
//! crate-internal refactor instead of a breaking change.

use rpm_spec::ast::{ConditionalMacro, PreambleItem, Span, SpecFile, Tag, TagValue, TextSegment};
use rpm_spec_profile::{MacroEntry, MacroRegistry, Profile, Provenance};
use rpm_spec_repo_core::EVR;

use crate::bcond::{BcondMap, BcondOverrides};
use crate::rules::util::preamble::collect_top_level_preamble;
use crate::spec_locals::scan_spec_locals;

/// Macro expansion depth limit for spec-NEVR and dependency atom
/// resolution. Matches the convention used by `branch_coverage`,
/// `files::classifier`, and the RPM-REPO-* family — chains like
/// `%{python3_pkgversion} → %{__python3_pkgversion} → "3.11"` resolve
/// well within 8 hops. This is the canonical site; RPM-REPO siblings
/// in `rules::repo::shared` re-export it so a single bump moves the
/// whole analyzer together.
pub const MACRO_EXPAND_DEPTH: u8 = 8;

/// Main-package NEVR projected from the top-level preamble.
///
/// `epoch` is `Option<u32>`: `None` when the spec omits `Epoch:`,
/// `Some(0)` when the spec writes `Epoch: 0` explicitly. rpm treats
/// these equivalently for *ordering*, but RPM-REPO-031 cares whether
/// the user wrote it (the diagnostic phrases "omits `Epoch:`"
/// differently from "has `Epoch: 0`"). [`Self::epoch_for_ordering`]
/// collapses both forms to `0` for callers that just want EVR cmp.
#[derive(Debug)]
#[non_exhaustive]
pub struct SpecMainNevr {
    /// Resolved `Name:` value.
    pub name: String,
    /// Resolved `Epoch:` value, or `None` when absent in the spec.
    pub epoch: Option<u32>,
    /// Resolved `Version:` value.
    pub version: String,
    /// Resolved `Release:` value.
    pub release: String,
    /// Anchor for diagnostics — points at the spec's `Version:` line so
    /// 030 and 031 underline the same place the user authored.
    pub evr_span: Span,
}

impl SpecMainNevr {
    /// Extract the main NEVR from `spec`'s top-level preamble, expanding
    /// any `%{macro}` references via `macros`. Real-world specs almost
    /// always parameterise `Version:` / `Release:` through macros
    /// (`Version: %{krb5_version}`), so the literal-only path is useless
    /// on every shipping spec; the [`MacroRegistry`] arg is mandatory.
    ///
    /// # Returns
    ///
    /// `None` when `Name:` / `Version:` / `Release:` are missing OR
    /// cannot be resolved to literal strings (e.g. macro chain failed
    /// to terminate at a literal).
    #[must_use]
    pub fn extract(spec: &SpecFile<Span>, macros: &MacroRegistry) -> Option<Self> {
        let mut name: Option<String> = None;
        let mut version: Option<(String, Span)> = None;
        let mut release: Option<String> = None;
        let mut epoch: Option<u32> = None;
        for item in collect_top_level_preamble(spec) {
            match item.tag {
                #[allow(clippy::collapsible_match)]
                Tag::Name => {
                    if let Some(t) = item_text(item, macros) {
                        name = Some(t);
                    }
                }
                #[allow(clippy::collapsible_match)]
                Tag::Version => {
                    if let Some(t) = item_text(item, macros) {
                        version = Some((t, item.data));
                    }
                }
                #[allow(clippy::collapsible_match)]
                Tag::Release => {
                    if let Some(t) = item_text(item, macros) {
                        release = Some(t);
                    }
                }
                Tag::Epoch => {
                    epoch = match &item.value {
                        TagValue::Number(n) => Some(*n),
                        TagValue::Text(_) => {
                            item_text(item, macros).and_then(|t| t.trim().parse::<u32>().ok())
                        }
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

    /// Build an [`EVR`] from this NEVR for rpm vercmp ordering.
    #[must_use]
    pub fn to_evr(&self) -> EVR {
        EVR::new(
            Some(self.epoch_for_ordering()),
            &self.version,
            &self.release,
        )
    }
}

/// Clone the profile's macro registry and overlay every `%global` /
/// `%define` declared in the spec's active branches. Without this,
/// every real-world spec fails [`SpecMainNevr::extract`] because
/// `Name: %{prog_name}-%{edition}` (typical postgres-style spec) refers
/// to macros declared via `%global` at the top of the spec — which the
/// profile registry doesn't know about.
///
/// Profile / `-D`-override values win over spec defaults (same
/// precedence policy [`scan_spec_locals`] uses internally), so an
/// operator overriding `pgsql_major` from the CLI keeps control.
///
/// Cost note: clones the entire `profile.macros` map (typically a few
/// hundred to a few thousand entries). At ~1 µs per profile clone this
/// is comfortably below the per-spec analyzer overhead, but callers
/// that run the same spec × profile pair more than once should hoist
/// the call out of the inner loop.
#[must_use]
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

/// Architectures considered a "match" for repo lookups, given a
/// [`Profile`].
///
/// Two states:
/// * [`ArchFilter::Any`] — profile carries no `build_arch`; the lint
///   can't second-guess what the operator intends, so any arch counts
///   (typical for `generic` profile or pre-1.0 profiles where the arch
///   block wasn't wired up). The previous `Vec<String>`-with-sentinel
///   shape encoded this same idea as `["noarch"]` + "len == 1 means
///   match-all" — a subtle invariant that survived only by convention.
/// * [`ArchFilter::Profile`] — `build_arch` is known; only that arch
///   plus `noarch` (yum-compat) qualifies.
///
/// The enum makes the two-state nature unmissable at the type level so
/// `matches` callers can't accidentally interpret a single-entry vec
/// as "only noarch" (the original sentinel intent).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ArchFilter {
    /// Profile didn't declare its build_arch — every arch counts.
    Any,
    /// Profile's build_arch is known — match only that or `noarch`.
    Profile {
        /// The profile's `build_arch` field.
        build_arch: String,
    },
}

impl ArchFilter {
    /// Build the filter from a [`Profile`]. `Profile::arch::build_arch`
    /// is the source of truth; absent → [`Self::Any`].
    #[must_use]
    pub fn from_profile(profile: &Profile) -> Self {
        match profile.arch.build_arch.as_deref() {
            Some(b) => Self::Profile {
                build_arch: b.to_string(),
            },
            None => Self::Any,
        }
    }

    /// `true` iff `arch` matches the filter.
    #[must_use]
    pub fn matches(&self, arch: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Profile { build_arch } => arch == build_arch || arch == "noarch",
        }
    }
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
/// Returns `None` if any unconditional macro fails to resolve — same
/// conservative-skip policy as the rest of the RPM-REPO-* family.
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
                        && let Some(value) = macros.expand_to_literal(&m.name, MACRO_EXPAND_DEPTH)
                    {
                        out.push_str(&value);
                    }
                    // Undefined → empty (matches rpm `%{?dist}` on a
                    // host where `dist` was never `%define`d).
                }
                ConditionalMacro::IfNotDefined => {
                    // `%{!?foo}` standalone — empty on defined, empty
                    // on undefined (no `:default` body to emit). The
                    // `%{!?foo:body}` form goes through a different
                    // parse and would need explicit handling.
                }
                // ConditionalMacro is non-exhaustive in upstream
                // rpm-spec; any new variant defaults to skip so we
                // don't silently inject wrong output.
                _ => {
                    tracing::debug!(
                        macro_name = %m.name,
                        conditional = ?m.conditional,
                        "resolve_text_segments: unknown ConditionalMacro variant, skipping",
                    );
                    return None;
                }
            },
            // Any other segment kind (e.g. interpolated expressions
            // future-added to TextSegment) — conservative skip.
            _ => {
                tracing::debug!(
                    segment = ?seg,
                    "resolve_text_segments: unknown TextSegment variant, skipping",
                );
                return None;
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_profile::{MacroEntry, Provenance};

    fn profile_with_arch(arch: &str) -> Profile {
        let mut p = Profile::default();
        p.arch.build_arch = Some(arch.to_string());
        p
    }

    fn registry_with(pairs: &[(&str, &str)]) -> MacroRegistry {
        let mut r = MacroRegistry::default();
        for (n, v) in pairs {
            r.insert(
                (*n).to_string(),
                MacroEntry::literal(*v, Provenance::Override),
            );
        }
        r
    }

    #[test]
    fn arch_filter_any_matches_everything() {
        let mut p = Profile::default();
        p.arch.build_arch = None;
        let f = ArchFilter::from_profile(&p);
        assert!(matches!(f, ArchFilter::Any));
        assert!(f.matches("x86_64"));
        assert!(f.matches("i686"));
        assert!(f.matches("noarch"));
        assert!(f.matches("riscv64"));
    }

    #[test]
    fn arch_filter_profile_includes_noarch() {
        let p = profile_with_arch("x86_64");
        let f = ArchFilter::from_profile(&p);
        assert!(f.matches("x86_64"));
        assert!(f.matches("noarch"));
        assert!(!f.matches("i686"));
        assert!(!f.matches("aarch64"));
    }

    #[test]
    fn extract_returns_none_when_name_missing() {
        let src = "Version: 1.0\nRelease: 1\nSummary: s\nLicense: MIT\n%description\nx\n";
        let outcome = crate::session::parse(src);
        let r = registry_with(&[]);
        assert!(SpecMainNevr::extract(&outcome.spec, &r).is_none());
    }

    #[test]
    fn extract_resolves_pure_macro_in_version() {
        // `Version: %{my_ver}` — single Macro(None) segment.
        let src = "Name: foo\nVersion: %{my_ver}\nRelease: 1\n\
                   Summary: s\nLicense: MIT\n%description\nx\n";
        let outcome = crate::session::parse(src);
        let r = registry_with(&[("my_ver", "2.5")]);
        let n = SpecMainNevr::extract(&outcome.spec, &r).expect("must extract");
        assert_eq!(n.name, "foo");
        assert_eq!(n.version, "2.5");
        assert_eq!(n.release, "1");
    }

    #[test]
    fn extract_resolves_mixed_literal_plus_conditional() {
        // `Release: 1%{?dist}` — Literal("1") + Macro(IfDefined "dist").
        // Hits the segment walker's IfDefined branch + concat path.
        let src = "Name: foo\nVersion: 1.0\nRelease: 1%{?dist}\n\
                   Summary: s\nLicense: MIT\n%description\nx\n";
        let outcome = crate::session::parse(src);
        // `dist` defined → suffix appended.
        let r_defined = registry_with(&[("dist", ".el9")]);
        let n = SpecMainNevr::extract(&outcome.spec, &r_defined).expect("must extract");
        assert_eq!(n.release, "1.el9");
        // `dist` undefined → suffix elides.
        let r_undef = registry_with(&[]);
        let n = SpecMainNevr::extract(&outcome.spec, &r_undef).expect("must extract");
        assert_eq!(n.release, "1");
    }

    #[test]
    fn extract_returns_none_on_unresolvable_macro() {
        // Unconditional `%{undefined}` in Version → conservative skip.
        let src = "Name: foo\nVersion: %{undefined_macro}\nRelease: 1\n\
                   Summary: s\nLicense: MIT\n%description\nx\n";
        let outcome = crate::session::parse(src);
        let r = registry_with(&[]);
        assert!(SpecMainNevr::extract(&outcome.spec, &r).is_none());
    }

    #[test]
    fn extract_epoch_from_macro_text_value() {
        // `Epoch: %{my_epoch}` — TagValue::Text path through item_text.
        let src = "Name: foo\nEpoch: %{my_epoch}\nVersion: 1.0\nRelease: 1\n\
                   Summary: s\nLicense: MIT\n%description\nx\n";
        let outcome = crate::session::parse(src);
        let r = registry_with(&[("my_epoch", "3")]);
        let n = SpecMainNevr::extract(&outcome.spec, &r).expect("must extract");
        assert_eq!(n.epoch, Some(3));
        assert_eq!(n.epoch_for_ordering(), 3);
    }

    #[test]
    fn extract_epoch_unparseable_macro_falls_back_to_none() {
        // Macro resolves to non-numeric → epoch reported as absent
        // (matches rpm's tolerance for syntactically broken epoch).
        let src = "Name: foo\nEpoch: %{junk}\nVersion: 1.0\nRelease: 1\n\
                   Summary: s\nLicense: MIT\n%description\nx\n";
        let outcome = crate::session::parse(src);
        let r = registry_with(&[("junk", "not-a-number")]);
        let n = SpecMainNevr::extract(&outcome.spec, &r).expect("must extract");
        assert_eq!(n.epoch, None);
    }
}
