//! Core data types for distribution profiles.
//!
//! A [`Profile`] is the resolved, fully-merged target environment a `.spec`
//! file is being analyzed against. It is layered: a builtin baseline
//! ([`LayerInfo::Builtin`]) underlies an optional `rpm --showrc` dump
//! ([`LayerInfo::Showrc`]) which underlies user overrides
//! ([`LayerInfo::Override`]). Higher layers win on key conflicts;
//! provenance on each [`MacroEntry`] records which layer the final value
//! came from.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Resolved profile passed to lints via the analyzer's
/// `Lint::set_profile` hook.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Profile {
    pub identity: Identity,
    pub macros: MacroRegistry,
    pub rpmlib: RpmlibFeatures,
    pub arch: ArchInfo,
    pub licenses: LicenseList,
    pub groups: GroupList,
    /// Layer trail in application order (low → high precedence) — useful
    /// for `rpm-spec-tool profile show` and debugging.
    pub layers: Vec<LayerInfo>,
}

/// Identity of the target distribution.
///
/// `family`, `vendor`, and `dist_tag` are normally inferred from a
/// `rpm --showrc` dump (see [`crate::autodetect`]). User overrides in
/// `[profiles.X.identity]` win over auto-detected values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Identity {
    /// Human-readable label — defaults to the config key (e.g. `"rhel-9"`).
    pub name: String,
    /// Distribution family. `None` means "no recognised marker macro was
    /// found and the user did not pick one explicitly" — semantically
    /// distinct from `Some(Family::Generic)`, which is an explicit
    /// opt-out (e.g. a custom internal distro).
    pub family: Option<Family>,
    pub vendor: Option<String>,
    pub dist_tag: Option<String>,
}

/// Distribution family — coarse classification used by family-aware lints.
///
/// Not all marker macros translate 1:1: derivative distributions
/// (AlmaLinux, Rocky, …) report themselves as [`Family::Rhel`] because the
/// macros they expose are the RHEL ones plus their own brand marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Family {
    Fedora,
    Rhel,
    Opensuse,
    Alt,
    Mageia,
    /// Generic / unknown distribution. Use when the spec is intentionally
    /// distribution-agnostic.
    Generic,
}

/// Registry of every macro known to the profile.
///
/// We keep raw bodies verbatim — lints that want to resolve `%{...}`
/// references do so themselves (the registry is intentionally dumb so the
/// shape of the data round-trips through `profile show --full` without
/// information loss).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MacroRegistry {
    pub entries: BTreeMap<String, MacroEntry>,
}

impl MacroRegistry {
    pub fn get(&self, name: &str) -> Option<&MacroEntry> {
        self.entries.get(name)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn insert(&mut self, name: impl Into<String>, entry: MacroEntry) {
        self.entries.insert(name.into(), entry);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MacroEntry {
    pub value: MacroValue,
    /// Parameter spec from `rpm --showrc`, e.g. `"(qp:m:)"` for
    /// `%__apply_patch`. `None` for unparameterized macros.
    pub opts: Option<String>,
    pub provenance: Provenance,
}

impl MacroEntry {
    pub fn literal(value: impl Into<String>, provenance: Provenance) -> Self {
        Self {
            value: MacroValue::Literal(value.into()),
            opts: None,
            provenance,
        }
    }

    /// Returns the single-line literal value, if any. `Raw` and `Builtin`
    /// return `None` — callers that want fallback rendering should match
    /// on [`MacroValue`] explicitly.
    pub fn as_literal(&self) -> Option<&str> {
        match &self.value {
            MacroValue::Literal(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Semantic equivalence on `(opts, value)` — answers "is this the
    /// same macro?" ignoring `Provenance`. A macro that arrived from a
    /// builtin showrc in one profile and from a user override in
    /// another still counts as equivalent when both sides agree on
    /// `opts` and on the value body.
    ///
    /// Not exposed as `PartialEq` for the whole struct because
    /// `Provenance` is semantically meaningful in most contexts —
    /// silent equality ignoring it would surprise callers grep'ing
    /// `==`.
    pub fn is_equivalent(&self, other: &Self) -> bool {
        self.opts == other.opts && self.value == other.value
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MacroValue {
    /// Single-line body with no `%{…}` references — safe to use as a
    /// resolved string.
    Literal(String),
    /// Multi-line body, lua-script, or body containing macro references.
    /// Kept verbatim; downstream consumers decide whether to expand.
    Raw { body: String, multiline: bool },
    /// Stub level (`-20: name\t<builtin>`) — name is known to rpm, body
    /// is implemented in C, no textual value exists.
    Builtin,
}

/// Body-level equality. The `multiline` flag on `Raw` is bookkeeping
/// for the renderer (it tells `profile show --full` whether to print
/// `<multiline N chars>` vs the body inline) and is intentionally not
/// part of equality — two `Raw` values with the same body but different
/// `multiline` are the same macro.
///
/// `Literal` vs `Raw` with identical string content are intentionally
/// **not** equal: the parser distinguishes resolved literals from
/// bodies awaiting `%{…}` expansion, and equating them would erase a
/// real semantic difference.
impl PartialEq for MacroValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (MacroValue::Literal(a), MacroValue::Literal(b)) => a == b,
            (MacroValue::Builtin, MacroValue::Builtin) => true,
            (MacroValue::Raw { body: a, .. }, MacroValue::Raw { body: b, .. }) => a == b,
            _ => false,
        }
    }
}

/// Where a macro entry came from. Set by the merge logic so
/// `profile show` can explain why a given macro has its current value.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "lowercase")]
pub enum Provenance {
    /// Loaded from `data/<name>.toml`.
    Builtin { profile: String },
    /// Loaded from a `rpm --showrc` dump.
    Showrc { level: i16, path: Option<PathBuf> },
    /// Set by `[profiles.X.macros]` in `.rpmspec.toml`.
    Override,
}

/// `Features supported by rpmlib:` section of `rpm --showrc`.
///
/// Keys are the verbatim feature strings (`"rpmlib(RichDependencies)"`),
/// values are the minimum rpm version that introduced them
/// (`"4.12.0-1"`). Useful for advanced dep lints (e.g. flagging Rich deps
/// against an rpm too old to support them).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RpmlibFeatures {
    pub features: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ArchInfo {
    pub build_arch: Option<String>,
    pub build_os: Option<String>,
    pub compatible_archs: Vec<String>,
    /// Raw `optflags` line from `RPMRC VALUES:` — keeps `%{…}` refs so
    /// callers can resolve them against the macro registry themselves.
    pub optflags_template: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LicenseList {
    pub allowed: BTreeSet<String>,
    pub mode: ValidationMode,
}

impl LicenseList {
    pub fn is_allowed(&self, license: &str) -> bool {
        self.allowed.contains(license)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GroupList {
    pub allowed: BTreeSet<String>,
    pub mode: ValidationMode,
}

impl GroupList {
    pub fn is_allowed(&self, group: &str) -> bool {
        self.allowed.contains(group)
    }
}

/// Three-valued whitelist mode. `Off` is the contract: consumer lints
/// MUST emit nothing when their list is `Off`. `Warn`/`Strict` differ
/// only in default severity for the consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidationMode {
    Off,
    Warn,
    Strict,
}

impl Default for ValidationMode {
    fn default() -> Self {
        Self::Off
    }
}

/// One layer in the merge chain — recorded so `profile show` can render
/// the layer trail. `#[non_exhaustive]` because new layer kinds (CLI
/// flags, env vars, future overlay sources) will be added over time —
/// downstream `match`es must include a wildcard arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
#[non_exhaustive]
pub enum LayerInfo {
    /// TOML metadata from `crates/profile/data/<name>.toml`. The `name`
    /// is always one of the static literals from the built-in registry,
    /// so we model it as `Cow<'static, str>` to skip the per-resolve
    /// allocation while staying serde-friendly.
    Builtin {
        name: Cow<'static, str>,
    },
    /// `rpm --showrc` dump bundled inside the binary as part of a
    /// distribution builtin (`data/<name>.showrc`). Distinct from
    /// [`LayerInfo::Showrc`] so `profile show` can render the source
    /// — the bundled dump has no on-disk path the user can edit.
    BuiltinShowrc {
        name: Cow<'static, str>,
        macros: usize,
    },
    Showrc {
        path: PathBuf,
        macros: usize,
    },
    Override {
        fields: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_empty() {
        let p = Profile::default();
        assert!(p.macros.is_empty());
        assert!(p.licenses.allowed.is_empty());
        assert_eq!(p.licenses.mode, ValidationMode::Off);
        // Default family is `None` (not detected, not explicitly chosen).
        assert!(p.identity.family.is_none());
    }

    #[test]
    fn license_mode_off_contract_default() {
        // Contract for future RPM024/025-style lints: when the list's
        // mode is `Off`, consumers MUST emit nothing — `is_allowed`
        // can still be called but its answer doesn't change behaviour.
        let l = LicenseList::default();
        assert_eq!(l.mode, ValidationMode::Off);
        // Empty whitelist + Off mode → nothing is "allowed", but
        // consumer lints are required to ignore the result entirely.
        assert!(!l.is_allowed("MIT"));
        // The same goes for groups.
        let g = GroupList::default();
        assert_eq!(g.mode, ValidationMode::Off);
        assert!(!g.is_allowed("System Environment/Daemons"));
    }

    #[test]
    fn macro_entry_as_literal() {
        let e = MacroEntry::literal(
            ".el9",
            Provenance::Showrc {
                level: -13,
                path: None,
            },
        );
        assert_eq!(e.as_literal(), Some(".el9"));
    }

    #[test]
    fn raw_macro_is_not_literal() {
        let e = MacroEntry {
            value: MacroValue::Raw {
                body: "%{lua: ...}".into(),
                multiline: true,
            },
            opts: None,
            provenance: Provenance::Override,
        };
        assert_eq!(e.as_literal(), None);
    }

    #[test]
    fn family_serde_lowercase() {
        let toml_str = "family = \"rhel\"";
        #[derive(Deserialize)]
        struct W {
            family: Family,
        }
        let w: W = toml::from_str(toml_str).unwrap();
        assert_eq!(w.family, Family::Rhel);
    }

    #[test]
    fn macro_value_eq_literal_pair() {
        assert_eq!(
            MacroValue::Literal(".el9".into()),
            MacroValue::Literal(".el9".into())
        );
        assert_ne!(
            MacroValue::Literal(".el9".into()),
            MacroValue::Literal(".el8".into())
        );
    }

    #[test]
    fn macro_value_eq_builtin_pair() {
        assert_eq!(MacroValue::Builtin, MacroValue::Builtin);
    }

    #[test]
    fn macro_value_eq_raw_ignores_multiline_flag() {
        let one_line = MacroValue::Raw {
            body: "%{?_with_foo}".into(),
            multiline: false,
        };
        let same_marked_multi = MacroValue::Raw {
            body: "%{?_with_foo}".into(),
            multiline: true,
        };
        assert_eq!(one_line, same_marked_multi);
    }

    #[test]
    fn macro_value_eq_rejects_cross_variant() {
        // The parser distinguishes resolved literals from bodies awaiting
        // expansion — equating Literal("/usr/bin/7za") and Raw{body:
        // "/usr/bin/7za"} would erase that distinction.
        let lit = MacroValue::Literal("/usr/bin/7za".into());
        let raw = MacroValue::Raw {
            body: "/usr/bin/7za".into(),
            multiline: false,
        };
        assert_ne!(lit, raw);
        assert_ne!(MacroValue::Builtin, lit);
    }

    #[test]
    fn macro_entry_is_equivalent_distinguishes_by_opts() {
        let mut a = MacroEntry::literal("x", Provenance::Override);
        a.opts = Some("v:".into());
        let mut b = MacroEntry::literal("x", Provenance::Override);
        b.opts = Some("n:".into());
        let mut c = MacroEntry::literal(
            "x",
            Provenance::Showrc {
                level: -13,
                path: None,
            },
        );
        c.opts = Some("v:".into());
        assert!(!a.is_equivalent(&b), "opts differ → not equivalent");
        assert!(
            a.is_equivalent(&c),
            "only provenance differs → still equivalent"
        );
    }
}
