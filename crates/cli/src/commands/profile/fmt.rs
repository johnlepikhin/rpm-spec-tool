//! Pure rendering primitives shared across the `profile` subcommands.
//!
//! Self-contained: no `Config`, no I/O — just `MacroValue` /
//! `Provenance` / `Family` formatting helpers and the width constants
//! used by the tabular renderers.

use rpm_spec_analyzer::profile::{MacroValue, Profile, Provenance};

/// Cap on the `name(opts)` alignment width used by macro listing tables.
pub(super) const MAX_MACRO_LABEL_WIDTH: usize = 48;

/// Cap on the profile-name alignment width used by per-profile
/// comparison tables.
pub(super) const MAX_PROFILE_NAME_WIDTH: usize = 40;

/// Truncation length for one-line value renders in comparison tables.
pub(super) const COMPACT_VALUE_MAX_LEN: usize = 80;

/// Compact, single-line rendering of a macro value. For multiline `Raw`
/// values, summarises as `<multiline N chars>` so each entry stays on
/// one line. `Builtin` becomes the literal token `<builtin>`.
pub(super) fn format_macro_value_inline(value: &MacroValue) -> String {
    match value {
        MacroValue::Literal(s) => s.clone(),
        MacroValue::Builtin => "<builtin>".to_string(),
        MacroValue::Raw { body, multiline } => {
            if *multiline {
                format!("<multiline {} chars>", body.len())
            } else {
                body.clone()
            }
        }
    }
}

/// `name(opts)` suffix when `opts` is present, empty otherwise. Inlines
/// into format strings as `{name}{opts}`.
pub(super) fn format_opts(opts: Option<&str>) -> String {
    opts.map(|o| format!("({o})")).unwrap_or_default()
}

/// Provenance tag used inside `[...]` in macro listings. Matches the
/// human-readable form used by `profile show --full`.
pub(super) fn format_provenance(prov: &Provenance) -> String {
    match prov {
        Provenance::Builtin { profile } => format!("builtin:{profile}"),
        Provenance::Showrc { level, path: _ } => format!("showrc:{level}"),
        Provenance::Override => "override".to_string(),
    }
}

/// Compact one-line value rendering for comparison tables — truncates
/// long single-line bodies and collapses multiline bodies to a
/// `<multiline …>` marker so each row stays on one line.
pub(super) fn compact_value(value: &MacroValue) -> String {
    let rendered = format_macro_value_inline(value);
    if rendered.len() <= COMPACT_VALUE_MAX_LEN {
        rendered
    } else {
        let mut truncated: String = rendered.chars().take(COMPACT_VALUE_MAX_LEN - 1).collect();
        truncated.push('…');
        truncated
    }
}

/// Human-friendly family label — `Debug` rendering of `Family` capitalises
/// the variant name (e.g. `Rhel`), `-` when no family is set.
pub(crate) fn family_label(p: &Profile) -> String {
    p.identity
        .family
        .map(|f| format!("{f:?}"))
        .unwrap_or_else(|| "-".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_analyzer::profile::Provenance;

    #[test]
    fn format_macro_value_literal() {
        assert_eq!(
            format_macro_value_inline(&MacroValue::Literal(".el9".into())),
            ".el9"
        );
    }

    #[test]
    fn format_macro_value_builtin() {
        assert_eq!(format_macro_value_inline(&MacroValue::Builtin), "<builtin>");
    }

    #[test]
    fn format_macro_value_raw_single_and_multiline() {
        let one = MacroValue::Raw {
            body: "%{?_with_foo:1}".into(),
            multiline: false,
        };
        assert_eq!(format_macro_value_inline(&one), "%{?_with_foo:1}");
        let many = MacroValue::Raw {
            body: "line1\nline2\nline3".into(),
            multiline: true,
        };
        let rendered = format_macro_value_inline(&many);
        assert!(rendered.starts_with("<multiline "));
        assert!(rendered.ends_with(" chars>"));
    }

    #[test]
    fn format_provenance_all_variants() {
        assert_eq!(
            format_provenance(&Provenance::Builtin {
                profile: "generic".into()
            }),
            "builtin:generic"
        );
        assert_eq!(
            format_provenance(&Provenance::Showrc {
                level: -13,
                path: None,
            }),
            "showrc:-13"
        );
        assert_eq!(format_provenance(&Provenance::Override), "override");
    }

    #[test]
    fn compact_value_truncates_long_values() {
        let long = "a".repeat(200);
        let v = MacroValue::Raw {
            body: long,
            multiline: false,
        };
        let s = compact_value(&v);
        assert!(s.chars().count() == COMPACT_VALUE_MAX_LEN);
        assert!(s.ends_with('…'));
    }
}
