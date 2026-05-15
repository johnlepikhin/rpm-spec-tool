//! End-to-end CLI tests covering exit codes, stdin, and key flag
//! interactions. Uses plain `std::process::Command` against the binary
//! `cargo` builds for tests — no extra dev-dependencies.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn binary() -> PathBuf {
    // `target/debug/<bin>` relative to the workspace root. `CARGO_BIN_EXE_*`
    // is set by Cargo for integration tests of the crate that owns the bin.
    PathBuf::from(env!("CARGO_BIN_EXE_rpm-spec-tool"))
}

fn run(args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let mut cmd = Command::new(binary());
    cmd.args(args);
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn rpm-spec-tool");
    if let Some(input) = stdin {
        child
            .stdin
            .as_mut()
            .expect("stdin captured")
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    let out = child.wait_with_output().expect("wait");
    let code = out.status.code().unwrap_or(-1);
    (
        code,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const CLEAN_SPEC: &str = "\
Name:           hello
Version:        1.0
Release:        1
Summary:        S

License:        MIT

%description
Body.

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

const MISSING_CHANGELOG_SPEC: &str = "\
Name:           hello
Version:        1.0
Release:        1
Summary:        S

License:        MIT

%description
Body.
";

fn write_temp(contents: &str) -> tempfile::NamedTempFile {
    let mut tmp = tempfile::Builder::new()
        .suffix(".spec")
        .tempfile()
        .expect("temp file");
    tmp.write_all(contents.as_bytes()).expect("write spec");
    tmp.flush().expect("flush");
    tmp
}

#[test]
fn version_flag_exits_zero() {
    let (code, stdout, _) = run(&["--version"], None);
    assert_eq!(code, 0);
    assert!(stdout.contains("rpm-spec-tool"));
}

#[test]
fn lint_default_warn_exits_zero() {
    let spec = write_temp(MISSING_CHANGELOG_SPEC);
    let (code, _, stderr) =
        run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0, "warn-only run must succeed; stderr={stderr}");
    assert!(stderr.contains("missing-changelog"));
}

#[test]
fn lint_deny_override_exits_one() {
    let spec = write_temp(MISSING_CHANGELOG_SPEC);
    let (code, _, _) = run(
        &[
            "lint",
            "--deny",
            "missing-changelog",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 1, "deny-promoted lint must fail with exit 1");
}

#[test]
fn lint_allow_override_silences_diagnostic() {
    let spec = write_temp(MISSING_CHANGELOG_SPEC);
    let (code, _, stderr) = run(
        &[
            "lint",
            "--allow",
            "missing-changelog",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        !stderr.contains("missing-changelog"),
        "diagnostic should be suppressed; got: {stderr}"
    );
}

#[test]
fn lint_nonexistent_file_exits_two() {
    let (code, _, stderr) = run(&["lint", "/nonexistent/path.spec"], None);
    // anyhow surfacing the IO error goes through main, which exits 2.
    assert_eq!(code, 2, "IO error must yield exit 2; stderr={stderr}");
}

#[test]
fn check_on_clean_spec_exits_zero() {
    let spec = write_temp(CLEAN_SPEC);
    let (code, _, _) = run(&["check", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
}

#[test]
fn lint_stdin_pipes_through() {
    let (code, _, stderr) = run(&["lint", "-"], Some(MISSING_CHANGELOG_SPEC));
    assert_eq!(code, 0);
    assert!(stderr.contains("<stdin>"));
    assert!(stderr.contains("missing-changelog"));
}

#[test]
fn json_output_is_parseable() {
    let spec = write_temp(MISSING_CHANGELOG_SPEC);
    let (code, stdout, _) = run(
        &[
            "lint",
            "--format",
            "json",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 0);
    let v: serde_json::Value =
        serde_json::from_str(&stdout).expect("valid JSON");
    let diags = v["files"][0]["diagnostics"].as_array().expect("array");
    assert!(diags.iter().any(|d| d["lint_id"] == "RPM001"));
}
