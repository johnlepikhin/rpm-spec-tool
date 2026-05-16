//! Integration tests against a real `rpm --showrc` dump.

use std::path::Path;

use rpm_spec_profile::showrc;
use rpm_spec_profile::types::{MacroValue, Provenance};

const FIXTURE: &str = include_str!("fixtures/rhel7-showrc.txt");

#[test]
fn rhel7_fixture_parses_all_records() {
    let path = Path::new("tests/fixtures/rhel7-showrc.txt");
    let patch = showrc::parse(FIXTURE, Some(path));

    // The fixture has 733 macro records: 730 with `:` separator plus 3
    // with `=` (locally-overridden entries `_db_backend`, `_target_cpu`,
    // `_target_os`). The parser must accept both separators.
    assert_eq!(
        patch.macros.len(),
        733,
        "expected 733 macros, got {}",
        patch.macros.len()
    );
}

#[test]
fn rhel7_fixture_extracts_arch_info() {
    let patch = showrc::parse(FIXTURE, None);
    assert_eq!(patch.arch.build_arch.as_deref(), Some("x86_64"));
    assert_eq!(patch.arch.build_os.as_deref(), Some("Linux"));
    let archs = patch.arch.compatible_archs.expect("compatible_archs");
    assert!(archs.contains(&"x86_64".to_string()));
    assert!(archs.contains(&"noarch".to_string()));
    assert!(
        patch.arch.optflags_template.is_some(),
        "optflags template should be present"
    );
}

#[test]
fn rhel7_fixture_extracts_rpmlib_features() {
    let patch = showrc::parse(FIXTURE, None);
    let rd = patch
        .rpmlib
        .iter()
        .find(|(k, _)| k == "rpmlib(RichDependencies)")
        .expect("RichDependencies feature");
    assert_eq!(rd.1, "4.12.0-1");
}

#[test]
fn rhel7_fixture_distinguishes_builtins_and_literals() {
    let patch = showrc::parse(FIXTURE, None);

    // -20 `<builtin>` macros must come through as MacroValue::Builtin.
    let p = patch
        .macros
        .iter()
        .find(|(n, _)| n == "P")
        .expect("`P` builtin");
    assert!(matches!(p.1.value, MacroValue::Builtin));

    // `dist` on rhel7 is a single-line literal `.el7` — must be Literal.
    let dist = patch
        .macros
        .iter()
        .find(|(n, _)| n == "dist")
        .expect("dist macro");
    assert!(matches!(&dist.1.value, MacroValue::Literal(s) if s == ".el7"));

    // `__apply_patch(qp:m:)` is a multi-line lua block — must be Raw,
    // multiline=true, and keep the `(qp:m:)` opts.
    let ap = patch
        .macros
        .iter()
        .find(|(n, _)| n == "__apply_patch")
        .expect("__apply_patch macro");
    assert_eq!(ap.1.opts.as_deref(), Some("qp:m:"));
    match &ap.1.value {
        MacroValue::Raw { multiline, body } => {
            assert!(multiline, "lua-block body should be multiline");
            assert!(body.contains("rpm.expand"), "lua body lost");
        }
        other => panic!("expected Raw, got {other:?}"),
    }
}

#[test]
fn rhel7_fixture_records_source_path_in_provenance() {
    let path = Path::new("tests/fixtures/rhel7-showrc.txt");
    let patch = showrc::parse(FIXTURE, Some(path));
    // Every macro must carry the source path.
    for (name, entry) in &patch.macros {
        match &entry.provenance {
            Provenance::Showrc {
                path: Some(p),
                level: _,
            } => assert_eq!(p, path),
            other => panic!("macro {name} has unexpected provenance: {other:?}"),
        }
    }
}

#[test]
fn rhel7_fixture_distribution_markers_present() {
    let patch = showrc::parse(FIXTURE, None);

    let vendor = patch
        .macros
        .iter()
        .find(|(n, _)| n == "_vendor")
        .expect("_vendor macro");
    assert!(matches!(&vendor.1.value, MacroValue::Literal(s) if s == "redhat"));

    let rhel = patch
        .macros
        .iter()
        .find(|(n, _)| n == "rhel")
        .expect("rhel marker");
    assert!(matches!(&rhel.1.value, MacroValue::Literal(s) if s == "7"));
}

#[test]
fn rhel7_fixture_equals_separator_records_present() {
    // `_db_backend`, `_target_cpu`, `_target_os` appear in the fixture
    // with `=` instead of `:` (locally-overridden entries). Earlier
    // versions of the parser dropped these silently.
    let patch = showrc::parse(FIXTURE, None);
    for name in ["_db_backend", "_target_cpu", "_target_os"] {
        assert!(
            patch.macros.iter().any(|(n, _)| n == name),
            "expected macro {name} from `=`-separator record"
        );
    }
}
