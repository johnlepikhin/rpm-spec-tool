//! `.rpmspec.toml` shape for the profile system.
//!
//! Lives in the same crate as [`Profile`] so the schema and the runtime
//! type evolve together. The analyzer crate re-exposes this as
//! `analyzer::config::Config::profile`/`profiles`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::merge::{IdentityPatch, ListPatch, ProfilePatch};
use crate::repos::{BuildrootConfig, RepoConfig};
use crate::types::{Family, MacroEntry, MacroValue, Provenance, ValidationMode};

/// Top-level slice that `analyzer::config::Config` embeds.
///
/// Two parts: the name of the active profile (which can also be
/// supplied via CLI override) and a map of user-defined named profiles.
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct ProfileSection {
    /// Active profile name. May reference a built-in
    /// (see [`crate::builtin::names`]) or a key from [`profiles`]. If
    /// `None`, the resolver falls back to the `generic` built-in.
    pub profile: Option<String>,
    /// Per-name profile descriptors.
    pub profiles: BTreeMap<String, ProfileEntry>,
}

impl ProfileSection {
    /// Build a section from its component parts. Used by the analyzer
    /// crate to bridge its own config struct into the resolver.
    pub fn new(profile: Option<String>, profiles: BTreeMap<String, ProfileEntry>) -> Self {
        Self { profile, profiles }
    }
}

/// One entry of `[profiles.<name>]`.
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct ProfileEntry {
    /// Base built-in profile to extend. Default `"generic"`.
    pub extends: Option<String>,
    /// Path to a `rpm --showrc` dump, relative to `.rpmspec.toml`.
    pub showrc_file: Option<PathBuf>,
    /// Identity overrides applied after the showrc layer.
    pub identity: ProfileIdentityOverride,
    /// Macro overrides — last write wins on the [`Profile`].
    pub macros: BTreeMap<String, MacroOverride>,
    /// License whitelist override.
    pub licenses: Option<ListOverride>,
    /// Group whitelist override.
    pub groups: Option<ListOverride>,
    /// Per-repository configuration for this profile. Keys are
    /// human-friendly identifiers (e.g. `"baseos"`, `"appstream"`).
    /// One repo with the same id from an inherited (`extends`)
    /// built-in is replaced wholesale; `enabled = false` masks an
    /// inherited entry without redefining its URL. Validated against
    /// `[a-z0-9_-]{1,64}` at resolve time.
    pub repos: BTreeMap<String, RepoConfig>,
    /// Base buildroot packages — what the chroot ships before
    /// processing `BuildRequires`. Defaults to an empty list when
    /// the profile inherits no built-in defaults.
    pub buildroot: BuildrootConfig,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct ProfileIdentityOverride {
    pub name: Option<String>,
    pub family: Option<Family>,
    pub vendor: Option<String>,
    pub dist_tag: Option<String>,
}

/// A `[profiles.X.macros]` value. Accepts either a bare string (literal
/// value) or a table form for parameterised / multi-line macros.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum MacroOverride {
    Literal(String),
    Full {
        value: String,
        #[serde(default)]
        opts: Option<String>,
        #[serde(default)]
        multiline: bool,
    },
}

impl MacroOverride {
    fn into_entry(self) -> MacroEntry {
        match self {
            MacroOverride::Literal(s) => MacroEntry {
                value: MacroValue::Literal(s),
                opts: None,
                provenance: Provenance::Override,
            },
            MacroOverride::Full {
                value,
                opts,
                multiline,
            } => {
                // Multiline bodies must always become `Raw` regardless
                // of `%` content — the renderer relies on `multiline`
                // to lay out the value across lines. For single-line
                // bodies the literal/raw split is identical to the CLI
                // `--define` path, so we delegate to the shared helper.
                let v = if multiline {
                    MacroValue::Raw {
                        body: value,
                        multiline: true,
                    }
                } else {
                    MacroValue::from_user_body(value)
                };
                MacroEntry {
                    value: v,
                    opts,
                    provenance: Provenance::Override,
                }
            }
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct ListOverride {
    pub mode: Option<ValidationMode>,
    pub allow: Vec<String>,
    pub replace: bool,
}

/// One entry of `[targets.<name>]` — a release target set.
///
/// A target set names a collection of distribution profiles that the
/// same `.spec` is expected to build under, plus optional shared
/// `--define` values applied uniformly and per-profile overrides for
/// outlier platforms.
///
/// Profiles named here must either be a built-in (see
/// [`crate::builtin::names`]) or a key from
/// [`ProfileSection::profiles`]. Resolution failures (unknown profile
/// name, etc.) are caught by the resolver rather than at TOML parse
/// time — `[targets.*]` is just data here.
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct TargetEntry {
    /// Ordered list of profile names. Duplicates are preserved here
    /// but the resolver collapses them — keeping the order lets users
    /// drive deterministic output (table column order, JSON array
    /// order).
    pub profiles: Vec<String>,
    /// `NAME = "VALUE"` pairs applied as `--define`-style overrides to
    /// every profile in this target. Layered between the profile's own
    /// `[profiles.X.macros]` and any CLI-supplied `--define` — CLI
    /// always wins.
    pub defines: BTreeMap<String, String>,
    /// Outlier overrides for individual profiles inside this target.
    /// Keys must appear in [`Self::profiles`]; resolver rejects
    /// unknown keys so a typo can't silently no-op.
    pub profile_overrides: BTreeMap<String, TargetProfileOverride>,
}

/// Per-profile override block inside a `[targets.<name>.profile-overrides.<profile>]`
/// section. Only `defines` for now — future extensions (per-profile
/// severity tweaks, conditional inclusion) land here.
#[derive(Debug, Default, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default, rename_all = "kebab-case")]
#[non_exhaustive]
pub struct TargetProfileOverride {
    /// Extra `--define`-style overrides applied only when resolving
    /// this profile within this target. Layered after the target-wide
    /// `defines` and still before any CLI `--define`.
    pub defines: BTreeMap<String, String>,
}

impl TargetEntry {
    /// Construct an ad-hoc target entry from a profile list. Used by
    /// `matrix check --profiles a,b,c` so the CLI can avoid struct
    /// expressions on this `#[non_exhaustive]` type from outside the
    /// crate.
    pub fn from_profiles(profiles: Vec<String>) -> Self {
        Self {
            profiles,
            ..Self::default()
        }
    }
}

impl ProfileEntry {
    /// Build the user-override [`ProfilePatch`] that the resolver applies
    /// after the showrc layer. Identity defaults (including the human
    /// `name`) are populated from `profile_key` when the user did not
    /// override them.
    pub fn override_patch(&self, profile_key: &str) -> ProfilePatch {
        let mut fields = Vec::new();

        let identity = IdentityPatch {
            // Always set the human-readable name (falls back to the
            // profile's TOML key). Users can override.
            name: Some(
                self.identity
                    .name
                    .clone()
                    .unwrap_or_else(|| profile_key.to_string()),
            ),
            family: self.identity.family,
            vendor: self.identity.vendor.clone(),
            dist_tag: self.identity.dist_tag.clone(),
        };
        if self.identity.family.is_some() {
            fields.push("identity.family".to_string());
        }
        if self.identity.vendor.is_some() {
            fields.push("identity.vendor".to_string());
        }
        if self.identity.dist_tag.is_some() {
            fields.push("identity.dist-tag".to_string());
        }

        let mut macros = Vec::with_capacity(self.macros.len());
        for (name, ov) in &self.macros {
            macros.push((name.clone(), ov.clone().into_entry()));
            fields.push(format!("macros.{name}"));
        }

        let licenses = self.licenses.as_ref().map(|l| {
            fields.push("licenses".to_string());
            ListPatch {
                mode: l.mode,
                allow: l.allow.clone(),
                replace: l.replace,
            }
        });
        let groups = self.groups.as_ref().map(|g| {
            fields.push("groups".to_string());
            ListPatch {
                mode: g.mode,
                allow: g.allow.clone(),
                replace: g.replace,
            }
        });

        let layer = (!fields.is_empty()).then_some(crate::types::LayerInfo::Override { fields });

        ProfilePatch {
            identity,
            macros,
            rpmlib: Vec::new(),
            arch: Default::default(),
            licenses,
            groups,
            layer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_section() {
        let s = r#"
profile = "rhel-9"

[profiles.rhel-9]
showrc-file = "vendor/rpm-showrc-rhel9.txt"
"#;
        let sec: ProfileSection = toml::from_str(s).unwrap();
        assert_eq!(sec.profile.as_deref(), Some("rhel-9"));
        let entry = sec.profiles.get("rhel-9").unwrap();
        assert_eq!(
            entry.showrc_file.as_deref(),
            Some(std::path::Path::new("vendor/rpm-showrc-rhel9.txt"))
        );
        assert!(entry.identity.family.is_none());
        assert!(entry.macros.is_empty());
    }

    #[test]
    fn parses_macro_override_short_and_full_forms() {
        let s = r#"
[profiles.X]
[profiles.X.macros]
_vendor = "acme"
custom = { value = "%{nil}", opts = "(n:)" }
"#;
        let sec: ProfileSection = toml::from_str(s).unwrap();
        let entry = sec.profiles.get("X").unwrap();
        assert!(matches!(
            entry.macros.get("_vendor"),
            Some(MacroOverride::Literal(s)) if s == "acme"
        ));
        assert!(matches!(
            entry.macros.get("custom"),
            Some(MacroOverride::Full { opts, .. }) if opts.as_deref() == Some("(n:)")
        ));
    }

    #[test]
    fn rejects_unknown_fields() {
        let s = r#"
[profiles.X]
random-key = 1
"#;
        let err = toml::from_str::<ProfileSection>(s).unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }

    #[test]
    fn override_patch_marks_only_set_fields() {
        let mut entry = ProfileEntry::default();
        entry.identity.vendor = Some("acme".into());
        entry
            .macros
            .insert("_vendor".into(), MacroOverride::Literal("acme".into()));
        let patch = entry.override_patch("X");

        // identity.name is always populated from profile key.
        assert_eq!(patch.identity.name.as_deref(), Some("X"));
        // Family and dist_tag not overridden → None.
        assert!(patch.identity.family.is_none());
        assert!(patch.identity.dist_tag.is_none());
        // Macros listed.
        assert_eq!(patch.macros.len(), 1);
        assert_eq!(patch.macros[0].0, "_vendor");
        match &patch.layer {
            Some(crate::types::LayerInfo::Override { fields }) => {
                assert!(fields.iter().any(|f| f == "identity.vendor"));
                assert!(fields.iter().any(|f| f == "macros._vendor"));
                assert!(!fields.iter().any(|f| f == "identity.family"));
            }
            other => panic!("expected Override layer, got {other:?}"),
        }
    }

    #[test]
    fn override_patch_with_no_fields_has_no_layer() {
        // A profile entry that only sets `extends` and `showrc-file`
        // contributes nothing on the override layer (those are consumed
        // by the resolver before override_patch is called).
        let entry = ProfileEntry::default();
        let patch = entry.override_patch("X");
        assert!(patch.layer.is_none());
    }

    #[test]
    fn macro_override_full_with_percent_becomes_raw() {
        let ov = MacroOverride::Full {
            value: "%{_libdir}/foo".into(),
            opts: None,
            multiline: false,
        };
        let entry = ov.into_entry();
        assert!(matches!(entry.value, MacroValue::Raw { .. }));
    }

    #[test]
    fn parses_minimal_target_entry() {
        let s = r#"
profiles = ["rhel-9-x86_64", "altlinux-10-x86_64"]
"#;
        let entry: TargetEntry = toml::from_str(s).unwrap();
        assert_eq!(entry.profiles, vec!["rhel-9-x86_64", "altlinux-10-x86_64"]);
        assert!(entry.defines.is_empty());
        assert!(entry.profile_overrides.is_empty());
    }

    #[test]
    fn parses_target_with_defines_and_overrides() {
        // Verifies the nested `profile-overrides` shape — the
        // documented kebab-case form drives `rename_all`.
        let s = r#"
profiles = ["rhel-9-x86_64", "altlinux-10-e2k"]

[defines]
product_build = "1"

[profile-overrides."altlinux-10-e2k"]
[profile-overrides."altlinux-10-e2k".defines]
use_jit = "0"
"#;
        let entry: TargetEntry = toml::from_str(s).unwrap();
        assert_eq!(
            entry.defines.get("product_build").map(String::as_str),
            Some("1")
        );
        let override_block = entry
            .profile_overrides
            .get("altlinux-10-e2k")
            .expect("override for e2k");
        assert_eq!(
            override_block.defines.get("use_jit").map(String::as_str),
            Some("0")
        );
    }

    #[test]
    fn target_entry_rejects_unknown_field() {
        let s = r#"
profiles = ["rhel-9-x86_64"]
random-key = "nope"
"#;
        let err = toml::from_str::<TargetEntry>(s).unwrap_err();
        assert!(err.to_string().contains("unknown"));
    }

    #[test]
    fn target_entry_default_has_empty_collections() {
        let entry = TargetEntry::default();
        assert!(entry.profiles.is_empty());
        assert!(entry.defines.is_empty());
        assert!(entry.profile_overrides.is_empty());
    }
}
