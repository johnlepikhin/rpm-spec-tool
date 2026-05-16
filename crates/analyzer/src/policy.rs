//! Per-family packaging policy maps.
//!
//! Phase 20 introduces a small set of "what does this distro want?"
//! tables that the new scriptlet/systemd/tmpfiles/users rules
//! (RPM303, RPM343–RPM348) read. The maps are keyed on
//! [`Family`] and exposed as immutable `&'static` slices via
//! [`PolicyRegistry::for_family`]; rules call into the registry once
//! in `set_profile` and stash whatever they need.
//!
//! Why not in the `profile` crate? The policy data is consumed by
//! analyzer rules and would otherwise force every profile TOML to
//! carry duplicate entries. Keeping it analyzer-local — derived from
//! `Profile::identity.family` — avoids that churn and is the same
//! shape `hardcoded_paths` uses for its fallback path table.

use rpm_spec::ast::{Text, TextSegment};
use rpm_spec_profile::{Family, Profile};

/// Bundle of policy knobs consulted by Phase 20+ rules.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PolicyRegistry {
    /// Macros the family supplies for systemd unit lifecycle. The
    /// slice covers `%post` / `%preun` / `%postun` semantics —
    /// listed in source order so a `for` loop yields a stable iteration.
    pub systemd_macros: &'static [&'static str],
    /// Macros the family supplies for `tmpfiles.d` creation.
    pub tmpfiles_create_macros: &'static [&'static str],
    /// `%{?dist}` policy. Fedora-derived families require it on
    /// `Release:`; non-Fedora distros don't.
    pub disttag: DistTagPolicy,
    /// Dist-tag substrings that flag a hardcoded tag (`.fc40`,
    /// `.el9`, …) — caller compares verbatim, no globbing.
    pub hardcoded_dist_substrings: &'static [&'static str],
}

/// Whether the active family enforces `%{?dist}` in `Release:`.
///
/// Only two states are needed today: families that *require* the
/// macro (Fedora/RHEL) and families that ignore it (everyone else).
/// Mageia is treated as "not required" but ships hardcoded-suffix
/// detection via [`PolicyRegistry::hardcoded_dist_substrings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DistTagPolicy {
    /// `Release:` must end in `%{?dist}` (Fedora/RHEL convention).
    Required,
    /// `%{?dist}` is not part of the distro's release-naming
    /// convention; flagging its absence would be noise.
    NotApplicable,
}

impl PolicyRegistry {
    /// Lookup table per family. Generic / unknown families get a
    /// conservative all-silent fallback so rules stay quiet rather
    /// than fire on a distro we don't know.
    pub fn for_family(family: Option<Family>) -> Self {
        match family {
            Some(Family::Fedora) | Some(Family::Rhel) => Self {
                systemd_macros: FEDORA_SYSTEMD_MACROS,
                tmpfiles_create_macros: FEDORA_TMPFILES_MACROS,
                disttag: DistTagPolicy::Required,
                hardcoded_dist_substrings: FEDORA_HARDCODED_DIST,
            },
            Some(Family::Opensuse) => Self {
                systemd_macros: OPENSUSE_SYSTEMD_MACROS,
                tmpfiles_create_macros: OPENSUSE_TMPFILES_MACROS,
                disttag: DistTagPolicy::NotApplicable,
                hardcoded_dist_substrings: &[],
            },
            Some(Family::Mageia) => Self {
                systemd_macros: MAGEIA_SYSTEMD_MACROS,
                tmpfiles_create_macros: FEDORA_TMPFILES_MACROS,
                disttag: DistTagPolicy::NotApplicable,
                hardcoded_dist_substrings: &[".mga"],
            },
            Some(Family::Alt) => Self {
                systemd_macros: ALT_SYSTEMD_MACROS,
                tmpfiles_create_macros: ALT_TMPFILES_MACROS,
                disttag: DistTagPolicy::NotApplicable,
                hardcoded_dist_substrings: &[],
            },
            Some(Family::Generic) | None => Self::generic(),
            // `Family` is `#[non_exhaustive]`; any future variant
            // falls back to the all-silent generic table until a
            // policy entry is added for it.
            Some(_) => Self::generic(),
        }
    }

    /// Convenience over [`Self::for_family`] for call sites that
    /// already hold a `&Profile`. Centralises the projection so a
    /// future expansion (e.g. version- or macro-driven policy) is a
    /// one-place change.
    pub fn for_profile(profile: &Profile) -> Self {
        Self::for_family(profile.identity.family)
    }

    /// Silent-baseline table used for Generic/unknown families. Kept
    /// public so [`Default`] callers and rules that pre-initialise
    /// before `set_profile` can reach it.
    pub fn generic() -> Self {
        Self {
            systemd_macros: &[],
            tmpfiles_create_macros: &[],
            disttag: DistTagPolicy::NotApplicable,
            hardcoded_dist_substrings: &[],
        }
    }
}

impl Default for PolicyRegistry {
    /// The Generic / silent-baseline table. Lets rules use
    /// `#[derive(Default)]` and skip a hand-rolled `Default` impl.
    fn default() -> Self {
        Self::generic()
    }
}

/// `true` when any segment of `line` is a macro reference whose name
/// matches one of `macros`. Shared by Phase 20 rules that gate on
/// "did a scriptlet call a known helper?".
pub(crate) fn line_references_any_macro(line: &Text, macros: &[&str]) -> bool {
    line.segments.iter().any(|seg| match seg {
        TextSegment::Macro(m) => macros.contains(&m.name.as_str()),
        _ => false,
    })
}

// ---------------------------------------------------------------------
// Fedora / RHEL family
// ---------------------------------------------------------------------

/// Fedora's `systemd-rpm-macros` package supplies these. The set is
/// stable across Fedora 30+ and RHEL 8+.
const FEDORA_SYSTEMD_MACROS: &[&str] = &[
    "systemd_post",
    "systemd_preun",
    "systemd_postun",
    "systemd_postun_with_restart",
    "systemd_user_post",
    "systemd_user_preun",
    "systemd_user_postun_with_restart",
    "systemd_requires",
    "systemd_ordering",
];

const FEDORA_TMPFILES_MACROS: &[&str] = &["tmpfiles_create", "tmpfiles_create_package"];

const FEDORA_HARDCODED_DIST: &[&str] = &[".fc", ".el"];

// ---------------------------------------------------------------------
// openSUSE family
// ---------------------------------------------------------------------

const OPENSUSE_SYSTEMD_MACROS: &[&str] = &[
    "service_add_pre",
    "service_add_post",
    "service_del_preun",
    "service_del_postun",
    "service_del_postun_with_restart",
];

const OPENSUSE_TMPFILES_MACROS: &[&str] = &["tmpfiles_create"];

// ---------------------------------------------------------------------
// Mageia family — uses Fedora-derived systemd macros plus its own
// `urpmi`-style ones for cache updates we don't track here.
// ---------------------------------------------------------------------

const MAGEIA_SYSTEMD_MACROS: &[&str] = FEDORA_SYSTEMD_MACROS;

// ---------------------------------------------------------------------
// ALT Linux family — `rpm-macros-systemd` ships its own set.
// ---------------------------------------------------------------------

const ALT_SYSTEMD_MACROS: &[&str] = &[
    "post_service",
    "preun_service",
    "postun_service",
    "post_systemd_unit",
    "preun_systemd_unit",
];

const ALT_TMPFILES_MACROS: &[&str] = &["systemd_tmpfiles_create", "tmpfiles_create"];

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec::ast::{ConditionalMacro, MacroKind, MacroRef, Text, TextSegment};

    fn macro_seg(name: &str) -> TextSegment {
        TextSegment::macro_ref(MacroRef {
            kind: MacroKind::Braced,
            name: name.into(),
            args: Vec::new(),
            conditional: ConditionalMacro::None,
            with_value: None,
        })
    }

    #[test]
    fn fedora_disttag_is_required() {
        let p = PolicyRegistry::for_family(Some(Family::Fedora));
        assert_eq!(p.disttag, DistTagPolicy::Required);
        assert!(p.systemd_macros.contains(&"systemd_post"));
        assert!(!p.systemd_macros.contains(&"service_add_post"));
    }

    #[test]
    fn opensuse_uses_service_macros() {
        let p = PolicyRegistry::for_family(Some(Family::Opensuse));
        assert_eq!(p.disttag, DistTagPolicy::NotApplicable);
        assert!(p.systemd_macros.contains(&"service_add_post"));
        assert!(!p.systemd_macros.contains(&"systemd_post"));
    }

    #[test]
    fn generic_is_silent_baseline() {
        let p = PolicyRegistry::for_family(None);
        assert_eq!(p.disttag, DistTagPolicy::NotApplicable);
        assert!(p.systemd_macros.is_empty());
        assert!(p.tmpfiles_create_macros.is_empty());
    }

    #[test]
    fn alt_uses_its_own_systemd_macros() {
        let p = PolicyRegistry::for_family(Some(Family::Alt));
        assert!(p.systemd_macros.contains(&"post_service"));
        assert!(
            p.tmpfiles_create_macros
                .contains(&"systemd_tmpfiles_create")
        );
    }

    #[test]
    fn mageia_has_dist_substrings_but_not_required() {
        let p = PolicyRegistry::for_family(Some(Family::Mageia));
        assert_eq!(p.disttag, DistTagPolicy::NotApplicable);
        assert_eq!(p.hardcoded_dist_substrings, &[".mga"]);
    }

    #[test]
    fn default_is_generic() {
        let p = PolicyRegistry::default();
        assert!(p.systemd_macros.is_empty());
        assert_eq!(p.disttag, DistTagPolicy::NotApplicable);
    }

    #[test]
    fn line_references_any_macro_finds_known_macro() {
        let line = Text {
            segments: vec![
                TextSegment::Literal("    ".into()),
                macro_seg("systemd_post"),
                TextSegment::Literal(" foo.service".into()),
            ],
        };
        assert!(line_references_any_macro(
            &line,
            &["systemd_post", "service_add_post"],
        ));
    }

    #[test]
    fn line_references_any_macro_misses_unknown() {
        let line = Text {
            segments: vec![macro_seg("some_other_macro")],
        };
        assert!(!line_references_any_macro(&line, &["systemd_post"]));
    }

    #[test]
    fn line_references_any_macro_ignores_literals() {
        let line = Text::from("systemd_post foo");
        assert!(!line_references_any_macro(&line, &["systemd_post"]));
    }
}
