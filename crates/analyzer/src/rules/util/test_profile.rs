//! Test-only `Profile` builder. Cfg-gated to `#[cfg(test)]` because no
//! production rule needs to mint a synthetic `Profile` — only the
//! profile-aware rule tests do.

/// Build a synthetic `Profile` for unit-testing profile-aware rules.
/// Convenience over `Profile::default()` mutation for the common
/// "family + a few macros + a few rpmlib features" case.
///
/// `dist_tag` is optional because most rules gate on family alone;
/// composite gates (RPM127's "Fedora ≥ 40") pass `Some(".fc40")` etc.
// NOTE: kept as `#[allow]` (not `#[expect]`) because at least one test
// module currently exercises this helper, so `dead_code` does not fire
// under `--all-targets` and `#[expect]` would emit
// `unfulfilled_lint_expectations`. Whether every rule module uses it can
// flip with each new rule, so the wildcard suppression stays.
#[allow(dead_code)]
pub(crate) fn make_test_profile(
    family: Option<rpm_spec_profile::Family>,
    dist_tag: Option<&str>,
    macros: &[(&str, &str)],
    rpmlib: &[(&str, &str)],
) -> rpm_spec_profile::Profile {
    use rpm_spec_profile::{MacroEntry, Profile, Provenance};
    let mut p = Profile::default();
    p.identity.family = family;
    p.identity.dist_tag = dist_tag.map(str::to_owned);
    for (name, body) in macros {
        p.macros
            .insert(*name, MacroEntry::literal(*body, Provenance::Override));
    }
    for (name, ver) in rpmlib {
        p.rpmlib.features.insert((*name).into(), (*ver).into());
    }
    p
}
