//! Built-in profile catalogue.
//!
//! Each built-in is shipped as a TOML file under `data/` and embedded
//! into the binary via [`include_str!`]. Distribution profiles can also
//! bundle a verbatim `rpm --showrc` dump (`data/<name>.showrc`) which
//! the loader parses on first use and exposes alongside the TOML
//! metadata.
//!
//! The resolver applies both patches in sequence (meta then showrc),
//! then runs identity auto-detection against the bundled macros —
//! mirroring the flow used for user-supplied `showrc-file` entries.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, OnceLock};

use serde::Deserialize;

use crate::merge::{IdentityPatch, ListPatch, ProfilePatch};
use crate::showrc;
use crate::types::{Family, LayerInfo, ValidationMode};

/// Sentinel-path prefix used in `Provenance::Showrc.path` for macros
/// parsed from a bundled-in-binary showrc dump. Never resolved against
/// the filesystem — the `<` makes it visually distinct from real paths.
const BUILTIN_SENTINEL_PREFIX: &str = "<builtin:";

/// Build the sentinel path for a bundled-builtin showrc dump.
fn builtin_sentinel_path(name: &str) -> PathBuf {
    PathBuf::from(format!("{BUILTIN_SENTINEL_PREFIX}{name}>"))
}

/// Name of the always-available baseline built-in. Used as the fallback
/// `extends` value when a profile entry doesn't specify one, and as the
/// default active profile when the config doesn't pick one.
pub const DEFAULT_BUILTIN: &str = "generic";

/// One built-in entry as compiled into the binary.
struct BuiltinDef {
    name: &'static str,
    toml_src: &'static str,
    /// Verbatim `rpm --showrc` output for distribution profiles. `None`
    /// for synthetic profiles like `generic`.
    showrc_src: Option<&'static str>,
}

/// Compile-time registry. Add new entries here when shipping additional
/// distribution profiles.
const REGISTRY: &[BuiltinDef] = &[
    BuiltinDef {
        name: DEFAULT_BUILTIN,
        toml_src: include_str!("../data/generic.toml"),
        showrc_src: None,
    },
    BuiltinDef {
        name: "rhel-8-x86_64",
        toml_src: include_str!("../data/rhel-8-x86_64.toml"),
        showrc_src: Some(include_str!("../data/rhel-8-x86_64.showrc")),
    },
    BuiltinDef {
        name: "rhel-8-aarch64",
        toml_src: include_str!("../data/rhel-8-aarch64.toml"),
        showrc_src: Some(include_str!("../data/rhel-8-aarch64.showrc")),
    },
    BuiltinDef {
        name: "rhel-9-x86_64",
        toml_src: include_str!("../data/rhel-9-x86_64.toml"),
        showrc_src: Some(include_str!("../data/rhel-9-x86_64.showrc")),
    },
    BuiltinDef {
        name: "rhel-9-aarch64",
        toml_src: include_str!("../data/rhel-9-aarch64.toml"),
        showrc_src: Some(include_str!("../data/rhel-9-aarch64.showrc")),
    },
    BuiltinDef {
        name: "rhel-10-x86_64",
        toml_src: include_str!("../data/rhel-10-x86_64.toml"),
        showrc_src: Some(include_str!("../data/rhel-10-x86_64.showrc")),
    },
    BuiltinDef {
        name: "rhel-10-aarch64",
        toml_src: include_str!("../data/rhel-10-aarch64.toml"),
        showrc_src: Some(include_str!("../data/rhel-10-aarch64.showrc")),
    },
    BuiltinDef {
        name: "redos-7.3-x86_64",
        toml_src: include_str!("../data/redos-7.3-x86_64.toml"),
        showrc_src: Some(include_str!("../data/redos-7.3-x86_64.showrc")),
    },
    BuiltinDef {
        name: "redos-7.3-aarch64",
        toml_src: include_str!("../data/redos-7.3-aarch64.toml"),
        showrc_src: Some(include_str!("../data/redos-7.3-aarch64.showrc")),
    },
    BuiltinDef {
        name: "redos-8-x86_64",
        toml_src: include_str!("../data/redos-8-x86_64.toml"),
        showrc_src: Some(include_str!("../data/redos-8-x86_64.showrc")),
    },
    BuiltinDef {
        name: "redos-8-aarch64",
        toml_src: include_str!("../data/redos-8-aarch64.toml"),
        showrc_src: Some(include_str!("../data/redos-8-aarch64.showrc")),
    },
    BuiltinDef {
        name: "altlinux-10-x86_64",
        toml_src: include_str!("../data/altlinux-10-x86_64.toml"),
        showrc_src: Some(include_str!("../data/altlinux-10-x86_64.showrc")),
    },
    BuiltinDef {
        name: "altlinux-10-aarch64",
        toml_src: include_str!("../data/altlinux-10-aarch64.toml"),
        showrc_src: Some(include_str!("../data/altlinux-10-aarch64.showrc")),
    },
    BuiltinDef {
        name: "altlinux-10-e2k",
        toml_src: include_str!("../data/altlinux-10-e2k.toml"),
        showrc_src: Some(include_str!("../data/altlinux-10-e2k.showrc")),
    },
    BuiltinDef {
        name: "altlinux-10-e2kv4",
        toml_src: include_str!("../data/altlinux-10-e2kv4.toml"),
        showrc_src: Some(include_str!("../data/altlinux-10-e2kv4.showrc")),
    },
    BuiltinDef {
        name: "altlinux-11-x86_64",
        toml_src: include_str!("../data/altlinux-11-x86_64.toml"),
        showrc_src: Some(include_str!("../data/altlinux-11-x86_64.showrc")),
    },
    BuiltinDef {
        name: "altlinux-11-aarch64",
        toml_src: include_str!("../data/altlinux-11-aarch64.toml"),
        showrc_src: Some(include_str!("../data/altlinux-11-aarch64.showrc")),
    },
    BuiltinDef {
        name: "altlinux-spt-10-x86_64",
        toml_src: include_str!("../data/altlinux-spt-10-x86_64.toml"),
        showrc_src: Some(include_str!("../data/altlinux-spt-10-x86_64.showrc")),
    },
    BuiltinDef {
        name: "altlinux-spt-10-aarch64",
        toml_src: include_str!("../data/altlinux-spt-10-aarch64.toml"),
        showrc_src: Some(include_str!("../data/altlinux-spt-10-aarch64.showrc")),
    },
    BuiltinDef {
        name: "altlinux-spt-10-e2k",
        toml_src: include_str!("../data/altlinux-spt-10-e2k.toml"),
        showrc_src: Some(include_str!("../data/altlinux-spt-10-e2k.showrc")),
    },
    BuiltinDef {
        name: "altlinux-spt-10-e2kv4",
        toml_src: include_str!("../data/altlinux-spt-10-e2kv4.toml"),
        showrc_src: Some(include_str!("../data/altlinux-spt-10-e2kv4.showrc")),
    },
    BuiltinDef {
        name: "sles-15-x86_64",
        toml_src: include_str!("../data/sles-15-x86_64.toml"),
        showrc_src: Some(include_str!("../data/sles-15-x86_64.showrc")),
    },
    BuiltinDef {
        name: "mosos-15-x86_64",
        toml_src: include_str!("../data/mosos-15-x86_64.toml"),
        showrc_src: Some(include_str!("../data/mosos-15-x86_64.showrc")),
    },
    BuiltinDef {
        name: "rosa-2021.1-x86_64",
        toml_src: include_str!("../data/rosa-2021.1-x86_64.toml"),
        showrc_src: Some(include_str!("../data/rosa-2021.1-x86_64.showrc")),
    },
];

/// A fully parsed built-in, returned by [`load`].
///
/// Both layers carry their source name in [`LayerInfo`]: `meta.layer` is
/// always `Some(LayerInfo::Builtin { name })`, and `showrc.layer` is
/// `Some(LayerInfo::BuiltinShowrc { name, macros })` when present.
/// Construction is internal to this module ([`build_snapshot`]); the
/// type is `#[non_exhaustive]` so additional cached projections can be
/// added later without a breaking change.
#[derive(Debug)]
#[non_exhaustive]
pub struct BuiltinSnapshot {
    /// Identity / whitelist patch deserialised from `data/<name>.toml`.
    /// Carries `LayerInfo::Builtin` so the resolver records the source.
    pub meta: ProfilePatch,
    /// Parsed `data/<name>.showrc` dump, when bundled. `None` for
    /// synthetic profiles (e.g. `generic`). Carries
    /// `LayerInfo::BuiltinShowrc` and macro provenance uses the
    /// `<builtin:NAME>` sentinel path from [`builtin_sentinel_path`].
    pub showrc: Option<ProfilePatch>,
}

/// Returns the names of every built-in profile shipped with this build,
/// in `REGISTRY` declaration order. The first entry is always
/// [`DEFAULT_BUILTIN`]. Order is stable across runs but not alphabetical
/// — sort at the call site if you need that.
pub fn names() -> &'static [&'static str] {
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| REGISTRY.iter().map(|d| d.name).collect())
}

/// Look up a built-in by name. Returns `None` for unknown names so the
/// resolver can raise a typed error with helpful context.
///
/// # Examples
///
/// ```
/// use rpm_spec_profile::builtin;
/// let snap = builtin::load("generic").unwrap();
/// // `generic` is the synthetic baseline — no bundled showrc.
/// assert!(snap.showrc.is_none());
/// ```
pub fn load(name: &str) -> Option<&'static BuiltinSnapshot> {
    // Resolve to the `&'static str` key the HashMap stores so we can
    // thread it into the per-entry lazy builder (which must be `'static`).
    let (static_name, slot) = CACHE.get_key_value(name)?;
    Some(slot.get_or_init(|| build_snapshot(static_name)))
}

/// One lazy slot per registry entry, keyed by name. The outer
/// `LazyLock` builds the map once; per-entry `OnceLock` parses the
/// bundled TOML + showrc on first access.
static CACHE: LazyLock<HashMap<&'static str, OnceLock<BuiltinSnapshot>>> =
    LazyLock::new(|| REGISTRY.iter().map(|d| (d.name, OnceLock::new())).collect());

fn build_snapshot(name: &'static str) -> BuiltinSnapshot {
    // `name` came from the CACHE map, which was built from REGISTRY —
    // this lookup cannot fail, but be defensive in case some future
    // caller misuses the private function.
    let def: &'static BuiltinDef = REGISTRY
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("BUG: build_snapshot called with non-registry name {name}"));

    // `expect` is intentional: TOML/showrc files are embedded at compile
    // time, so malformed content is a build bug, not a runtime error.
    // The test `every_builtin_loads_cleanly` guards against shipping a
    // broken file.
    let raw: RawProfile = toml::from_str(def.toml_src)
        .unwrap_or_else(|e| panic!("BUG: data/{}.toml is malformed: {e}", def.name));
    let meta = raw.into_patch(def.name);

    let showrc = def.showrc_src.map(|text| {
        let sentinel = builtin_sentinel_path(def.name);
        let mut patch = showrc::parse(text, Some(&sentinel));
        // Replace the showrc parser's `Showrc { path, macros }` layer
        // with the bundled-source flavour so `profile show` renders it
        // as "builtin-showrc" rather than as a file path the user can't
        // open.
        let count = patch.macros.len();
        patch.layer = Some(LayerInfo::BuiltinShowrc {
            name: Cow::Borrowed(def.name),
            macros: count,
        });
        patch
    });

    BuiltinSnapshot { meta, showrc }
}

/// On-disk representation of a built-in profile file. Kept private —
/// users describe their own profiles via `[profiles.X.*]` in
/// `.rpmspec.toml`, not by writing TOML matching this shape.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawProfile {
    identity: RawIdentity,
    licenses: RawList,
    groups: RawList,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawIdentity {
    name: Option<String>,
    family: Option<Family>,
    vendor: Option<String>,
    #[serde(rename = "dist-tag")]
    dist_tag: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawList {
    mode: Option<ValidationMode>,
    allow: Vec<String>,
}

impl RawProfile {
    fn into_patch(self, builtin_name: &'static str) -> ProfilePatch {
        let identity = IdentityPatch {
            name: self.identity.name,
            family: self.identity.family,
            vendor: self.identity.vendor,
            dist_tag: self.identity.dist_tag,
        };

        let licenses = if has_list_content(&self.licenses) {
            Some(ListPatch {
                mode: self.licenses.mode,
                allow: self.licenses.allow,
                replace: false,
            })
        } else {
            None
        };
        let groups = if has_list_content(&self.groups) {
            Some(ListPatch {
                mode: self.groups.mode,
                allow: self.groups.allow,
                replace: false,
            })
        } else {
            None
        };

        ProfilePatch {
            identity,
            macros: Vec::new(),
            rpmlib: Vec::new(),
            arch: Default::default(),
            licenses,
            groups,
            layer: Some(LayerInfo::Builtin {
                name: Cow::Borrowed(builtin_name),
            }),
        }
    }
}

fn has_list_content(l: &RawList) -> bool {
    l.mode.is_some() || !l.allow.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_loads_and_is_minimal() {
        let snap = load("generic").expect("generic builtin exists");
        let p = &snap.meta;
        // Identity: family explicitly set, no vendor / dist-tag.
        assert_eq!(p.identity.family, Some(Family::Generic));
        assert_eq!(p.identity.name.as_deref(), Some("generic"));
        assert!(p.identity.vendor.is_none());
        assert!(p.identity.dist_tag.is_none());
        // No macros, no rpmlib features.
        assert!(p.macros.is_empty());
        assert!(p.rpmlib.is_empty());
        // Whitelists exist but explicitly Off.
        let lic = p.licenses.as_ref().expect("licenses patch");
        assert_eq!(lic.mode, Some(ValidationMode::Off));
        assert!(lic.allow.is_empty());
        // Layer recorded.
        assert!(matches!(
            p.layer.as_ref(),
            Some(LayerInfo::Builtin { name }) if name == "generic"
        ));
        // No bundled showrc for `generic`.
        assert!(snap.showrc.is_none());
    }

    #[test]
    fn unknown_builtin_returns_none() {
        assert!(load("does-not-exist").is_none());
    }

    #[test]
    fn names_lists_known_builtins() {
        let n = names();
        assert!(n.contains(&"generic"));
        // Every distribution profile collected via the SSH bootstrap.
        assert!(n.contains(&"rhel-9-x86_64"));
        assert!(n.contains(&"redos-8-x86_64"));
        assert!(n.contains(&"altlinux-10-e2k"));
        assert!(n.contains(&"sles-15-x86_64"));
    }

    #[test]
    fn generic_applied_to_empty_profile() {
        let snap = load("generic").unwrap();
        let mut profile = crate::types::Profile::default();
        profile.apply(snap.meta.clone());
        assert_eq!(profile.identity.family, Some(Family::Generic));
        assert_eq!(profile.layers.len(), 1);
    }

    #[test]
    fn distribution_builtin_carries_showrc() {
        let snap = load("rhel-9-x86_64").expect("rhel-9-x86_64 builtin exists");
        let showrc = snap.showrc.as_ref().expect("bundled showrc patch");
        // Showrc dump has macros and an arch — both must round-trip.
        assert!(showrc.macros.len() > 100, "too few macros");
        assert!(showrc.arch.build_arch.is_some(), "no build_arch detected");
        // Layer is the bundled flavour, not the file-path flavour.
        assert!(matches!(
            showrc.layer.as_ref(),
            Some(LayerInfo::BuiltinShowrc { name, .. }) if name == "rhel-9-x86_64"
        ));
    }
}
