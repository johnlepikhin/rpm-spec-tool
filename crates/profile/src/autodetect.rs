//! Derive [`Identity`] fields from a parsed `rpm --showrc` macro list.
//!
//! Used by the resolver between the showrc layer and the user-override
//! layer: anything the user did not explicitly set in
//! `[profiles.X.identity]` falls back to whatever showrc tells us.
//!
//! Auto-detect rules (all "best effort" — missing data leaves the field
//! at `None` / [`Family::Generic`]):
//!
//! * `vendor`   ← `%_vendor` (literal-valued macro only)
//! * `dist_tag` ← `%dist` (literal-valued)
//! * `family`   ← first marker macro found, by fixed priority:
//!   `altlinux` > `mageia` > `suse_version` > `rhel` > `fedora`.
//!   Derivative distributions (AlmaLinux, Rocky, CentOS Stream, …)
//!   report multiple markers; the priority order keeps them under their
//!   parent family ([`Family::Rhel`] for the el-family, etc.).

use crate::merge::IdentityPatch;
use crate::types::{Family, MacroEntry, MacroValue};

/// Iterate the macro entries in showrc-parse order and produce an
/// [`IdentityPatch`] with whatever could be inferred. Fields the macros
/// don't provide stay `None` / unset.
pub fn detect<'a, I>(entries: I) -> IdentityPatch
where
    I: IntoIterator<Item = (&'a str, &'a MacroEntry)>,
{
    // We need random access by name for the priority lookup, so collect
    // into a small temporary map. Cost is negligible (~700 entries).
    use std::collections::HashMap;
    let by_name: HashMap<&str, &MacroEntry> = entries.into_iter().collect();

    let mut out = IdentityPatch::default();

    if let Some(v) = by_name.get("_vendor").and_then(|e| literal(e)) {
        out.vendor = Some(v.to_string());
    }
    if let Some(v) = by_name.get("dist").and_then(|e| literal(e)) {
        out.dist_tag = Some(v.to_string());
    }

    // Priority-ordered marker lookup. First hit wins.
    const MARKERS: &[(&str, Family)] = &[
        ("altlinux", Family::Alt),
        ("mageia", Family::Mageia),
        ("suse_version", Family::Opensuse),
        ("rhel", Family::Rhel),
        ("fedora", Family::Fedora),
    ];
    for (marker, family) in MARKERS {
        if let Some(entry) = by_name.get(*marker) {
            if literal(entry).is_some() {
                out.family = Some(*family);
                break;
            }
        }
    }

    out
}

fn literal(entry: &MacroEntry) -> Option<&str> {
    match &entry.value {
        MacroValue::Literal(s) => Some(s.as_str()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MacroEntry, Provenance};

    fn lit(s: &str) -> MacroEntry {
        MacroEntry::literal(
            s,
            Provenance::Showrc {
                level: -13,
                path: None,
            },
        )
    }

    #[test]
    fn detects_vendor_and_dist_tag() {
        let v = lit("redhat");
        let d = lit(".el9");
        let entries = vec![("_vendor", &v), ("dist", &d)];
        let id = detect(entries);
        assert_eq!(id.vendor.as_deref(), Some("redhat"));
        assert_eq!(id.dist_tag.as_deref(), Some(".el9"));
        assert!(id.family.is_none()); // no marker → Generic by virtue of None
    }

    #[test]
    fn rhel_family_from_marker() {
        let e = lit("9");
        let id = detect(vec![("rhel", &e)]);
        assert_eq!(id.family, Some(Family::Rhel));
    }

    #[test]
    fn fedora_family_when_only_fedora_marker() {
        let e = lit("40");
        let id = detect(vec![("fedora", &e)]);
        assert_eq!(id.family, Some(Family::Fedora));
    }

    #[test]
    fn rhel_wins_over_fedora_on_derivatives() {
        // CentOS / RHEL clones often expose both rhel and fedora markers
        // (the latter via inherited macros). Priority must keep them in
        // the RHEL family.
        let r = lit("9");
        let f = lit("38");
        let id = detect(vec![("fedora", &f), ("rhel", &r)]);
        assert_eq!(id.family, Some(Family::Rhel));
    }

    #[test]
    fn altlinux_wins_over_legacy_markers() {
        // ALT-family lineage sometimes still ships a vestigial `mandriva`
        // macro from its Mandriva ancestry. `altlinux` is set explicitly,
        // so it must win.
        let alt = lit("p10");
        let mageia = lit("9");
        let id = detect(vec![("mageia", &mageia), ("altlinux", &alt)]);
        assert_eq!(id.family, Some(Family::Alt));
    }

    #[test]
    fn non_literal_marker_is_ignored() {
        // A marker whose value is computed (`%{lua: ...}`) shouldn't
        // count as a positive identification — we'd be guessing.
        let e = MacroEntry {
            value: MacroValue::Raw {
                body: "%(echo 9)".into(),
                multiline: false,
            },
            opts: None,
            provenance: Provenance::Showrc {
                level: -13,
                path: None,
            },
        };
        let id = detect(vec![("rhel", &e)]);
        assert!(id.family.is_none());
    }
}
