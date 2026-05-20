//! Top-level resolver: turns a [`ProfileSection`] into a fully merged
//! [`Profile`].
//!
//! Layer order (low → high precedence):
//! 1. Built-in baseline named by `extends` (or `"generic"` by default).
//!    Distribution builtins may also bundle a `rpm --showrc` dump
//!    (`data/<name>.showrc`); when present, it is applied immediately
//!    after the TOML metadata and feeds the identity auto-detector.
//! 2. `rpm --showrc` dump pointed at by `showrc-file`, if any. This
//!    layers on top of any bundled showrc, so user dumps win on
//!    macro collisions.
//! 3. Auto-detected identity from the showrc macros (only fields the
//!    user did not explicitly override).
//! 4. User overrides from `[profiles.X.*]`.
//!
//! Active-profile selection precedence (high → low):
//! 1. CLI override (`--profile <name>`).
//! 2. `profile = "<name>"` in the config.
//! 3. The built-in `generic`.

use std::path::{Path, PathBuf};

use crate::autodetect;
use crate::builtin::{self, BuiltinSnapshot, DEFAULT_BUILTIN};
use crate::config_layer::{ProfileEntry, ProfileIdentityOverride, ProfileSection, TargetEntry};
use crate::merge::{IdentityPatch, ProfilePatch};
use crate::overrides::{self, DefineParseError};
use crate::showrc;
use crate::types::{LayerInfo, Profile, ResolvedTarget, ResolvedTargetSet};

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("profile `{name}` is not defined in .rpmspec.toml and is not a built-in")]
    UnknownProfile { name: String },
    #[error("built-in profile `{name}` does not exist")]
    UnknownBuiltin { name: String },
    #[error(
        "profile `{profile_key}` extends unknown built-in `{extends}` — \
         available built-ins are listed by `rpm-spec-tool profile show`"
    )]
    UnknownExtendsTarget {
        profile_key: String,
        extends: String,
    },
    #[error("failed to read showrc file {path}: {source}")]
    ShowrcIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A `--define` argument failed to parse. Carries the offending
    /// argument and the precise parse failure. Distinct prefix in the
    /// `Display` impl so CLI users immediately see it's about argv,
    /// not a `.rpmspec.toml` or showrc problem.
    #[error("invalid --define argument: {0}")]
    BadDefine(#[from] DefineParseError),
    /// A target set's `profile-overrides` table referenced a profile
    /// name that is not in the target's `profiles` list. Almost
    /// always a typo — silently ignoring the override would mask a
    /// real misconfiguration.
    #[error(
        "target set `{target}` has a profile-overrides entry for `{profile}` \
         which is not in its `profiles` list"
    )]
    UnknownProfileInTarget { target: String, profile: String },
    /// A target set has an empty `profiles` list. The matrix consumer
    /// has nothing to run against — fail explicitly rather than
    /// produce an empty result the caller has to special-case.
    #[error("target set `{target}` has an empty `profiles` list")]
    EmptyTargetSet { target: String },
    /// A `[targets.X.defines]` or `[targets.X.profile-overrides.Y.defines]`
    /// entry has a value that can't safely round-trip through the
    /// `"NAME VALUE"` format the resolver uses internally. Currently
    /// triggers on newline / carriage-return — those silently parse as
    /// part of the value but produce a macro body that rpmbuild can
    /// never reproduce from a CLI flag.
    #[error("target set `{target}`: defines entry `{key}` has an invalid value ({reason})")]
    InvalidTargetDefine {
        target: String,
        key: String,
        reason: &'static str,
    },
    /// A `[profiles.X.repos.<id>]` key did not match the repo-id
    /// grammar (lowercase ascii, digits, `-`, `_`, ≤64 chars).
    /// Validated at resolve time rather than parse time so the
    /// diagnostic can name the owning profile.
    #[error("profile `{profile}` has invalid repo id `{id}`: {reason}")]
    InvalidRepoId {
        profile: String,
        id: String,
        reason: String,
    },
}

/// Caller-supplied knobs for [`resolve`]. Carries CLI-time inputs that
/// don't belong in [`ProfileSection`] (which models the on-disk
/// `.rpmspec.toml`).
///
/// Construction is **builder-only** from outside the crate: fields are
/// `pub(crate)` so a future extension (e.g. `--undefine`) can land
/// without forcing external callers to update struct literals. Use the
/// associated constructors:
///
/// * [`Self::default()`] — no CLI inputs.
/// * [`Self::with_override`] — set `--profile`.
/// * [`Self::with_defines`] — set raw `--define` args (chainable).
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct ResolveOptions<'a> {
    /// Value of `--profile <name>` from the command line; wins over the
    /// config's `profile = …` key. `None` means "use config (or
    /// `generic` fallback)".
    pub(crate) cli_override: Option<&'a str>,
    /// Raw `--define NAME VALUE` arguments in CLI order. Parsed by
    /// [`crate::overrides::parse_define`] and applied as the final
    /// layer — wins over the showrc dump and over
    /// `[profiles.X.macros]`. A parse failure aborts resolution
    /// entirely; a partially-applied profile never escapes.
    pub(crate) cli_defines: &'a [String],
}

impl<'a> ResolveOptions<'a> {
    /// Convenience constructor for the common "only `--profile`"
    /// shape. Lets call sites that don't care about defines avoid the
    /// struct literal: `ResolveOptions::with_override(Some("rhel-9-x86_64"))`.
    ///
    /// Returns a fresh `Self` rather than `&mut Self` so callers can
    /// chain [`Self::with_defines`] without naming the intermediate
    /// binding. `#[non_exhaustive]` on the struct makes a builder
    /// pattern necessary for external crates — `crates/cli` is one
    /// such crate.
    pub fn with_override(cli_override: Option<&'a str>) -> Self {
        Self {
            cli_override,
            ..Self::default()
        }
    }

    /// Chain a slice of raw `--define` arguments onto the options.
    /// The slice borrow lives for the same `'a` lifetime as the rest
    /// of the options so callers can keep their argv-owned vectors
    /// on the stack.
    pub fn with_defines(mut self, defines: &'a [String]) -> Self {
        self.cli_defines = defines;
        self
    }
}

/// Resolve the active profile.
///
/// `opts` carries CLI-time inputs: `--profile <name>` override and any
/// `--define NAME VALUE` arguments (see [`ResolveOptions`]). `base_dir`
/// is the directory of `.rpmspec.toml` — `showrc-file` paths are
/// resolved relative to it.
///
/// CLI defines are parsed up-front: if any raw argument is malformed
/// the whole resolve aborts with [`ResolveError::BadDefine`] before any
/// distribution layers are even loaded, so a partially-built profile
/// can't escape.
pub fn resolve(
    section: &ProfileSection,
    base_dir: &Path,
    opts: ResolveOptions<'_>,
) -> Result<Profile, ResolveError> {
    // Parse `--define` first — failing here doesn't waste a showrc
    // parse, and the error carries the offending raw arg verbatim.
    let cli_defines = overrides::parse_all(opts.cli_defines)?;

    let active = active_name(section, opts.cli_override);
    let _span = tracing::info_span!(
        "resolve_profile",
        active,
        base_dir = %base_dir.display(),
        cli_override = opts.cli_override,
        cli_defines = cli_defines.len(),
    )
    .entered();
    let mut profile = Profile::default();

    if let Some(entry) = section.profiles.get(active) {
        apply_entry(&mut profile, active, entry, base_dir)?;
    } else if builtin::load(active).is_some() {
        apply_builtin_layer(&mut profile, active, None)?;
    } else {
        // Only fail with UnknownProfile when the user actively asked for
        // something that doesn't exist. A *missing* `profile = …` key
        // resolves to `generic`, which is always present.
        if opts.cli_override.is_some() || section.profile.is_some() {
            return Err(ResolveError::UnknownProfile {
                name: active.to_string(),
            });
        }
        apply_builtin_layer(&mut profile, DEFAULT_BUILTIN, None)?;
    }

    // The human-readable name defaults to the active key unless an
    // earlier layer already filled it in. Done *before* CLI defines so
    // a packager can override `_vendor` (etc.) without touching the
    // profile name.
    if profile.identity.name.is_empty() {
        profile.identity.name = active.to_string();
    }

    // Layer 5 — CLI defines. Applied last so they outrank both the
    // showrc dump and `[profiles.X.macros]`.
    if !cli_defines.is_empty() {
        let names: Vec<String> = cli_defines.iter().map(|d| d.name.clone()).collect();
        let macros: Vec<(String, _)> = cli_defines.into_iter().map(|d| (d.name, d.entry)).collect();
        tracing::info!(
            count = macros.len(),
            "applying CLI --define layer (overrides showrc and TOML overrides)"
        );
        profile.apply(ProfilePatch {
            macros,
            layer: Some(LayerInfo::CliDefine { names }),
            ..Default::default()
        });
    }

    Ok(profile)
}

fn active_name<'a>(section: &'a ProfileSection, cli_override: Option<&'a str>) -> &'a str {
    cli_override
        .or(section.profile.as_deref())
        .unwrap_or(DEFAULT_BUILTIN)
}

/// Apply a built-in (TOML meta + optional bundled showrc) to `profile`.
///
/// `user_identity` lets the caller suppress autodetect fields that will
/// be overridden later — passed by `apply_entry()` so explicit
/// `[profiles.X.identity]` values don't get clobbered by inference.
/// When called for a bare CLI built-in (no `[profiles.X]` entry),
/// callers pass `None` and let autodetect fill all four fields.
fn apply_builtin_layer(
    profile: &mut Profile,
    name: &str,
    user_identity: Option<&ProfileIdentityOverride>,
) -> Result<(), ResolveError> {
    let snap: &BuiltinSnapshot =
        builtin::load(name).ok_or_else(|| ResolveError::UnknownBuiltin {
            name: name.to_string(),
        })?;
    // `Profile::apply` consumes its patch, so we must clone the static
    // snapshot. Both layers are cloned exactly once.
    profile.apply(snap.meta.clone());

    if let Some(patch) = snap.showrc.as_ref() {
        tracing::info!(
            builtin = name,
            macros = patch.macros.len(),
            rpmlib = patch.rpmlib.len(),
            arch = ?patch.arch.build_arch,
            "bundled showrc applied"
        );
        // Compute autodetect from the borrowed snapshot before moving
        // the patch into `profile.apply` — saves a second deep clone of
        // 400-700 macro entries per resolve.
        let detected = autodetect::detect(patch.macros.iter().map(|(n, e)| (n.as_str(), e)));
        tracing::debug!(
            family = ?detected.family,
            vendor = ?detected.vendor,
            dist_tag = ?detected.dist_tag,
            builtin = name,
            "autodetected identity from bundled showrc"
        );
        let masked = mask_against_user_opt(&detected, user_identity);
        profile.apply(patch.clone());
        if has_identity_changes(&masked) {
            profile.apply(ProfilePatch {
                identity: masked,
                ..Default::default()
            });
        }
    }

    // Seed `profile.repos` from the built-in's `[repos.*]` /
    // `[buildroot]` blocks (if any). User-side `[profiles.X.repos]`
    // / `[profiles.X.buildroot]` layered on top by `apply_repos`
    // below — see the merge semantics there.
    if let Some(repo_set) = snap.repos.as_ref() {
        tracing::debug!(
            builtin = name,
            repo_count = repo_set.repos.len(),
            base_packages = repo_set.buildroot.base_packages.len(),
            "bundled repos applied"
        );
        profile.repos = Some(repo_set.clone());
    }

    Ok(())
}

fn apply_entry(
    profile: &mut Profile,
    profile_key: &str,
    entry: &ProfileEntry,
    base_dir: &Path,
) -> Result<(), ResolveError> {
    // Layer 1 — built-in baseline (TOML + optional bundled showrc +
    // autodetect against the bundled macros).
    let base = entry.extends.as_deref().unwrap_or(DEFAULT_BUILTIN);
    apply_builtin_layer(profile, base, Some(&entry.identity)).map_err(|e| match e {
        // Rewrite the bare "unknown builtin" error with context the
        // user can act on — namely, which `[profiles.X]` entry's
        // `extends` key pointed at the missing built-in.
        ResolveError::UnknownBuiltin { name } => ResolveError::UnknownExtendsTarget {
            profile_key: profile_key.to_string(),
            extends: name,
        },
        other => other,
    })?;

    // Layer 2 — user-supplied showrc dump (if any). Layered on top of
    // any bundled showrc, so user dumps win on macro collisions.
    let mut showrc_macros_for_autodetect: Option<ProfilePatch> = None;
    if let Some(rel) = &entry.showrc_file {
        let abs = base_dir.join(rel);
        tracing::debug!(path = %abs.display(), "reading showrc dump");
        let text = std::fs::read_to_string(&abs).map_err(|source| ResolveError::ShowrcIo {
            path: abs.clone(),
            source,
        })?;
        let patch = showrc::parse(&text, Some(&abs));
        tracing::info!(
            path = %abs.display(),
            macros = patch.macros.len(),
            rpmlib = patch.rpmlib.len(),
            arch = ?patch.arch.build_arch,
            "user showrc parsed"
        );
        showrc_macros_for_autodetect = Some(patch.clone());
        profile.apply(patch);
    }

    // Layer 3 — autodetect identity from the user's showrc, only for
    // fields the user did NOT explicitly set in [profiles.X.identity].
    // This re-runs autodetect on top of whatever the built-in already
    // inferred; explicit user identity still wins.
    if let Some(showrc_patch) = showrc_macros_for_autodetect {
        let detected = autodetect::detect(showrc_patch.macros.iter().map(|(n, e)| (n.as_str(), e)));
        tracing::debug!(
            family = ?detected.family,
            vendor = ?detected.vendor,
            dist_tag = ?detected.dist_tag,
            "autodetected identity from user showrc"
        );
        let masked = mask_against_user(&detected, &entry.identity);
        if has_identity_changes(&masked) {
            // No layer recorded for autodetect — it's a refinement of
            // the showrc layer, not a fresh layer the user authored.
            profile.apply(ProfilePatch {
                identity: masked,
                ..Default::default()
            });
        }
    }

    // Layer 4 — explicit user overrides from [profiles.X.*].
    let override_patch = entry.override_patch(profile_key);
    profile.apply(override_patch);

    // Layer 5 — repositories and buildroot config. Repos slot into
    // their own `Option<RepoSet>` slot rather than going through the
    // patch / layer-trail mechanism because they're consumed by
    // entirely separate analyzers (the RepoSession / RPM-REPO-*
    // lints) and have atomic, not merged, replacement semantics
    // (an inherited repo with the same id is replaced wholesale).
    apply_repos(profile, profile_key, entry)?;

    Ok(())
}

/// Validate repo IDs declared in `[profiles.X.repos.<id>]`, then
/// overlay the user's `[profiles.X.repos]` / `[profiles.X.buildroot]`
/// onto whatever the built-in layer already installed (if any).
///
/// Merge semantics:
/// * Repos with the same `id` from `extends`-base built-in are
///   **replaced wholesale** by the user's entry — same policy the
///   plan document calls for (atomic merge, no per-field patching).
/// * `enabled = false` on a user-side entry that matches an
///   inherited id is the canonical way to mask a built-in repo.
/// * New ids the user introduces extend the set.
/// * `buildroot.base_packages` / `buildroot.implicit_buildrequires`
///   from user side **extend** the built-in lists (additive). If the
///   user wants to fully replace, they can either drop the entries
///   from the chain (by not declaring repos at all) or use future
///   `replace = true` syntax once added.
///
/// Empty user-side blocks + no built-in repos collapses to `None`
/// so downstream `repos.is_none()` keeps meaning "this profile has
/// no repo configuration of any flavour".
fn apply_repos(
    profile: &mut Profile,
    profile_key: &str,
    entry: &ProfileEntry,
) -> Result<(), ResolveError> {
    use crate::repos::{RepoSet, validate_repo_id};

    for id in entry.repos.keys() {
        validate_repo_id(id).map_err(|reason| ResolveError::InvalidRepoId {
            profile: profile_key.to_string(),
            id: id.clone(),
            reason,
        })?;
    }

    let user_empty = entry.repos.is_empty()
        && entry.buildroot.base_packages.is_empty()
        && entry.buildroot.implicit_buildrequires.is_empty();
    if user_empty {
        // Nothing to overlay — keep whatever the built-in seeded
        // (which may itself be `None`).
        return Ok(());
    }

    let mut merged = profile.repos.take().unwrap_or_else(|| RepoSet {
        repos: Default::default(),
        buildroot: Default::default(),
    });
    for (id, cfg) in &entry.repos {
        merged.repos.insert(id.clone(), cfg.clone());
    }
    // Extend then dedup so a user-side `gcc` doesn't appear twice
    // when the built-in baseline already lists it. Stable ordering
    // preserved (built-in packages first, user-added packages
    // appended) — meaningful for chroot install order and
    // `profile show` rendering. `dedup` would only kill *adjacent*
    // duplicates; we want a true set-merge.
    dedup_preserving_order(&mut merged.buildroot.base_packages);
    merged
        .buildroot
        .base_packages
        .extend(entry.buildroot.base_packages.iter().cloned());
    dedup_preserving_order(&mut merged.buildroot.base_packages);
    merged
        .buildroot
        .implicit_buildrequires
        .extend(entry.buildroot.implicit_buildrequires.iter().cloned());
    dedup_preserving_order(&mut merged.buildroot.implicit_buildrequires);
    profile.repos = Some(merged);
    Ok(())
}

/// Drop second-and-later occurrences of any repeated entry while
/// preserving the relative order of the first occurrences. The
/// alternative — `Vec::dedup` — only kills *adjacent* duplicates;
/// we want a true set-merge that keeps the "built-in first, user
/// added last" ordering visible in `profile show`.
fn dedup_preserving_order(v: &mut Vec<String>) {
    let mut seen = std::collections::BTreeSet::new();
    v.retain(|s| seen.insert(s.clone()));
}

/// Drop autodetected fields the user already specified — explicit
/// override has precedence over inference.
fn mask_against_user(detected: &IdentityPatch, user: &ProfileIdentityOverride) -> IdentityPatch {
    IdentityPatch {
        name: None, // never auto-set from showrc
        family: if user.family.is_some() {
            None
        } else {
            detected.family
        },
        vendor: if user.vendor.is_some() {
            None
        } else {
            detected.vendor.clone()
        },
        dist_tag: if user.dist_tag.is_some() {
            None
        } else {
            detected.dist_tag.clone()
        },
    }
}

/// `mask_against_user` for the bare-builtin path where there is no
/// `[profiles.X.identity]` to consult. Threads a fresh default
/// override (every field `None`) so autodetect contributes everything.
fn mask_against_user_opt(
    detected: &IdentityPatch,
    user: Option<&ProfileIdentityOverride>,
) -> IdentityPatch {
    let empty = ProfileIdentityOverride::default();
    mask_against_user(detected, user.unwrap_or(&empty))
}

fn has_identity_changes(p: &IdentityPatch) -> bool {
    p.family.is_some() || p.vendor.is_some() || p.dist_tag.is_some()
}

/// Validate one TOML-supplied define before it's stringified into the
/// resolver's `"NAME VALUE"` channel. Catches:
///
/// * keys that aren't valid rpm macro names (would silently round-trip
///   wrong: `"foo bar" = "1"` → parse_define sees name="foo" value="bar 1"),
/// * values containing newlines / carriage returns (silently absorbed
///   by parse_define but can't be reproduced from any CLI flag).
fn validate_target_define(target_id: &str, key: &str, value: &str) -> Result<(), ResolveError> {
    // Validate the key directly through the shared name validator —
    // a probe like `format!("{key} _")` + parse_define silently
    // accepts keys with embedded whitespace (`"foo bar"` → name="foo"),
    // missing the very class of misconfiguration this check is for.
    overrides::validate_name(key).map_err(ResolveError::BadDefine)?;
    if value.contains('\n') || value.contains('\r') {
        return Err(ResolveError::InvalidTargetDefine {
            target: target_id.to_string(),
            key: key.to_string(),
            reason: "value contains a newline / carriage return",
        });
    }
    Ok(())
}

/// Resolve a target set (a release matrix) into one fully-merged
/// [`Profile`] per member.
///
/// `target_id` is the `[targets.<id>]` TOML key (or any synthetic
/// label for ad-hoc target sets built from `--profiles a,b,c` on the
/// CLI). Validation order is fail-fast:
///
/// 1. The `profiles` list must not be empty.
/// 2. Every key in `profile_overrides` must appear in `profiles`
///    (catches typos).
/// 3. CLI `--define` args parse before any profile is resolved
///    (matches [`resolve`]'s contract).
/// 4. Each profile is resolved sequentially in declared order via
///    the existing single-profile [`resolve`]; the first failure
///    aborts the whole set so callers never see a partially-built
///    [`ResolvedTargetSet`].
///
/// Defines precedence inside one profile resolution:
///
/// ```text
/// extends → showrc → autodetect → [profiles.X.macros]
///   → target.defines (low)
///   → target.profile_overrides[X].defines (mid)
///   → opts.cli_defines (high — user's --define)
/// ```
///
/// The target-level and per-profile defines are stacked into the
/// same `cli_defines` slice as the user's `--define`, in this order,
/// so the resolver's existing last-wins semantics give the documented
/// precedence. They land on a single [`LayerInfo::CliDefine`] layer —
/// distinguishing them at the layer-trail level is a follow-up
/// (`doc/matrix.md` notes this).
///
/// Duplicate profile names in `target.profiles` are collapsed to
/// first occurrence so the result preserves the user's declared
/// column order without surprising the matrix renderer with
/// duplicated columns.
pub fn resolve_target_set(
    section: &ProfileSection,
    target_id: &str,
    target: &TargetEntry,
    base_dir: &Path,
    opts: ResolveOptions<'_>,
) -> Result<ResolvedTargetSet, ResolveError> {
    let _span = tracing::info_span!(
        "resolve_target_set",
        target = target_id,
        profiles = target.profiles.len(),
        defines = target.defines.len(),
        overrides = target.profile_overrides.len(),
    )
    .entered();
    if target.profiles.is_empty() {
        return Err(ResolveError::EmptyTargetSet {
            target: target_id.to_string(),
        });
    }
    // Validate per-profile overrides reference declared profiles —
    // catches `[targets.X.profile-overrides.rhel-8]` typos when
    // the actual list says `rhel-9`. Cheap up-front check.
    for override_key in target.profile_overrides.keys() {
        if !target.profiles.iter().any(|p| p == override_key) {
            return Err(ResolveError::UnknownProfileInTarget {
                target: target_id.to_string(),
                profile: override_key.clone(),
            });
        }
    }

    // Validate target-wide defines once up-front so a bad key/value
    // doesn't surface as a confusing inner BadDefine on the first
    // profile resolution. Reuses parse_define for name validation.
    for (k, v) in &target.defines {
        validate_target_define(target_id, k, v)?;
    }
    for (profile_key, po) in &target.profile_overrides {
        for (k, v) in &po.defines {
            validate_target_define(
                &format!("{target_id} / profile-overrides.{profile_key}"),
                k,
                v,
            )?;
        }
    }

    let mut seen = std::collections::BTreeSet::<&str>::new();
    let mut resolved = Vec::with_capacity(target.profiles.len());

    // Target-wide defines are identical for every member; stringify
    // them once instead of per-profile inside the loop.
    let mut target_define_strings: Vec<String> = Vec::with_capacity(target.defines.len());
    for (k, v) in &target.defines {
        target_define_strings.push(format!("{k} {v}"));
    }

    for profile_id in &target.profiles {
        // First occurrence wins; later duplicates are silently
        // collapsed. Logged for observability since duplicates in
        // hand-written TOML are usually a copy-paste mistake.
        if !seen.insert(profile_id.as_str()) {
            tracing::info!(
                target = target_id,
                profile = %profile_id,
                "duplicate profile in target set — collapsing to first occurrence"
            );
            continue;
        }

        // Stack defines: target-wide first, per-profile override
        // second, user --define last (highest precedence). All three
        // sit in one cli_defines slice — resolve()'s last-write-wins
        // does the right thing. Target-wide part is shared across
        // members and prepared above.
        let per_profile_override_count = target
            .profile_overrides
            .get(profile_id)
            .map_or(0, |po| po.defines.len());
        let mut combined_defines: Vec<String> = Vec::with_capacity(
            target_define_strings.len() + per_profile_override_count + opts.cli_defines.len(),
        );
        combined_defines.extend(target_define_strings.iter().cloned());
        if let Some(po) = target.profile_overrides.get(profile_id) {
            for (k, v) in &po.defines {
                combined_defines.push(format!("{k} {v}"));
            }
        }
        for d in opts.cli_defines {
            combined_defines.push(d.clone());
        }

        let per_profile_opts = ResolveOptions {
            cli_override: Some(profile_id.as_str()),
            cli_defines: &combined_defines,
        };
        let profile = resolve(section, base_dir, per_profile_opts)?;
        resolved.push(ResolvedTarget {
            profile_id: profile_id.clone(),
            profile,
        });
    }

    Ok(ResolvedTargetSet {
        id: target_id.to_string(),
        targets: resolved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Family, LayerInfo};
    use std::collections::BTreeMap;

    fn empty_section() -> ProfileSection {
        ProfileSection {
            profile: None,
            profiles: BTreeMap::new(),
        }
    }

    #[test]
    fn no_config_resolves_to_generic() {
        let p = resolve(&empty_section(), Path::new("."), ResolveOptions::default()).unwrap();
        // Builtin `generic` sets family = Generic explicitly.
        assert_eq!(p.identity.family, Some(Family::Generic));
        assert_eq!(p.identity.name, "generic");
        // One layer: builtin generic.
        assert_eq!(p.layers.len(), 1);
        assert!(matches!(
            p.layers[0],
            LayerInfo::Builtin { ref name } if name == "generic"
        ));
    }

    #[test]
    fn cli_override_wins_over_config_profile() {
        let mut section = empty_section();
        section.profile = Some("X".into());
        section.profiles.insert(
            "X".into(),
            ProfileEntry {
                ..Default::default()
            },
        );
        section.profiles.insert(
            "Y".into(),
            ProfileEntry {
                ..Default::default()
            },
        );
        let p = resolve(
            &section,
            Path::new("."),
            ResolveOptions::with_override(Some("Y")),
        )
        .unwrap();
        assert_eq!(p.identity.name, "Y");
    }

    #[test]
    fn cli_override_unknown_errors() {
        let section = empty_section();
        let err = resolve(
            &section,
            Path::new("."),
            ResolveOptions::with_override(Some("does-not-exist")),
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::UnknownProfile { .. }));
    }

    #[test]
    fn showrc_layer_auto_detects_identity() {
        let mut section = empty_section();
        section.profile = Some("rhel-9-x86_64".into());
        section.profiles.insert(
            "rhel-9-x86_64".into(),
            ProfileEntry {
                showrc_file: Some(PathBuf::from("tests/fixtures/rhel7-showrc.txt")),
                ..Default::default()
            },
        );
        let base = Path::new(env!("CARGO_MANIFEST_DIR"));
        let p = resolve(&section, base, ResolveOptions::default()).unwrap();
        // Fixture is rhel7 — detects rhel family, redhat vendor, .el7 tag.
        assert_eq!(p.identity.family, Some(Family::Rhel));
        assert_eq!(p.identity.vendor.as_deref(), Some("redhat"));
        assert_eq!(p.identity.dist_tag.as_deref(), Some(".el7"));
        // 733 macros from the fixture + identity refinement (no layer
        // recorded) + builtin baseline.
        assert!(p.macros.len() >= 700);
    }

    #[test]
    fn identity_override_wins_over_autodetect() {
        let mut section = empty_section();
        let mut entry = ProfileEntry {
            showrc_file: Some(PathBuf::from("tests/fixtures/rhel7-showrc.txt")),
            ..Default::default()
        };
        entry.identity.family = Some(Family::Fedora);
        entry.identity.vendor = Some("acme".into());
        section.profile = Some("X".into());
        section.profiles.insert("X".into(), entry);

        let base = Path::new(env!("CARGO_MANIFEST_DIR"));
        let p = resolve(&section, base, ResolveOptions::default()).unwrap();
        assert_eq!(p.identity.family, Some(Family::Fedora)); // user wins
        assert_eq!(p.identity.vendor.as_deref(), Some("acme"));
        // dist_tag wasn't overridden → still auto-detected.
        assert_eq!(p.identity.dist_tag.as_deref(), Some(".el7"));
    }

    #[test]
    fn missing_showrc_file_reports_path() {
        // ShowrcIo wraps a real std::io::Error and exposes the
        // attempted path so the CLI can surface a useful message.
        let mut section = empty_section();
        section.profile = Some("X".into());
        section.profiles.insert(
            "X".into(),
            ProfileEntry {
                showrc_file: Some(PathBuf::from("does-not-exist.txt")),
                ..Default::default()
            },
        );
        let err = resolve(&section, Path::new("."), ResolveOptions::default()).unwrap_err();
        match err {
            ResolveError::ShowrcIo { path, .. } => {
                assert!(path.ends_with("does-not-exist.txt"));
            }
            other => panic!("expected ShowrcIo, got {other:?}"),
        }
    }

    #[test]
    fn unknown_extends_errors() {
        let mut section = empty_section();
        section.profile = Some("X".into());
        section.profiles.insert(
            "X".into(),
            ProfileEntry {
                extends: Some("no-such-builtin".into()),
                ..Default::default()
            },
        );
        let err = resolve(&section, Path::new("."), ResolveOptions::default()).unwrap_err();
        // `apply_entry` wraps the bare `UnknownBuiltin` into a contextual
        // error that names both the offending entry and its `extends` target.
        match err {
            ResolveError::UnknownExtendsTarget {
                profile_key,
                extends,
            } => {
                assert_eq!(profile_key, "X");
                assert_eq!(extends, "no-such-builtin");
            }
            other => panic!("expected UnknownExtendsTarget, got {other:?}"),
        }
    }

    #[test]
    fn overrides_only_without_showrc() {
        // Pure override layer — no showrc, just inline identity and
        // macros. Identity comes entirely from the user.
        let mut section = empty_section();
        let mut entry = ProfileEntry::default();
        entry.identity.family = Some(Family::Alt);
        entry.identity.vendor = Some("acme".into());
        section.profile = Some("X".into());
        section.profiles.insert("X".into(), entry);

        let p = resolve(&section, Path::new("."), ResolveOptions::default()).unwrap();
        assert_eq!(p.identity.family, Some(Family::Alt));
        assert_eq!(p.identity.vendor.as_deref(), Some("acme"));
        // No showrc → macros stay empty (apart from anything the
        // builtin contributed, which is nothing for `generic`).
        assert!(p.macros.is_empty());
    }

    #[test]
    fn cli_defines_become_last_layer_with_cli_define_variant() {
        // Pure CLI-define case: one --define, no profile-side macros.
        // Asserts that (a) the macro lands in the registry with
        // `Provenance::Override`, (b) a `LayerInfo::CliDefine` is
        // recorded as the *final* layer (so `profile show` can label
        // it distinctly), and (c) the layer names match CLI order.
        let defines = vec!["with_python 1".to_string(), "_vendor acme".to_string()];
        let opts = ResolveOptions {
            cli_defines: &defines,
            ..Default::default()
        };
        let p = resolve(&empty_section(), Path::new("."), opts).unwrap();
        let py = p.macros.get("with_python").expect("with_python defined");
        assert_eq!(py.as_literal(), Some("1"));
        assert!(matches!(py.provenance, crate::types::Provenance::Override));
        let v = p.macros.get("_vendor").expect("_vendor defined");
        assert_eq!(v.as_literal(), Some("acme"));

        match p.layers.last().expect("at least one layer") {
            LayerInfo::CliDefine { names } => {
                assert_eq!(
                    names,
                    &vec!["with_python".to_string(), "_vendor".to_string()],
                    "names must mirror CLI order"
                );
            }
            other => panic!("expected CliDefine as last layer, got {other:?}"),
        }
    }

    #[test]
    fn cli_defines_win_over_config_macros() {
        // Same key set in `[profiles.X.macros]` and via `--define`.
        // CLI must win because it's applied as a later layer.
        let mut section = empty_section();
        let mut entry = ProfileEntry::default();
        entry.macros.insert(
            "_libdir".into(),
            crate::config_layer::MacroOverride::Literal("/usr/lib64".into()),
        );
        section.profile = Some("X".into());
        section.profiles.insert("X".into(), entry);

        let defines = vec!["_libdir /opt/local/lib".to_string()];
        let opts = ResolveOptions {
            cli_defines: &defines,
            ..Default::default()
        };
        let p = resolve(&section, Path::new("."), opts).unwrap();
        let v = p.macros.get("_libdir").unwrap();
        assert_eq!(v.as_literal(), Some("/opt/local/lib"));
    }

    #[test]
    fn cli_defines_override_showrc_dist() {
        // Real-world case: rhel7 fixture defines `dist = .el7`.
        // `--define 'dist .fc40'` must overwrite that value so
        // downstream lints (e.g. RPM127 Fedora-40 gate) see the
        // user-supplied tag.
        let mut section = empty_section();
        section.profile = Some("rhel-7".into());
        section.profiles.insert(
            "rhel-7".into(),
            ProfileEntry {
                showrc_file: Some(PathBuf::from("tests/fixtures/rhel7-showrc.txt")),
                ..Default::default()
            },
        );
        let base = Path::new(env!("CARGO_MANIFEST_DIR"));
        let defines = vec!["dist .fc40".to_string()];
        let opts = ResolveOptions {
            cli_defines: &defines,
            ..Default::default()
        };
        let p = resolve(&section, base, opts).unwrap();
        let dist = p.macros.get("dist").unwrap();
        assert_eq!(dist.as_literal(), Some(".fc40"));
        assert!(matches!(
            dist.provenance,
            crate::types::Provenance::Override
        ));
    }

    #[test]
    fn cli_define_parse_error_aborts_resolve() {
        // Malformed `--define` must surface as `BadDefine` *before*
        // any layer is applied — the returned error type is the
        // contract for the CLI to print a useful message.
        let defines = vec!["%bad value".to_string()];
        let opts = ResolveOptions {
            cli_defines: &defines,
            ..Default::default()
        };
        let err = resolve(&empty_section(), Path::new("."), opts).unwrap_err();
        assert!(matches!(err, ResolveError::BadDefine(_)));
    }

    #[test]
    fn empty_cli_defines_record_no_layer() {
        // Sanity: when the CLI passes an empty defines slice, no
        // `CliDefine` layer is appended (avoids a confusing empty
        // entry in `profile show` output).
        let opts = ResolveOptions::default();
        let p = resolve(&empty_section(), Path::new("."), opts).unwrap();
        for layer in &p.layers {
            assert!(
                !matches!(layer, LayerInfo::CliDefine { .. }),
                "no CliDefine layer when no defines passed; got {layer:?}"
            );
        }
    }

    #[test]
    fn macro_override_supersedes_showrc_with_override_provenance() {
        let mut section = empty_section();
        let mut entry = ProfileEntry {
            showrc_file: Some(PathBuf::from("tests/fixtures/rhel7-showrc.txt")),
            ..Default::default()
        };
        entry.macros.insert(
            "_vendor".into(),
            crate::config_layer::MacroOverride::Literal("acme".into()),
        );
        section.profile = Some("X".into());
        section.profiles.insert("X".into(), entry);

        let base = Path::new(env!("CARGO_MANIFEST_DIR"));
        let p = resolve(&section, base, ResolveOptions::default()).unwrap();
        let v = p.macros.get("_vendor").unwrap();
        assert_eq!(v.as_literal(), Some("acme"));
        assert!(matches!(v.provenance, crate::types::Provenance::Override));
        // Some other macro is still from showrc.
        let dist = p.macros.get("dist").unwrap();
        assert!(matches!(
            dist.provenance,
            crate::types::Provenance::Showrc { .. }
        ));
    }

    // -- resolve_target_set --

    fn target_with(profiles: &[&str]) -> TargetEntry {
        TargetEntry {
            profiles: profiles.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn target_set_resolves_each_profile_in_declared_order() {
        // Order matters for matrix renderers — columns line up with
        // the user's declared profile list.
        let section = empty_section();
        let target = target_with(&["generic", "rhel-9-x86_64"]);
        let set = resolve_target_set(
            &section,
            "smoke",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap();
        assert_eq!(set.id, "smoke");
        assert_eq!(set.targets.len(), 2);
        assert_eq!(set.targets[0].profile_id, "generic");
        assert_eq!(set.targets[1].profile_id, "rhel-9-x86_64");
        // Each entry's profile must have been built from the matching
        // built-in (proves resolve() was called with the right
        // cli_override). identity.name itself is a human label from
        // the built-in TOML, not the profile key.
        assert!(
            set.targets[0]
                .profile
                .layers
                .iter()
                .any(|l| matches!(l, LayerInfo::Builtin { name } if name.as_ref() == "generic"))
        );
        assert!(set.targets[1].profile.layers.iter().any(|l| {
            matches!(l, LayerInfo::Builtin { name } if name.as_ref() == "rhel-9-x86_64")
        }));
    }

    #[test]
    fn target_set_empty_profiles_is_rejected() {
        // Empty matrix is almost certainly a misconfiguration; the
        // matrix CLI would otherwise print a header and zero rows
        // with exit 0.
        let section = empty_section();
        let target = TargetEntry::default();
        let err = resolve_target_set(
            &section,
            "empty",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::EmptyTargetSet { ref target } if target == "empty"));
    }

    #[test]
    fn target_set_unknown_override_key_is_rejected() {
        // Per-profile override referencing a profile not in `profiles`
        // is treated as a typo (rather than silently no-op).
        let section = empty_section();
        let mut target = target_with(&["generic"]);
        target.profile_overrides.insert(
            "rhel-9-x86_64".into(),
            crate::config_layer::TargetProfileOverride::default(),
        );
        let err = resolve_target_set(
            &section,
            "T",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap_err();
        match err {
            ResolveError::UnknownProfileInTarget { target, profile } => {
                assert_eq!(target, "T");
                assert_eq!(profile, "rhel-9-x86_64");
            }
            other => panic!("expected UnknownProfileInTarget, got {other:?}"),
        }
    }

    #[test]
    fn target_set_defines_layer_on_every_profile() {
        // Target-level defines must reach every member profile,
        // through the existing CliDefine layer mechanism.
        let section = empty_section();
        let mut target = target_with(&["generic", "rhel-9-x86_64"]);
        target.defines.insert("product_build".into(), "1".into());

        let set = resolve_target_set(
            &section,
            "T",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap();
        for rt in &set.targets {
            let m = rt
                .profile
                .macros
                .get("product_build")
                .expect("define landed on profile");
            assert_eq!(m.as_literal(), Some("1"));
        }
    }

    #[test]
    fn target_set_per_profile_override_wins_over_target_defines() {
        // Same key set at target level and at per-profile level —
        // per-profile override must win for that one profile, target
        // value applies to the rest.
        let section = empty_section();
        let mut target = target_with(&["generic", "rhel-9-x86_64"]);
        target.defines.insert("use_jit".into(), "1".into());
        let mut e2k_override = crate::config_layer::TargetProfileOverride::default();
        e2k_override.defines.insert("use_jit".into(), "0".into());
        target
            .profile_overrides
            .insert("rhel-9-x86_64".into(), e2k_override);

        let set = resolve_target_set(
            &section,
            "T",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap();
        let generic = set
            .targets
            .iter()
            .find(|t| t.profile_id == "generic")
            .unwrap();
        let rhel = set
            .targets
            .iter()
            .find(|t| t.profile_id == "rhel-9-x86_64")
            .unwrap();
        assert_eq!(
            generic.profile.macros.get("use_jit").unwrap().as_literal(),
            Some("1")
        );
        assert_eq!(
            rhel.profile.macros.get("use_jit").unwrap().as_literal(),
            Some("0")
        );
    }

    #[test]
    fn target_set_cli_define_wins_over_target_defines() {
        // User-supplied --define beats both target-level and
        // per-profile-level defines — matches the documented
        // precedence chain.
        let section = empty_section();
        let mut target = target_with(&["generic"]);
        target
            .defines
            .insert("product_build".into(), "from-target".into());
        let cli_defines = vec!["product_build from-cli".to_string()];
        let opts = ResolveOptions {
            cli_defines: &cli_defines,
            ..Default::default()
        };
        let set = resolve_target_set(&section, "T", &target, Path::new("."), opts).unwrap();
        assert_eq!(
            set.targets[0]
                .profile
                .macros
                .get("product_build")
                .unwrap()
                .as_literal(),
            Some("from-cli")
        );
    }

    #[test]
    fn target_set_duplicate_profile_is_collapsed() {
        // `profiles = ["generic", "generic"]` resolves once.
        let section = empty_section();
        let target = target_with(&["generic", "generic", "rhel-9-x86_64"]);
        let set = resolve_target_set(
            &section,
            "T",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap();
        assert_eq!(set.targets.len(), 2);
        assert_eq!(set.targets[0].profile_id, "generic");
        assert_eq!(set.targets[1].profile_id, "rhel-9-x86_64");
    }

    #[test]
    fn validate_target_define_accepts_simple_pair() {
        assert!(validate_target_define("T", "_vendor", "acme").is_ok());
    }

    #[test]
    fn validate_target_define_rejects_key_with_whitespace() {
        // Bug fixture: a TOML quoted key like `"foo bar" = "1"` would
        // silently pass the old probe-based check (parse_define split
        // on whitespace gave name="foo"). validate_name is strict.
        let err = validate_target_define("T", "foo bar", "1").unwrap_err();
        assert!(matches!(err, ResolveError::BadDefine(_)));
    }

    #[test]
    fn validate_target_define_rejects_digit_leading_key() {
        let err = validate_target_define("T", "9foo", "1").unwrap_err();
        assert!(matches!(err, ResolveError::BadDefine(_)));
    }

    #[test]
    fn validate_target_define_rejects_value_with_newline() {
        let err = validate_target_define("T", "foo", "line1\nline2").unwrap_err();
        match err {
            ResolveError::InvalidTargetDefine { target, key, .. } => {
                assert_eq!(target, "T");
                assert_eq!(key, "foo");
            }
            other => panic!("expected InvalidTargetDefine, got {other:?}"),
        }
    }

    #[test]
    fn validate_target_define_rejects_value_with_cr() {
        let err = validate_target_define("T", "foo", "a\rb").unwrap_err();
        assert!(matches!(err, ResolveError::InvalidTargetDefine { .. }));
    }

    #[test]
    fn target_set_resolves_when_define_is_valid() {
        // End-to-end: well-formed target-wide define still lands on
        // the resolved profile — the new validator must not block
        // the happy path.
        let section = empty_section();
        let mut target = target_with(&["generic"]);
        target.defines.insert("_vendor".into(), "acme".into());
        let set = resolve_target_set(
            &section,
            "T",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap();
        assert_eq!(
            set.targets[0]
                .profile
                .macros
                .get("_vendor")
                .unwrap()
                .as_literal(),
            Some("acme")
        );
    }

    #[test]
    fn target_set_rejects_bad_target_define_at_resolution() {
        // The validator is invoked through resolve_target_set, so a
        // bad key surfaces as a resolve error before any profile is
        // loaded.
        let section = empty_section();
        let mut target = target_with(&["generic"]);
        target.defines.insert("foo bar".into(), "1".into());
        let err = resolve_target_set(
            &section,
            "T",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::BadDefine(_)));
    }

    #[test]
    fn target_set_rejects_bad_profile_override_define() {
        // Same validator runs against per-profile overrides. The
        // target_id passed to InvalidTargetDefine carries the
        // "T / profile-overrides.X" prefix so the user can see
        // which block was rejected.
        let section = empty_section();
        let mut target = target_with(&["generic"]);
        let mut po = crate::config_layer::TargetProfileOverride::default();
        po.defines.insert("foo".into(), "first\nsecond".into());
        target.profile_overrides.insert("generic".into(), po);
        let err = resolve_target_set(
            &section,
            "T",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap_err();
        match err {
            ResolveError::InvalidTargetDefine { target, key, .. } => {
                assert!(target.contains("profile-overrides.generic"));
                assert_eq!(key, "foo");
            }
            other => panic!("expected InvalidTargetDefine, got {other:?}"),
        }
    }

    #[test]
    fn target_set_unknown_profile_propagates_resolve_error() {
        // Listing a non-existent profile in `profiles` surfaces
        // through the inner resolve() — no special-cased error.
        let section = empty_section();
        let target = target_with(&["does-not-exist"]);
        let err = resolve_target_set(
            &section,
            "T",
            &target,
            Path::new("."),
            ResolveOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ResolveError::UnknownProfile { .. }));
    }

    #[test]
    fn builtin_repos_seed_profile_without_user_config() {
        // Resolving a profile whose CLI override picks an ALT
        // built-in (no user `[profiles.X]` entry) must still expose
        // the bundled `classic` repo. Regression test for the
        // built-in-repos seeding path in `apply_builtin_layer`.
        let mut section = empty_section();
        section.profile = Some("altlinux-11-x86_64".into());
        let p = resolve(&section, Path::new("."), ResolveOptions::default()).unwrap();
        let repos = p.repos.expect("ALT 11 must ship repos");
        assert!(
            repos.repos.contains_key("classic"),
            "ALT 11 must expose classic out of the box, got keys: {:?}",
            repos.repos.keys().collect::<Vec<_>>(),
        );
        assert!(repos.repos["classic"].enabled);
        assert!(!repos.buildroot.base_packages.is_empty());
    }

    #[test]
    fn user_repos_overlay_replaces_inherited_repo_by_id() {
        // A user `[profiles.my-alt.repos.classic]` block must replace
        // the inherited built-in `classic` repo wholesale — this is
        // the canonical way to point a profile at an internal mirror
        // while keeping the rest of the chain intact.
        let mut section = empty_section();
        section.profile = Some("my-alt".into());
        let mut user_repos = BTreeMap::new();
        let mut overridden = crate::repos::RepoConfig::default();
        overridden.baseurl = Some("http://internal-mirror.example/p11/$basearch/".into());
        overridden.kind = crate::repos::RepoKind::AptRpm;
        overridden.priority = 50;
        user_repos.insert("classic".into(), overridden);
        section.profiles.insert(
            "my-alt".into(),
            ProfileEntry {
                extends: Some("altlinux-11-x86_64".into()),
                repos: user_repos,
                ..Default::default()
            },
        );
        let p = resolve(&section, Path::new("."), ResolveOptions::default()).unwrap();
        let repos = p.repos.expect("repos populated");
        let classic = &repos.repos["classic"];
        // URL came from user — built-in URL was discarded.
        assert_eq!(
            classic.baseurl.as_deref(),
            Some("http://internal-mirror.example/p11/$basearch/"),
        );
        // Priority also picked up the user-side value.
        assert_eq!(classic.priority, 50);
    }

    #[test]
    fn user_disabled_masks_inherited_repo() {
        // Setting `enabled = false` on a user-side repo with the same
        // id as an inherited built-in is the documented way to mask
        // the built-in without redefining the URL. The merged set
        // keeps the repo (so `repo cache` / `repo show` can still
        // describe it) but downstream consumers must respect
        // `enabled = false`.
        let mut section = empty_section();
        section.profile = Some("my-alt".into());
        let mut user_repos = BTreeMap::new();
        // Keep baseurl unset so the mask intent is unambiguous.
        let mut masked = crate::repos::RepoConfig::default();
        masked.enabled = false;
        user_repos.insert("classic".into(), masked);
        section.profiles.insert(
            "my-alt".into(),
            ProfileEntry {
                extends: Some("altlinux-11-x86_64".into()),
                repos: user_repos,
                ..Default::default()
            },
        );
        let p = resolve(&section, Path::new("."), ResolveOptions::default()).unwrap();
        let classic = &p.repos.unwrap().repos["classic"];
        assert!(!classic.enabled, "user disable must mask built-in");
    }

    #[test]
    fn user_buildroot_dedup_against_inherited_baseline() {
        // If the user lists `gcc` (which the ALT built-in baseline
        // already provides) the merged list must contain it exactly
        // once. The merge is order-preserving (built-in first), so
        // `gcc` keeps its built-in position.
        let mut section = empty_section();
        section.profile = Some("my-alt".into());
        let mut buildroot = crate::repos::BuildrootConfig::default();
        buildroot.base_packages = vec!["gcc".into(), "acme-toolchain".into()];
        section.profiles.insert(
            "my-alt".into(),
            ProfileEntry {
                extends: Some("altlinux-11-x86_64".into()),
                buildroot,
                ..Default::default()
            },
        );
        let p = resolve(&section, Path::new("."), ResolveOptions::default()).unwrap();
        let pkgs = &p.repos.unwrap().buildroot.base_packages;
        let gcc_count = pkgs.iter().filter(|s| *s == "gcc").count();
        assert_eq!(
            gcc_count, 1,
            "duplicate gcc after merge: {pkgs:?}",
        );
        assert!(pkgs.iter().any(|s| s == "acme-toolchain"));
    }

    #[test]
    fn user_buildroot_extends_inherited_base_packages() {
        // User `[profiles.X.buildroot.base-packages]` extends the
        // built-in baseline (additive), so a profile can add an
        // internal `acme-toolchain` package on top of `rpm-build`
        // etc without re-listing every default.
        let mut section = empty_section();
        section.profile = Some("my-alt".into());
        let mut buildroot = crate::repos::BuildrootConfig::default();
        buildroot.base_packages = vec!["acme-toolchain".into()];
        section.profiles.insert(
            "my-alt".into(),
            ProfileEntry {
                extends: Some("altlinux-11-x86_64".into()),
                buildroot,
                ..Default::default()
            },
        );
        let p = resolve(&section, Path::new("."), ResolveOptions::default()).unwrap();
        let pkgs = &p.repos.unwrap().buildroot.base_packages;
        assert!(
            pkgs.iter().any(|s| s == "rpm-build"),
            "built-in `rpm-build` must survive the merge: {pkgs:?}",
        );
        assert!(
            pkgs.iter().any(|s| s == "acme-toolchain"),
            "user-added package must be present: {pkgs:?}",
        );
    }
}
