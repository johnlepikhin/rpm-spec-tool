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
#[non_exhaustive]
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

impl Family {
    /// `true` for families whose canonical builders (Mock, Koji, OBS,
    /// `hasher`) run the build inside an offline chroot. Used by
    /// rules that enforce conventions tied to that assumption (no
    /// network in `%build`, mandatory `BuildRequires:` for build
    /// tools, etc.).
    #[must_use]
    pub fn has_offline_build_chroot(self) -> bool {
        matches!(
            self,
            Self::Fedora | Self::Rhel | Self::Opensuse | Self::Mageia | Self::Alt
        )
    }
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

    /// Recursively expand a macro name to its literal string value,
    /// following `%{other_macro}` references until a literal is reached
    /// or `depth` drops to zero. Returns `None` when the macro is
    /// undefined, when the chain hits a non-literal that can't be
    /// resolved (`Raw` body with unresolvable references, `Builtin`),
    /// when expansion depth is exceeded, when an intermediate body
    /// exceeds [`MAX_EXPAND_BODY`], or when the accumulated output
    /// exceeds [`MAX_EXPAND_OUTPUT`] (both caps defend against
    /// malicious profiles that try to amplify a small body into
    /// megabyte/gigabyte output via repeated reference expansion).
    ///
    /// Only the `%{name}` and `%name` syntactic forms are recognised;
    /// parameterised macros (`%macro(arg)`), conditional references
    /// (`%{?name}`, `%{!?name}`), defaulting (`%{name:default}`), and
    /// lua substitutions return `None` — they need expansion semantics
    /// that aren't available at lint time.
    ///
    /// `depth` decrements once per `Raw` expansion pass, so cycles
    /// (`a → %{b}`, `b → %{a}`) terminate within `depth` iterations.
    /// 8 is a safe default for path macros (`_libdir = %{_prefix}/lib64`
    /// → `_prefix = /usr` resolves in 2). The argument is `u8` to
    /// document the expected magnitude; callers needing deeper chains
    /// must explicitly justify the value.
    #[must_use]
    pub fn expand_to_literal(&self, name: &str, depth: u8) -> Option<String> {
        if depth == 0 {
            tracing::debug!(macro_name = %name, "expand_to_literal: depth limit reached");
            return None;
        }
        let entry = self.get(name)?;
        match &entry.value {
            MacroValue::Literal(s) => Some(s.clone()),
            MacroValue::Builtin => None,
            MacroValue::Raw { body, .. } => self.expand_body(body, depth - 1),
        }
    }

    /// Substitute every `%{name}` / `%name` reference in `body` by
    /// recursive expansion. Returns `None` if any reference fails to
    /// resolve, since a partially-expanded path would yield wrong
    /// caller decisions (e.g. wrong lint suggestions).
    ///
    /// Only plain `%name` and unconditional `%{name}` references are
    /// resolved here. Conditional (`%{?name}`, `%{!?name}`),
    /// defaulted (`%{name:default}`), shell (`%(...)`), arithmetic
    /// (`%[...]`), and any other macro token returns `None` — those
    /// forms need runtime expansion semantics not available at lint
    /// time. `%%` is preserved as a literal `%` in the output.
    fn expand_body(&self, body: &str, depth: u8) -> Option<String> {
        use crate::macro_lexer::{MacroKind, scan_macro_ref};

        if body.len() > MAX_EXPAND_BODY {
            tracing::debug!(
                body_len = body.len(),
                cap = MAX_EXPAND_BODY,
                "expand_body: input body exceeds cap"
            );
            return None;
        }
        // Reject non-ASCII bodies up-front. RPM macro names are
        // ASCII-only by convention; multi-byte UTF-8 inside `body`
        // would break the byte-cursor scanner below (it copies bytes
        // verbatim and would emit mojibake). Lint correctness requires
        // either bailing out or running a proper char-iterator — bail
        // is simpler and matches what every real-world RPM macro looks
        // like in practice.
        if !body.is_ascii() {
            tracing::debug!("expand_body: non-ASCII input — bailing out");
            return None;
        }

        let bytes = body.as_bytes();
        let mut out = String::with_capacity(body.len());
        let mut i = 0;
        while i < bytes.len() {
            // Output amplification guard — checked on every loop
            // iteration so a body with hundreds of refs can't blow
            // up output even when each individual ref resolves to
            // something benign.
            if out.len() > MAX_EXPAND_OUTPUT {
                tracing::debug!(
                    out_len = out.len(),
                    cap = MAX_EXPAND_OUTPUT,
                    "expand_body: accumulated output exceeds cap"
                );
                return None;
            }
            if bytes[i] != b'%' {
                out.push(bytes[i] as char);
                i += 1;
                continue;
            }
            // Every `%` byte must scan to a well-formed token; a
            // malformed `%` (lone trailing, unterminated `%{`, `%`
            // followed by a non-ident char, …) is conservatively
            // treated as unresolvable.
            let r = scan_macro_ref(bytes, i)?;
            match r.kind {
                MacroKind::LiteralPercent => out.push('%'),
                MacroKind::Plain
                | MacroKind::Braced {
                    conditional: None,
                    has_default: false,
                } => {
                    let expanded = self.expand_to_literal(r.name, depth)?;
                    out.push_str(&expanded);
                }
                // Conditional refs, defaulted refs, shell/arithmetic:
                // depend on whether the macro is defined / on runtime
                // semantics — can't statically reduce.
                MacroKind::Braced { .. }
                | MacroKind::ShellExpansion
                | MacroKind::ArithmeticExpr => return None,
            }
            i = r.full_range.end;
        }
        Some(out)
    }
}

/// Hard cap on the length of an individual macro `Raw` body that
/// [`MacroRegistry::expand_to_literal`] will scan. Bodies exceeding
/// the cap cause expansion to return `None`. Protects against
/// malicious profiles crafting huge bodies that consume CPU per
/// recursive expansion call.
pub const MAX_EXPAND_BODY: usize = 4096;

/// Hard cap on the *accumulated* output of
/// [`MacroRegistry::expand_to_literal`]. Bodies with many references
/// (`%{a}%{a}…%{a}`) where each ref resolves to a non-trivial literal
/// can amplify a small input into a much larger output; this cap
/// stops the amplification long before it consumes meaningful memory.
pub const MAX_EXPAND_OUTPUT: usize = 64 * 1024;

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

impl MacroValue {
    /// Build a [`MacroValue`] from a user-supplied body (CLI `--define`
    /// or `[profiles.X.macros]` short form). A body containing `%` is
    /// treated as [`MacroValue::Raw`] (deferred expansion); otherwise
    /// it becomes [`MacroValue::Literal`]. `multiline` is always
    /// `false` for CLI-sourced bodies — argv can't carry newlines —
    /// and config callers that need it should construct `Raw` directly.
    ///
    /// Centralised here (rather than duplicated in `overrides.rs` and
    /// `config_layer.rs`) so the literal/raw heuristic stays in one
    /// place. A future tightening (e.g. recognise `%%` as a literal
    /// percent) only needs to touch this function.
    pub fn from_user_body(body: impl Into<String>) -> Self {
        let body = body.into();
        if body.contains('%') {
            MacroValue::Raw {
                body,
                multiline: false,
            }
        } else {
            MacroValue::Literal(body)
        }
    }
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
    /// Full set of target architectures the profile may ever produce
    /// across all builds (e.g. all primary arches of a distribution
    /// family). Distinct from [`Self::compatible_archs`], which is the
    /// host-compat list for *one* build. Populated by per-distro
    /// builtin / override layers; empty when unknown.
    ///
    /// Consumed by arch-domain lints (RPM440/RPM441/RPM453) to decide
    /// whether an `%ifarch <list>` covers the whole universe. Lints
    /// must treat an empty set as "unknown" and bail out conservatively.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub target_arch_universe: BTreeSet<String>,
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ValidationMode {
    #[default]
    Off,
    Warn,
    Strict,
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
    /// `--define NAME VALUE` from the CLI (or any future caller using
    /// [`crate::resolve::ResolveOptions::cli_defines`]). Distinct from
    /// [`LayerInfo::Override`] so `profile show` can label the layer
    /// "cli defines: NAMES" — the macros all share `Provenance::Override`
    /// but their *origin* is the command line, not a `.rpmspec.toml`
    /// entry the user can `grep` for.
    CliDefine {
        /// Names of the macros injected on this layer, in CLI order.
        /// Stored at layer granularity rather than per-macro because all
        /// defines from one resolve form a single conceptual "CLI batch".
        names: Vec<String>,
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
    fn from_user_body_plain_string_becomes_literal() {
        assert_eq!(
            MacroValue::from_user_body("/usr/lib64"),
            MacroValue::Literal("/usr/lib64".into())
        );
    }

    #[test]
    fn from_user_body_with_percent_ref_becomes_raw() {
        let v = MacroValue::from_user_body("%{_prefix}/lib");
        match v {
            MacroValue::Raw { body, multiline } => {
                assert_eq!(body, "%{_prefix}/lib");
                assert!(!multiline, "CLI/argv bodies are always single-line");
            }
            other => panic!("expected Raw, got {other:?}"),
        }
    }

    #[test]
    fn from_user_body_empty_string_becomes_literal() {
        // Empty body is technically valid (rpmbuild accepts `-D 'foo '`).
        // The linter rejects it at parse time, but the helper itself
        // doesn't enforce that — it stays a pure value-classifier.
        assert_eq!(
            MacroValue::from_user_body(""),
            MacroValue::Literal(String::new())
        );
    }

    #[test]
    fn from_user_body_percent_anywhere_triggers_raw() {
        // Any `%` — not just `%{` — flips to Raw, because rpm macros can
        // use the bare `%name` form. Cheaper than parsing for refs, and
        // pessimistic-correct: a body without refs that happens to
        // contain `%` (e.g. `100%`) just goes through the Raw renderer.
        let v = MacroValue::from_user_body("100% pure");
        assert!(matches!(v, MacroValue::Raw { .. }));
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

    // -- MacroRegistry::expand_to_literal --

    fn raw(body: &str) -> MacroEntry {
        let mut e = MacroEntry::literal("", Provenance::Override);
        e.value = MacroValue::Raw {
            body: body.to_string(),
            multiline: false,
        };
        e
    }

    #[test]
    fn expand_literal_returns_value_clone() {
        let mut reg = MacroRegistry::default();
        reg.insert("a", MacroEntry::literal(".el9", Provenance::Override));
        assert_eq!(reg.expand_to_literal("a", 8), Some(".el9".to_string()));
    }

    #[test]
    fn expand_undefined_macro_returns_none() {
        let reg = MacroRegistry::default();
        assert_eq!(reg.expand_to_literal("nope", 8), None);
    }

    #[test]
    fn expand_builtin_returns_none() {
        let mut reg = MacroRegistry::default();
        let mut entry = MacroEntry::literal("", Provenance::Override);
        entry.value = MacroValue::Builtin;
        reg.insert("P", entry);
        assert_eq!(reg.expand_to_literal("P", 8), None);
    }

    #[test]
    fn expand_raw_substitutes_brace_reference() {
        let mut reg = MacroRegistry::default();
        reg.insert("_prefix", MacroEntry::literal("/usr", Provenance::Override));
        reg.insert("_libdir", raw("%{_prefix}/lib64"));
        assert_eq!(
            reg.expand_to_literal("_libdir", 8),
            Some("/usr/lib64".to_string())
        );
    }

    #[test]
    fn expand_raw_substitutes_bare_name_form() {
        let mut reg = MacroRegistry::default();
        reg.insert("_prefix", MacroEntry::literal("/usr", Provenance::Override));
        reg.insert("p", raw("%_prefix/bin"));
        assert_eq!(reg.expand_to_literal("p", 8), Some("/usr/bin".to_string()));
    }

    #[test]
    fn expand_percent_percent_is_literal_percent() {
        let mut reg = MacroRegistry::default();
        reg.insert("p", raw("100%% pure"));
        assert_eq!(reg.expand_to_literal("p", 8), Some("100% pure".to_string()));
    }

    #[test]
    fn expand_rejects_conditional_question_mark() {
        let mut reg = MacroRegistry::default();
        reg.insert("p", raw("%{?foo}"));
        assert_eq!(reg.expand_to_literal("p", 8), None);
    }

    #[test]
    fn expand_rejects_bang_conditional() {
        let mut reg = MacroRegistry::default();
        reg.insert("p", raw("%{!?foo}"));
        assert_eq!(reg.expand_to_literal("p", 8), None);
    }

    #[test]
    fn expand_rejects_default_value_colon_form() {
        let mut reg = MacroRegistry::default();
        reg.insert("p", raw("%{foo:default}"));
        assert_eq!(reg.expand_to_literal("p", 8), None);
    }

    #[test]
    fn expand_rejects_unclosed_brace() {
        let mut reg = MacroRegistry::default();
        reg.insert("p", raw("%{foo"));
        assert_eq!(reg.expand_to_literal("p", 8), None);
    }

    #[test]
    fn expand_cycle_terminates_within_depth() {
        // a → %{b} → %{a} → ... should stop and return None,
        // not stack-overflow.
        let mut reg = MacroRegistry::default();
        reg.insert("a", raw("%{b}"));
        reg.insert("b", raw("%{a}"));
        assert_eq!(reg.expand_to_literal("a", 8), None);
    }

    #[test]
    fn expand_depth_zero_returns_none() {
        let mut reg = MacroRegistry::default();
        reg.insert("a", MacroEntry::literal("x", Provenance::Override));
        assert_eq!(reg.expand_to_literal("a", 0), None);
    }

    #[test]
    fn expand_body_at_max_size_succeeds() {
        // body of MAX_EXPAND_BODY bytes — literal `a` ×4096, no refs.
        let mut reg = MacroRegistry::default();
        reg.insert("p", raw(&"a".repeat(MAX_EXPAND_BODY)));
        let got = reg.expand_to_literal("p", 8);
        assert_eq!(got.as_ref().map(|s| s.len()), Some(MAX_EXPAND_BODY));
    }

    #[test]
    fn expand_body_over_max_size_fails() {
        // body of MAX_EXPAND_BODY + 1 bytes — must reject.
        let mut reg = MacroRegistry::default();
        reg.insert("p", raw(&"a".repeat(MAX_EXPAND_BODY + 1)));
        assert_eq!(reg.expand_to_literal("p", 8), None);
    }

    #[test]
    fn expand_output_amplification_capped() {
        // Body has many references that each resolve to a non-trivial
        // literal. Without the output cap, this would amplify to N×K
        // bytes and (in worse profiles) cascade exponentially.
        let mut reg = MacroRegistry::default();
        // `chunk` resolves to ~1KB.
        reg.insert(
            "chunk",
            MacroEntry::literal("x".repeat(1024), Provenance::Override),
        );
        // Body referencing `%{chunk}` 100 times → ~100KB output,
        // which exceeds MAX_EXPAND_OUTPUT (64KB).
        let body = "%{chunk}".repeat(100);
        reg.insert("amplifier", raw(&body));
        assert_eq!(
            reg.expand_to_literal("amplifier", 8),
            None,
            "output amplification must be capped at MAX_EXPAND_OUTPUT"
        );
    }

    #[test]
    fn expand_non_ascii_body_rejected() {
        // Multi-byte UTF-8 in a body — the byte-cursor scanner would
        // emit mojibake if we proceeded; bail out instead.
        let mut reg = MacroRegistry::default();
        reg.insert("p", raw("hellö"));
        assert_eq!(reg.expand_to_literal("p", 8), None);
    }
}
