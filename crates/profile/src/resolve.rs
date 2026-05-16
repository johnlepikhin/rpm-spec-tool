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
use crate::config_layer::{ProfileEntry, ProfileIdentityOverride, ProfileSection};
use crate::merge::{IdentityPatch, ProfilePatch};
use crate::showrc;
use crate::types::Profile;

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
}

/// Resolve the active profile.
///
/// `cli_override` is the value of `--profile <name>` if the user passed
/// one; it wins over the config's `profile` key. `base_dir` is the
/// directory of `.rpmspec.toml` — `showrc-file` paths are resolved
/// relative to it.
pub fn resolve(
    section: &ProfileSection,
    base_dir: &Path,
    cli_override: Option<&str>,
) -> Result<Profile, ResolveError> {
    let active = active_name(section, cli_override);
    let _span = tracing::info_span!(
        "resolve_profile",
        active,
        base_dir = %base_dir.display(),
        cli_override
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
        if cli_override.is_some() || section.profile.is_some() {
            return Err(ResolveError::UnknownProfile {
                name: active.to_string(),
            });
        }
        apply_builtin_layer(&mut profile, DEFAULT_BUILTIN, None)?;
    }

    // The human-readable name defaults to the active key unless an
    // earlier layer already filled it in.
    if profile.identity.name.is_empty() {
        profile.identity.name = active.to_string();
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
    Ok(())
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
        let p = resolve(&empty_section(), Path::new("."), None).unwrap();
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
        let p = resolve(&section, Path::new("."), Some("Y")).unwrap();
        assert_eq!(p.identity.name, "Y");
    }

    #[test]
    fn cli_override_unknown_errors() {
        let section = empty_section();
        let err = resolve(&section, Path::new("."), Some("does-not-exist")).unwrap_err();
        assert!(matches!(err, ResolveError::UnknownProfile { .. }));
    }

    #[test]
    fn showrc_layer_auto_detects_identity() {
        let mut section = empty_section();
        section.profile = Some("rhel-9".into());
        section.profiles.insert(
            "rhel-9".into(),
            ProfileEntry {
                showrc_file: Some(PathBuf::from("tests/fixtures/rhel7-showrc.txt")),
                ..Default::default()
            },
        );
        let base = Path::new(env!("CARGO_MANIFEST_DIR"));
        let p = resolve(&section, base, None).unwrap();
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
        let p = resolve(&section, base, None).unwrap();
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
        let err = resolve(&section, Path::new("."), None).unwrap_err();
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
        let err = resolve(&section, Path::new("."), None).unwrap_err();
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

        let p = resolve(&section, Path::new("."), None).unwrap();
        assert_eq!(p.identity.family, Some(Family::Alt));
        assert_eq!(p.identity.vendor.as_deref(), Some("acme"));
        // No showrc → macros stay empty (apart from anything the
        // builtin contributed, which is nothing for `generic`).
        assert!(p.macros.is_empty());
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
        let p = resolve(&section, base, None).unwrap();
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
}
