//! Smoke test: run RPM-REPO-001/002/003 against four real-world
//! spec fixtures (gcc / openssh / nginx / python3.13) and assert
//! that the analyzer COMPLETES without panic and the rules either
//! emit `RPM-REPO-*` findings or stay silent — both are valid
//! outcomes against the tiny in-memory universe.
//!
//! The point isn't to assert specific findings (the universe is
//! tiny on purpose so most BR atoms WILL be unresolvable). The
//! point is to exercise the macro-expansion / sub-package /
//! conditional-branch paths against the messy structure of
//! production specs without crashing.
//!
//! Fixture sharing: `tiny_universe()` + `redos_profile()` live in
//! `rpm_spec_analyzer::rules::repo::test_fixtures`, exposed under
//! the `test-fixtures` Cargo feature (declared as `required-features`
//! on this test target — `cargo test` enables it automatically). The
//! same helpers feed the per-rule unit tests in `src/rules/repo/`,
//! so there's a single source of truth for the universe shape.

use std::path::Path;

use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::rules::repo::test_fixtures::{redos_profile, tiny_universe};
use rpm_spec_analyzer::{LintSession, parse};

const FIXTURE_SPECS: &[&str] = &["gcc.spec", "openssh.spec", "nginx.spec", "python3.13.spec"];

fn fixtures_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/fixtures/specs")
}

#[test]
fn fixture_specs_lint_without_panic() {
    let dir = fixtures_dir();
    assert!(
        dir.exists(),
        "fixtures dir missing: {} — run from workspace root",
        dir.display()
    );

    let config = Config::default();
    let profile = redos_profile();
    let universe = tiny_universe();

    for spec_name in FIXTURE_SPECS {
        let path = dir.join(spec_name);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let outcome = parse(&source);
        let mut session = LintSession::from_config_with_profile_and_universe(
            &config,
            profile.clone(),
            Some(universe.clone()),
        );
        let diags = session.run(&outcome.spec, &source);
        let repo_diags: Vec<_> = diags
            .iter()
            .filter(|d| d.lint_id.starts_with("RPM-REPO-"))
            .collect();
        eprintln!(
            "{spec_name}: total {} diagnostic(s), {} repo-aware",
            diags.len(),
            repo_diags.len()
        );
        // We can't assert exact counts (depend on each spec's BR
        // list which the user may update); just confirm no panic
        // and that each emitted diagnostic carries a stable
        // `RPM-REPO-NNN` ID with non-empty message.
        for d in repo_diags {
            assert!(d.lint_id.starts_with("RPM-REPO-"), "lint_id: {}", d.lint_id);
            assert!(!d.message.is_empty());
            assert!(d.repo_context.is_some(), "RPM-REPO-* must attach repo_context");
        }
    }
}
