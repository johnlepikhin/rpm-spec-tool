//! Layered merge for [`Profile`] data.
//!
//! Layers are applied in low → high precedence order. The first layer
//! seeds the profile; subsequent layers overlay fields. Macro conflicts
//! resolve to last-writer-wins; whitelist conflicts honour an optional
//! `replace` flag (default: union).

use crate::types::*;

/// Patch applied on top of an existing [`Profile`]. Each `Option<…>`
/// represents "leave the current value unchanged when `None`".
///
/// Used both by the showrc layer and by user overrides — the difference
/// is only in what's filled in.
#[derive(Debug, Clone, Default)]
pub struct ProfilePatch {
    pub identity: IdentityPatch,
    pub macros: Vec<(String, MacroEntry)>,
    pub rpmlib: Vec<(String, String)>,
    pub arch: ArchPatch,
    pub licenses: Option<ListPatch>,
    pub groups: Option<ListPatch>,
    pub layer: Option<LayerInfo>,
}

#[derive(Debug, Clone, Default)]
pub struct IdentityPatch {
    pub name: Option<String>,
    pub family: Option<Family>,
    pub vendor: Option<String>,
    pub dist_tag: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ArchPatch {
    pub build_arch: Option<String>,
    pub build_os: Option<String>,
    pub compatible_archs: Option<Vec<String>>,
    pub optflags_template: Option<String>,
    /// When `Some`, replaces the profile's
    /// [`ArchInfo::target_arch_universe`] set wholesale (no merge).
    pub target_arch_universe: Option<std::collections::BTreeSet<String>>,
}

#[derive(Debug, Clone)]
pub struct ListPatch {
    pub mode: Option<ValidationMode>,
    pub allow: Vec<String>,
    pub replace: bool,
}

impl Profile {
    /// Return the profile's target arch universe (set of all
    /// architectures this profile may ever produce across all builds)
    /// when populated, else `None`.
    ///
    /// Consumed by arch-domain lints (RPM440/RPM441/RPM453). Treat
    /// `None` as "unknown" — never assume an arch list is exhaustive
    /// without an explicit profile-side declaration.
    pub fn arch_universe(&self) -> Option<&std::collections::BTreeSet<String>> {
        if self.arch.target_arch_universe.is_empty() {
            None
        } else {
            Some(&self.arch.target_arch_universe)
        }
    }

    /// Apply `patch` in-place. Returns `self` for chaining.
    pub fn apply(&mut self, patch: ProfilePatch) -> &mut Self {
        let ProfilePatch {
            identity,
            macros,
            rpmlib,
            arch,
            licenses,
            groups,
            layer,
        } = patch;

        if let Some(name) = identity.name {
            self.identity.name = name;
        }
        if let Some(family) = identity.family {
            self.identity.family = Some(family);
        }
        if let Some(v) = identity.vendor {
            self.identity.vendor = Some(v);
        }
        if let Some(d) = identity.dist_tag {
            self.identity.dist_tag = Some(d);
        }

        for (k, v) in macros {
            self.macros.insert(k, v);
        }

        for (k, v) in rpmlib {
            self.rpmlib.features.insert(k, v);
        }

        if let Some(v) = arch.build_arch {
            self.arch.build_arch = Some(v);
        }
        if let Some(v) = arch.build_os {
            self.arch.build_os = Some(v);
        }
        if let Some(v) = arch.compatible_archs {
            self.arch.compatible_archs = v;
        }
        if let Some(v) = arch.optflags_template {
            self.arch.optflags_template = Some(v);
        }
        if let Some(v) = arch.target_arch_universe {
            self.arch.target_arch_universe = v;
        }

        if let Some(lp) = licenses {
            apply_list_patch(&mut self.licenses.allowed, &mut self.licenses.mode, lp);
        }
        if let Some(gp) = groups {
            apply_list_patch(&mut self.groups.allowed, &mut self.groups.mode, gp);
        }

        if let Some(l) = layer {
            self.layers.push(l);
        }
        self
    }
}

fn apply_list_patch(
    allowed: &mut std::collections::BTreeSet<String>,
    mode: &mut ValidationMode,
    patch: ListPatch,
) {
    if patch.replace {
        allowed.clear();
    }
    allowed.extend(patch.allow);
    if let Some(m) = patch.mode {
        *mode = m;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_patch_is_noop() {
        let mut p = Profile::default();
        p.apply(ProfilePatch::default());
        assert!(p.macros.is_empty());
        // Default identity: no detection happened and the user did not
        // pick a family — distinct from `Some(Family::Generic)`.
        assert!(p.identity.family.is_none());
        assert!(p.layers.is_empty());
    }

    #[test]
    fn arch_universe_is_none_until_populated() {
        let p = Profile::default();
        assert!(p.arch_universe().is_none());
    }

    #[test]
    fn arch_universe_returns_populated_set() {
        use std::collections::BTreeSet;
        let mut universe = BTreeSet::new();
        universe.insert("x86_64".to_string());
        universe.insert("aarch64".to_string());
        let mut p = Profile::default();
        p.apply(ProfilePatch {
            arch: ArchPatch {
                target_arch_universe: Some(universe.clone()),
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(p.arch_universe(), Some(&universe));
    }

    #[test]
    fn macros_last_writer_wins() {
        let mut p = Profile::default();
        p.apply(ProfilePatch {
            macros: vec![(
                "_vendor".into(),
                MacroEntry::literal(
                    "redhat",
                    Provenance::Showrc {
                        level: -13,
                        path: None,
                    },
                ),
            )],
            ..Default::default()
        });
        p.apply(ProfilePatch {
            macros: vec![(
                "_vendor".into(),
                MacroEntry::literal("acme", Provenance::Override),
            )],
            ..Default::default()
        });
        let e = p.macros.get("_vendor").unwrap();
        assert_eq!(e.as_literal(), Some("acme"));
        assert!(matches!(e.provenance, Provenance::Override));
    }

    #[test]
    fn license_list_union_by_default() {
        let mut p = Profile::default();
        p.apply(ProfilePatch {
            licenses: Some(ListPatch {
                mode: Some(ValidationMode::Strict),
                allow: vec!["GPL-2.0-or-later".into()],
                replace: false,
            }),
            ..Default::default()
        });
        p.apply(ProfilePatch {
            licenses: Some(ListPatch {
                mode: None,
                allow: vec!["MIT".into()],
                replace: false,
            }),
            ..Default::default()
        });
        assert_eq!(p.licenses.allowed.len(), 2);
        assert!(p.licenses.is_allowed("MIT"));
        assert!(p.licenses.is_allowed("GPL-2.0-or-later"));
        assert_eq!(p.licenses.mode, ValidationMode::Strict);
    }

    #[test]
    fn license_replace_clears_previous() {
        let mut p = Profile::default();
        p.apply(ProfilePatch {
            licenses: Some(ListPatch {
                mode: Some(ValidationMode::Warn),
                allow: vec!["GPL-2.0-or-later".into(), "MIT".into()],
                replace: false,
            }),
            ..Default::default()
        });
        p.apply(ProfilePatch {
            licenses: Some(ListPatch {
                mode: None,
                allow: vec!["Proprietary".into()],
                replace: true,
            }),
            ..Default::default()
        });
        assert_eq!(p.licenses.allowed.len(), 1);
        assert!(p.licenses.is_allowed("Proprietary"));
        // mode is sticky — second patch didn't touch it
        assert_eq!(p.licenses.mode, ValidationMode::Warn);
    }

    #[test]
    fn identity_partial_override() {
        let mut p = Profile::default();
        p.apply(ProfilePatch {
            identity: IdentityPatch {
                family: Some(Family::Rhel),
                vendor: Some("redhat".into()),
                dist_tag: Some(".el9".into()),
                name: None,
            },
            ..Default::default()
        });
        p.apply(ProfilePatch {
            identity: IdentityPatch {
                vendor: Some("acme".into()),
                ..Default::default()
            },
            ..Default::default()
        });
        assert_eq!(p.identity.family, Some(Family::Rhel));
        assert_eq!(p.identity.vendor.as_deref(), Some("acme"));
        assert_eq!(p.identity.dist_tag.as_deref(), Some(".el9"));
    }

    #[test]
    fn layer_trail_recorded() {
        let mut p = Profile::default();
        p.apply(ProfilePatch {
            layer: Some(LayerInfo::Builtin {
                name: "generic".into(),
            }),
            ..Default::default()
        });
        p.apply(ProfilePatch {
            layer: Some(LayerInfo::Override {
                fields: vec!["macros._vendor".into()],
            }),
            ..Default::default()
        });
        assert_eq!(p.layers.len(), 2);
    }
}
