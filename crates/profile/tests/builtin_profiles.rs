//! Smoke tests for every built-in profile shipped under `data/`.
//!
//! Catches regressions where a newly-added profile breaks the showrc
//! parser, the loader, or the resolver. Each test asserts only invariants
//! that hold for *every* distribution dump (arch is set, at least 100
//! macros parsed, identity ends up non-empty after resolution) so adding
//! a profile doesn't require touching this file.

use std::path::{Path, PathBuf};

use rpm_spec_profile::{Family, LayerInfo, ProfileEntry, ProfileSection, builtin, resolve_profile};

#[test]
fn every_builtin_loads_cleanly() {
    let names = builtin::names();
    assert!(!names.is_empty(), "registry is empty");
    assert!(names.contains(&"generic"), "generic must always ship");

    for name in names {
        let snap = builtin::load(name).unwrap_or_else(|| panic!("load({name}) returned None"));

        if let Some(showrc) = &snap.showrc {
            // Distribution profiles must carry a non-trivial dump.
            assert!(
                showrc.macros.len() >= 100,
                "{name}: only {} macros — likely truncated dump",
                showrc.macros.len()
            );
            assert!(
                showrc.arch.build_arch.is_some(),
                "{name}: no build_arch parsed from showrc header"
            );
            // Layer is the bundled flavour, not a file path.
            match showrc.layer.as_ref() {
                Some(LayerInfo::BuiltinShowrc { name: n, macros }) => {
                    assert_eq!(n, name);
                    assert_eq!(*macros, showrc.macros.len());
                }
                other => panic!("{name}: expected BuiltinShowrc layer, got {other:?}"),
            }
        }
    }
}

#[test]
fn every_builtin_resolves_via_cli_override() {
    let empty = ProfileSection::default();
    let base = Path::new(".");

    for &name in builtin::names() {
        let resolved = resolve_profile(&empty, base, Some(name))
            .unwrap_or_else(|e| panic!("resolve({name}) failed: {e:?}"));

        // The resolver fills `identity.name` with the active key when no
        // explicit `name` is supplied — every profile must at least have
        // a non-empty label.
        assert!(
            !resolved.identity.name.is_empty(),
            "{name}: empty identity.name"
        );

        if name == "generic" {
            continue;
        }

        // Distribution profiles must have an arch (from bundled showrc).
        assert!(
            resolved.arch.build_arch.is_some(),
            "{name}: resolved profile has no build_arch"
        );

        // Layer trail: builtin TOML + builtin showrc. Autodetected
        // identity refinement is *not* a recorded layer.
        assert!(
            resolved.layers.len() >= 2,
            "{name}: expected ≥2 layers, got {}",
            resolved.layers.len()
        );
        match &resolved.layers[0] {
            LayerInfo::Builtin { name: layer_name } => assert_eq!(layer_name.as_ref(), name),
            other => panic!("{name}: layer[0] not Builtin: {other:?}"),
        }
        match &resolved.layers[1] {
            LayerInfo::BuiltinShowrc {
                name: layer_name, ..
            } => assert_eq!(layer_name.as_ref(), name),
            other => panic!("{name}: layer[1] not BuiltinShowrc: {other:?}"),
        }
    }
}

/// Document-and-lock-in the recommended user flow: a `[profiles.X]`
/// entry that `extends` a distribution builtin and pins a single
/// identity field. Bundled showrc must seed macros + autodetect family;
/// the user pin must override the autodetected vendor; layer trail must
/// include the override layer at the end.
#[test]
fn user_entry_extends_distribution_builtin_with_identity_pin() {
    let mut section = ProfileSection::default();
    let mut entry = ProfileEntry::default();
    entry.extends = Some("rhel-9-x86_64".into());
    entry.identity.vendor = Some("acme".into()); // user override
    section.profile = Some("acmebuild".into());
    section.profiles.insert("acmebuild".into(), entry);

    let p = resolve_profile(&section, Path::new("."), None).unwrap();

    // Bundled showrc was applied.
    assert!(p.macros.len() > 100);
    assert_eq!(p.arch.build_arch.as_deref(), Some("x86_64"));

    // Identity: family from bundled autodetect (rhel marker macro),
    // vendor from user pin (autodetect would say "redhat" — user wins),
    // dist-tag from the TOML stub (`data/rhel-9-x86_64.toml` pins `.el9`
    // because the showrc `dist` macro is a lua expression).
    assert_eq!(p.identity.family, Some(Family::Rhel));
    assert_eq!(p.identity.vendor.as_deref(), Some("acme"));
    assert_eq!(p.identity.dist_tag.as_deref(), Some(".el9"));

    // Layer trail: Builtin → BuiltinShowrc → Override. Autodetect
    // refinements are intentionally not layers.
    assert!(p.layers.len() >= 3);
    match &p.layers[0] {
        LayerInfo::Builtin { name } => assert_eq!(name.as_ref(), "rhel-9-x86_64"),
        other => panic!("layer[0] = {other:?}"),
    }
    match &p.layers[1] {
        LayerInfo::BuiltinShowrc { name, .. } => assert_eq!(name.as_ref(), "rhel-9-x86_64"),
        other => panic!("layer[1] = {other:?}"),
    }
    match p.layers.last().unwrap() {
        LayerInfo::Override { fields } => {
            assert!(fields.iter().any(|f| f == "identity.vendor"));
        }
        other => panic!("last layer = {other:?}"),
    }
}

/// User-supplied `showrc-file` layered on top of a bundled builtin
/// showrc — user-side macros must win on key collisions (last writer),
/// macros only in the bundled dump remain visible.
#[test]
fn user_showrc_layered_over_bundled_showrc_wins_on_collisions() {
    // Write a tiny user-side showrc fixture that redefines `_vendor`
    // (which the bundled rhel-9 dump sets to "redhat"). We don't care
    // about the rest of the dump; the parser ignores unrecognised lines.
    let tmp = tempdir();
    let user_dump = tmp.join("user.showrc");
    std::fs::write(
        &user_dump,
        "========================\n\
         -13: _vendor\tacme\n\
         -13: only_in_user\thello\n\
         ========================\n",
    )
    .unwrap();

    let mut section = ProfileSection::default();
    let mut entry = ProfileEntry::default();
    entry.extends = Some("rhel-9-x86_64".into());
    entry.showrc_file = Some(PathBuf::from(user_dump.file_name().unwrap()));
    section.profile = Some("layered".into());
    section.profiles.insert("layered".into(), entry);

    let p = resolve_profile(&section, &tmp, None).unwrap();

    // Collision: user-layer wins. The bundled showrc set `_vendor=redhat`.
    let vendor = p.macros.get("_vendor").expect("_vendor present");
    assert_eq!(vendor.as_literal(), Some("acme"));

    // Macro only in user layer is visible.
    assert!(p.macros.get("only_in_user").is_some());

    // Bundled-only macro stays visible (e.g. `rhel` marker macro).
    assert!(p.macros.get("rhel").is_some(), "bundled `rhel` macro lost");

    // Layer trail: Builtin → BuiltinShowrc → Showrc (user). Override
    // layer is absent because the entry has no other [profiles.X.*] fields.
    assert!(p.layers.len() >= 3);
    assert!(matches!(p.layers[2], LayerInfo::Showrc { .. }));
}

// Simple per-test scratch dir; avoids pulling in the `tempfile` crate
// for what is structurally a one-file fixture.
fn tempdir() -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "rpm-spec-profile-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}
