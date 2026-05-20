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
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{LazyLock, OnceLock};

use serde::Deserialize;

use crate::merge::{IdentityPatch, ListPatch, ProfilePatch};
use crate::repos::{BuildrootConfig, RepoConfig, RepoSet, validate_repo_id};
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
/// Layers carry their source name in [`LayerInfo`]: `meta.layer` is
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
    /// Built-in `[repos.*]` + `[buildroot]` blocks from `data/<name>.toml`.
    /// `None` when neither block was authored (the common case for
    /// `generic` and the family-baseline profiles without a public
    /// mirror). The resolver seeds [`crate::Profile::repos`] from this
    /// before overlaying user-side `[profiles.X.repos]` so a profile
    /// can ship "works out of the box against the public mirror" while
    /// the user keeps the option to override any individual id.
    pub repos: Option<RepoSet>,
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
    let RawProfileParts { meta, repos } = raw.into_parts(def.name);

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

    BuiltinSnapshot { meta, showrc, repos }
}

/// On-disk representation of a built-in profile file. Kept private —
/// users describe their own profiles via `[profiles.X.*]` in
/// `.rpmspec.toml`, not by writing TOML matching this shape.
///
/// Schema parallels `crate::config_layer::ProfileEntry` for the
/// fields that make sense at the built-in tier (no `extends`,
/// `showrc-file`, or macro overrides — those are either redundant
/// against the bundled showrc or specific to user TOML).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawProfile {
    identity: RawIdentity,
    licenses: RawList,
    groups: RawList,
    /// Per-repo configuration. Keys validated as
    /// `[a-z0-9_-]{1,64}` at load time.
    #[serde(default)]
    repos: BTreeMap<String, RepoConfig>,
    /// Buildroot baseline: `base-packages` (the chroot seed) and
    /// `implicit-buildrequires` (shadow BRs the platform always
    /// provides).
    #[serde(default)]
    buildroot: BuildrootConfig,
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

/// Outputs of [`RawProfile::into_parts`] — the two artefacts a
/// built-in TOML splits into. Kept as a struct (rather than a bare
/// tuple) so future additions (e.g. a TestSuite block, signature
/// metadata) don't churn every caller.
struct RawProfileParts {
    meta: ProfilePatch,
    repos: Option<RepoSet>,
}

impl RawProfile {
    /// Split a parsed built-in into the layered `ProfilePatch`
    /// (identity / licenses / groups) and the standalone `RepoSet`.
    /// They live in different slots on [`crate::Profile`] — repos
    /// have atomic-per-id replacement semantics, not patched merge —
    /// so the pieces flow through different paths in the resolver.
    fn into_parts(
        self,
        builtin_name: &'static str,
    ) -> RawProfileParts {
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

        // Validate every repo id at load time; a typo in a built-in
        // TOML file is a build-time bug, so panic is appropriate.
        for id in self.repos.keys() {
            if let Err(reason) = validate_repo_id(id) {
                panic!(
                    "BUG: built-in `{builtin_name}` has invalid repo id `{id}`: {reason}",
                );
            }
        }
        let repos = if self.repos.is_empty()
            && self.buildroot.base_packages.is_empty()
            && self.buildroot.implicit_buildrequires.is_empty()
        {
            None
        } else {
            Some(RepoSet {
                repos: self.repos,
                buildroot: self.buildroot,
            })
        };

        let meta = ProfilePatch {
            identity,
            macros: Vec::new(),
            rpmlib: Vec::new(),
            arch: Default::default(),
            licenses,
            groups,
            layer: Some(LayerInfo::Builtin {
                name: Cow::Borrowed(builtin_name),
            }),
        };
        RawProfileParts { meta, repos }
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
        // No bundled repos for `generic` — it's the synthetic
        // baseline with no platform-specific defaults.
        assert!(snap.repos.is_none());
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

    #[test]
    fn altlinux_11_x86_64_ships_classic_repo() {
        // Iteration A landed enabled apt-rpm `classic` repos for ALT
        // built-ins so the lints work out of the box without per-user
        // config. Regression test pins both the existence and the
        // expected shape (kind = apt-rpm, enabled, base role).
        let snap = load("altlinux-11-x86_64").expect("altlinux-11-x86_64 builtin exists");
        let repos = snap.repos.as_ref().expect("ALT 11 must ship repos");
        let classic = repos
            .repos
            .get("classic")
            .expect("ALT 11 must expose `classic` component");
        assert!(classic.enabled, "classic must be enabled in the built-in");
        assert_eq!(classic.kind, crate::repos::RepoKind::AptRpm);
        assert!(matches!(classic.role, crate::repos::RepoRole::Base));
        let url = classic
            .baseurl
            .as_deref()
            .expect("ALT 11 classic must declare a baseurl");
        assert!(url.contains("p11"), "expected p11 in baseurl, got: {url}");
        assert!(
            url.contains("$basearch"),
            "ALT URL must template via $basearch, got: {url}"
        );
        // Buildroot baseline must include rpm-build (the build-time
        // contract the chroot must satisfy before any spec runs).
        assert!(
            repos
                .buildroot
                .base_packages
                .iter()
                .any(|p| p == "rpm-build"),
            "ALT 11 buildroot missing rpm-build: {:?}",
            repos.buildroot.base_packages,
        );
    }

    #[test]
    fn vendor_internal_builtin_has_no_repos_but_ships_buildroot() {
        // ALT-SPT / REDos / MosOS / Rosa / SLES / RHEL each have no
        // public mirror; the built-in TOML provides a buildroot
        // baseline plus commented-example guidance in comments — but
        // no actual [repos.*] block. The snapshot's `repos` slot is
        // therefore `Some(...)` (because buildroot is non-empty) but
        // the inner `repos` map is empty.
        for name in [
            "altlinux-spt-10-x86_64",
            "redos-8-x86_64",
            "mosos-15-x86_64",
            "rosa-2021.1-x86_64",
            "sles-15-x86_64",
            "rhel-9-x86_64",
        ] {
            let snap = load(name).unwrap_or_else(|| panic!("builtin `{name}` not found"));
            let r = snap
                .repos
                .as_ref()
                .unwrap_or_else(|| panic!("`{name}` should expose buildroot"));
            assert!(
                r.repos.is_empty(),
                "`{name}` ships no enabled repos by default (got {} entries)",
                r.repos.len(),
            );
            assert!(
                !r.buildroot.base_packages.is_empty(),
                "`{name}` must declare a buildroot baseline"
            );
        }
    }

    #[test]
    fn every_builtin_loads_cleanly() {
        // Guard against shipping a broken TOML or a typo in the
        // catalogue; every name we register must yield a snapshot.
        for &name in names() {
            let snap = load(name)
                .unwrap_or_else(|| panic!("builtin `{name}` registered but failed to load"));
            // Layer trail is always populated on the meta patch — the
            // resolver depends on it to render `profile show`.
            assert!(
                snap.meta.layer.is_some(),
                "`{name}` meta patch missing LayerInfo"
            );
        }
    }
}
