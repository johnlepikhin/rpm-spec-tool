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
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
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
        &["lint", "--format", "json", spec.path().to_str().unwrap()],
        None,
    );
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let diags = v["files"][0]["diagnostics"].as_array().expect("array");
    assert!(diags.iter().any(|d| d["lint_id"] == "RPM001"));
}

// =====================================================================
// Phase 1 rules — RPM010..RPM022.
// =====================================================================

#[test]
fn missing_name_tag_exits_one() {
    // Default severity for missing-name-tag is `deny`; a spec without
    // Name: must fail the lint run.
    let spec = write_temp(
        "Version: 1\nRelease: 1\nSummary: x\nLicense: MIT\nURL: https://e.org\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 1, "deny lint must fail: {stderr}");
    assert!(stderr.contains("missing-name-tag"));
}

#[test]
fn obsolete_tag_autofix_drops_packager() {
    // Run with --fix on a spec containing Packager: and verify a re-run
    // no longer reports RPM020.
    let spec = write_temp(
        "Name: x\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
URL: https://e.org\nPackager: me <me@e.org>\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let path = spec.path().to_str().unwrap();
    let (code, _, _) = run(&["lint", "--fix", path], None);
    assert_eq!(code, 0, "--fix should succeed");

    // Second pass: the Packager line is gone.
    let (code, _, stderr) = run(&["lint", path], None);
    assert_eq!(code, 0);
    assert!(
        !stderr.contains("obsolete-tag"),
        "Packager should be gone after --fix, stderr: {stderr}"
    );
}

#[test]
fn deprecated_clean_section_autofix_drops_section() {
    // The clean section is multiline (header + body until next section).
    // --fix must remove the whole block without breaking the spec.
    let spec = write_temp(
        "Name: x\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
URL: https://e.org\n\
%description\nb\n\
%clean\nrm -rf %{buildroot}\necho done\n\
%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let path = spec.path().to_str().unwrap();
    let (code, _, _) = run(&["lint", "--fix", path], None);
    assert_eq!(code, 0, "--fix should succeed");

    // After the fix the file must still parse cleanly and have no
    // deprecated-clean-section diagnostic.
    let (code, _, stderr) = run(&["lint", path], None);
    assert_eq!(code, 0);
    assert!(
        !stderr.contains("deprecated-clean-section"),
        "%clean must be gone after --fix; stderr: {stderr}"
    );
    let after = std::fs::read_to_string(path).expect("read file");
    assert!(
        !after.contains("%clean"),
        "%clean header still present:\n{after}"
    );
    assert!(
        after.contains("%changelog"),
        "%changelog must remain:\n{after}"
    );
}

// =====================================================================
// Phase 2 rules — RPM031..RPM040.
// =====================================================================

#[test]
fn self_obsoletion_exits_one() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
URL: https://e.org\nObsoletes: hello\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 1, "self-obsoletion is deny");
    assert!(stderr.contains("self-obsoletion"));
}

#[test]
fn subpackage_self_obsoletion_detected() {
    let spec = write_temp(
        "Name: main\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
URL: https://e.org\n\
%description\nb\n\
%package -n foo\n\
Summary: sub\n\
Obsoletes: foo\n\
%description -n foo\nsub\n\
%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 1);
    assert!(
        stderr.contains("self-obsoletion") && stderr.contains("foo"),
        "expected subpackage self-obsoletion mention; stderr: {stderr}"
    );
}

#[test]
fn useless_provides_autofix_drops_line() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
URL: https://e.org\nProvides: hello\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let path = spec.path().to_str().unwrap();
    let (code, _, _) = run(&["lint", "--fix", path], None);
    assert_eq!(code, 0);

    let after = std::fs::read_to_string(path).expect("read");
    assert!(
        !after.contains("Provides:"),
        "useless Provides line must be removed:\n{after}"
    );

    let (code, _, stderr) = run(&["lint", path], None);
    assert_eq!(code, 0);
    assert!(
        !stderr.contains("useless-explicit-provides"),
        "diagnostic should be gone; stderr: {stderr}"
    );
}

// =====================================================================
// Phase 3 rules — sections & changelog health.
// =====================================================================

#[test]
fn missing_prep_section_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%build\nmake\n%install\nmake install\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0, "warn shouldn't fail; stderr={stderr}");
    assert!(stderr.contains("missing-prep-section"));
}

#[test]
fn duplicate_buildscript_exits_one() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%prep\n%setup -q\n%build\nmake\n%install\nmake install\n%build\nmake more\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 1, "duplicate build is deny");
    assert!(stderr.contains("duplicate-buildscript-section"));
}

// =====================================================================
// Phase 4 rules — style / source-text.
// =====================================================================

#[test]
fn summary_ends_with_dot_autofix() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: A library.\nLicense: MIT\n\
URL: https://e.org\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let path = spec.path().to_str().unwrap();
    let (code, _, _) = run(&["lint", "--fix", path], None);
    assert_eq!(code, 0);
    let after = std::fs::read_to_string(path).unwrap();
    assert!(after.contains("Summary: A library\n"), "got:\n{after}");
    assert!(!after.contains("Summary: A library.\n"));
}

#[test]
fn hardcoded_paths_flags_install_script() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: Demo\nLicense: MIT\n\
URL: https://e.org\n\
%install\nmkdir -p /usr/lib/foo\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(stderr.contains("hardcoded-paths"));
}

#[test]
fn rpm_buildroot_shell_var_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: Demo\nLicense: MIT\n\
URL: https://e.org\n\
%install\nmkdir -p $RPM_BUILD_ROOT/usr/bin\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(stderr.contains("rpm-buildroot-shell-var"));
}

#[test]
fn macro_in_hash_comment_autofix_escapes() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: Demo\nLicense: MIT\n\
URL: https://e.org\n\
# %{name} is the thing\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let path = spec.path().to_str().unwrap();
    let (code, _, _) = run(&["lint", "--fix", path], None);
    assert_eq!(code, 0);
    let after = std::fs::read_to_string(path).unwrap();
    // The fix wraps `%{` into `%%{` so rpm leaves it alone.
    assert!(
        after.contains("# %%{name}"),
        "expected `# %%{{name}}` after fix; got:\n{after}"
    );
}

// =====================================================================
// Phase 5 rules — modernization.
// =====================================================================

#[test]
fn egrep_autofix_replaces_with_grep_e() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: Demo\nLicense: MIT\n\
URL: https://e.org\n\
%build\nls | egrep foo\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let path = spec.path().to_str().unwrap();
    let (code, _, _) = run(&["lint", "--fix", path], None);
    assert_eq!(code, 0);
    let after = std::fs::read_to_string(path).unwrap();
    assert!(after.contains("ls | grep -E foo"), "got:\n{after}");
    assert!(!after.contains("egrep"), "egrep should be gone:\n{after}");
}

#[test]
fn setup_without_q_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: Demo\nLicense: MIT\n\
URL: https://e.org\n\
%prep\n%setup -n foo\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(stderr.contains("setup-without-q-flag"), "stderr:\n{stderr}");
}

#[test]
fn patch_not_applied_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: Demo\nLicense: MIT\n\
URL: https://e.org\n\
Patch1: missing.patch\n\
%prep\n%setup -q\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("patch-defined-not-applied"),
        "stderr:\n{stderr}"
    );
    assert!(stderr.contains("Patch1"), "stderr:\n{stderr}");
}

// =====================================================================
// format subcommand — indent override
// =====================================================================

const SPEC_WITH_IF: &str = "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if 0%{?rhel}\nRequires: rhel-pkg\n%endif\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n";

const SPEC_NO_IF: &str = "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n";

#[test]
fn format_default_keeps_conditionals_flush_left() {
    let spec = write_temp(SPEC_WITH_IF);
    let (code, stdout, stderr) = run(&["format", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    // Default indent = 0 → Requires: line is at column 1, no leading spaces.
    assert!(
        stdout.contains("\nRequires:"),
        "expected flush-left Requires:\n{stdout}"
    );
    // No cosmetic warning when indent is not requested.
    assert!(
        !stderr.contains("cosmetic"),
        "stderr should not warn: {stderr}"
    );
}

#[test]
fn format_indent_warns_and_indents() {
    let spec = write_temp(SPEC_WITH_IF);
    let (code, stdout, stderr) = run(
        &["format", "--indent", "4", spec.path().to_str().unwrap()],
        None,
    );
    assert_eq!(code, 0);
    // Body of the %if is indented by 4 spaces.
    assert!(
        stdout.contains("\n    Requires:"),
        "expected 4-space-indented Requires:\n{stdout}"
    );
    // Warning is emitted on stderr and identifies the CLI source.
    assert!(
        stderr.contains("--indent > 0") && stderr.contains("cosmetic only"),
        "expected CLI-sourced cosmetic warning, got:\n{stderr}"
    );
}

#[test]
fn format_indent_zero_no_warning() {
    let spec = write_temp(SPEC_NO_IF);
    let (code, _, stderr) = run(
        &["format", "--indent", "0", spec.path().to_str().unwrap()],
        None,
    );
    assert_eq!(code, 0);
    // --indent 0 is a no-op; no warning.
    assert!(
        !stderr.contains("cosmetic"),
        "no warning expected: {stderr}"
    );
}

#[test]
fn format_indent_rejects_huge_value() {
    // clap's value_parser range caps --indent at MAX_INDENT (64); a
    // larger value must fail at argument parsing, not blow up the
    // printer with billions of spaces.
    let spec = write_temp(SPEC_WITH_IF);
    let (code, _, stderr) = run(
        &[
            "format",
            "--indent",
            "9999999",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_ne!(code, 0, "expected non-zero exit for out-of-range --indent");
    assert!(
        stderr.contains("not in") || stderr.contains("invalid value"),
        "expected clap range-validation error, got:\n{stderr}"
    );
}

#[test]
fn format_indent_warning_from_config_file() {
    // The cosmetic warning must also fire when `conditional_indent`
    // comes from `.rpmspec.toml` (not just from `--indent`). The
    // warning label distinguishes the two sources so the user knows
    // where to look.
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg_path = tmp.path().join("rpmspec.toml");
    // TOML keys use kebab-case (`conditional-indent`) to match the
    // serde rename in `analyzer::config::FormatConfig`.
    std::fs::write(&cfg_path, "[format]\nconditional-indent = 4\n").expect("write config");
    let spec_path = tmp.path().join("hello.spec");
    std::fs::write(&spec_path, SPEC_WITH_IF).expect("write spec");

    let (code, stdout, stderr) = run(
        &[
            "format",
            "--config",
            cfg_path.to_str().unwrap(),
            spec_path.to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains("\n    Requires:"),
        "expected 4-space indent from config:\n{stdout}"
    );
    assert!(
        stderr.contains("[format].conditional_indent") && stderr.contains("cosmetic only"),
        "expected config-sourced cosmetic warning, got:\n{stderr}"
    );
}

#[test]
fn format_check_with_indent_exits_one() {
    // `--check --indent N>0` is a likely CI footgun: the indented
    // output never matches the original, so exit code is 1 *and* the
    // user still gets the cosmetic warning. Lock the behaviour in.
    let spec = write_temp(SPEC_WITH_IF);
    let (code, _, stderr) = run(
        &[
            "format",
            "--check",
            "--indent",
            "4",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 1);
    assert!(
        stderr.contains("cosmetic only"),
        "expected warning: {stderr}"
    );
    assert!(
        stderr.contains("would reformat"),
        "expected check-diff: {stderr}"
    );
}

// =====================================================================
// Phase 6 — conditional-block lints.
// =====================================================================

#[test]
fn deep_conditional_nesting_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if 1\n%if 1\n%if 1\n%if 1\n%if 1\nBuildArch: noarch\n\
%endif\n%endif\n%endif\n%endif\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("deep-conditional-nesting"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn constant_condition_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if 0\nBuildArch: noarch\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(stderr.contains("constant-condition"), "stderr:\n{stderr}");
}

#[test]
fn empty_conditional_branch_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if 0\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("empty-conditional-branch"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn unreachable_elif_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if 0\nBuildArch: noarch\n%elif 0\nBuildArch: x86_64\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("unreachable-elif-branch"),
        "stderr:\n{stderr}"
    );
}

// =====================================================================
// Phase 7 — conditional optimisation.
// =====================================================================

#[test]
fn nested_and_collapse_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if 1\n%if 1\nBuildArch: noarch\n%endif\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(stderr.contains("nested-and-collapse"), "stderr:\n{stderr}");
}

#[test]
fn double_negation_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if !!0%{?rhel}\nBuildArch: noarch\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("double-negation-in-expr"),
        "stderr:\n{stderr}"
    );
}

// =====================================================================
// Phase 7d — interval analysis + anti-patterns.
// =====================================================================

#[test]
fn inequality_contradiction_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if X >= 10 && X < 5\nBuildArch: noarch\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("inequality-contradiction"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn conditional_buildarch_warns_when_enabled() {
    // RPM106 is Allow by default, so we need `--warn` to surface it.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if 1\nBuildArch: noarch\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(
        &[
            "lint",
            "--warn",
            "conditional-buildarch",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        stderr.contains("conditional-buildarch"),
        "stderr:\n{stderr}"
    );
}

// =====================================================================
// Phase 8b — path-condition engine (RPM113/114/115/116).
// =====================================================================

#[test]
fn unreachable_branch_warns_on_nested_negation() {
    // Outer path `!X` makes inner `X` impossible — RPM113.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if !X\n%if X\nBuildArch: noarch\n%endif\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("unreachable-branch-under-parent"),
        "stderr:\n{stderr}"
    );
}

// =====================================================================
// Phase X — `pretty` subcommand (ANSI syntax highlighting).
// =====================================================================

#[test]
fn pretty_emits_ansi_when_color_always() {
    let spec = write_temp(CLEAN_SPEC);
    let (code, stdout, _) = run(
        &["pretty", "--color", "always", spec.path().to_str().unwrap()],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        stdout.contains('\x1b'),
        "expected ANSI escape in stdout with --color always:\n{stdout}"
    );
    // Spec content must still survive the round trip through pretty.
    assert!(
        stdout.contains("hello"),
        "expected the Name value in stdout:\n{stdout}"
    );
}

#[test]
fn pretty_omits_ansi_when_piped_under_color_auto() {
    // `run()` collects stdout via a pipe — stdout is *not* a TTY in
    // the test harness, so `--color auto` (the default) must elide
    // colour. This is the matched-clippy/bat behaviour.
    let spec = write_temp(CLEAN_SPEC);
    let (code, stdout, _) = run(&["pretty", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains('\x1b'),
        "expected no ANSI escapes on piped stdout:\n{stdout:?}"
    );
}

#[test]
fn pretty_round_trips_through_ansi_strip() {
    // `pretty --color never` must produce output that re-parses to
    // the same AST as the input. We don't compare bytes against
    // `format` because pretty defaults to `--indent 2` (a display
    // choice) — instead we round-trip through the parser.
    let spec = write_temp(CLEAN_SPEC);
    let (code, stdout, _) = run(
        &["pretty", "--color", "never", spec.path().to_str().unwrap()],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        !stdout.contains('\x1b'),
        "expected no ANSI escapes with --color never"
    );
    // Sanity: a few key strings must survive.
    for needle in &["Name:", "hello", "%description", "%changelog"] {
        assert!(
            stdout.contains(needle),
            "missing `{needle}` in pretty output:\n{stdout}"
        );
    }
    // Real regression coverage: re-parse the pretty output and require
    // its AST to match the input's AST. `parse_str` already strips
    // spans (returns `SpecFile<()>`), so direct equality is meaningful.
    let original = rpm_spec::parser::parse_str(CLEAN_SPEC).spec;
    let reprinted = rpm_spec::parser::parse_str(&stdout).spec;
    assert_eq!(
        original, reprinted,
        "pretty output must re-parse to the same AST.\n=== STDOUT ===\n{stdout}\n=== END ==="
    );
}

#[test]
fn pretty_renders_fixture_without_error() {
    // Mirrors how `lint`/`format` tests would consume a fixture file
    // (kept for consistency once the analyzer pulls fixtures from the
    // workspace tree). Asserts a clean exit and AST round-trip on a
    // representative spec touching preamble, %if, %description and
    // %changelog token kinds.
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/pretty_sample.spec"
    );
    let source = std::fs::read_to_string(fixture).expect("read fixture");
    let (code, stdout, stderr) = run(&["pretty", "--color", "never", fixture], None);
    assert_eq!(code, 0, "pretty on fixture must succeed; stderr={stderr}");
    let original = rpm_spec::parser::parse_str(&source).spec;
    let reprinted = rpm_spec::parser::parse_str(&stdout).spec;
    assert_eq!(
        original, reprinted,
        "fixture pretty output must re-parse to the same AST.\n\
         === STDOUT ===\n{stdout}\n=== END ==="
    );
}

#[test]
fn pretty_indent_override_applies() {
    // `--indent 4` must indent the inner `%if` body by 4 spaces per
    // level. Use the existing nested-conditional fixture so the test
    // exercises both the CLI flag plumbing and the printer's nested
    // indent logic.
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/bad_unreachable_nested.spec"
    );
    let (code, stdout, stderr) = run(
        &["pretty", "--color", "never", "--indent", "4", fixture],
        None,
    );
    assert_eq!(code, 0, "pretty --indent 4 must succeed; stderr={stderr}");
    // Inner nested `%if X` sits two levels deep, so its body line
    // (`BuildArch: ...`) must be prefixed by 8 spaces, and the inner
    // `%if`/`%endif` themselves by 4.
    assert!(
        stdout.contains("\n    %if X\n"),
        "expected 4-space indent on inner %if; got:\n{stdout}"
    );
    assert!(
        stdout.contains("\n        BuildArch:"),
        "expected 8-space indent on doubly-nested body; got:\n{stdout}"
    );
}

#[test]
fn pretty_default_indent_is_two_when_config_zero() {
    // No `--indent` flag and no config override → the display-mode
    // floor in `commands/pretty.rs` kicks in and pcfg.indent is set to
    // `DEFAULT_PRETTY_INDENT` (2). Asserts the floor end-to-end.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\n\
URL: https://e.org\n\
%if 1\n%global x 1\n%endif\n\
%description\nb\n\
%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, stdout, stderr) = run(
        &["pretty", "--color", "never", spec.path().to_str().unwrap()],
        None,
    );
    assert_eq!(code, 0, "pretty must succeed; stderr={stderr}");
    assert!(
        stdout.contains("\n  %global x 1\n"),
        "expected default 2-space indent inside %if; got:\n{stdout}"
    );
}

#[test]
fn pretty_preamble_align_column_override_applies() {
    // `--preamble-align-column 20` widens the gap between tag colons
    // and their values. With the default of 16, `Name:` is followed
    // by 11 spaces; at 20 it must be followed by 15.
    let spec = write_temp(CLEAN_SPEC);
    let (code, stdout, stderr) = run(
        &[
            "pretty",
            "--color",
            "never",
            "--preamble-align-column",
            "20",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 0, "pretty must succeed; stderr={stderr}");
    assert!(
        stdout.contains("Name:               hello"),
        "expected value aligned at column 20; got:\n{stdout}"
    );
    // AST still round-trips after the alignment change.
    let original = rpm_spec::parser::parse_str(CLEAN_SPEC).spec;
    let reprinted = rpm_spec::parser::parse_str(&stdout).spec;
    assert_eq!(original, reprinted);
}

#[test]
fn always_true_branch_warns_on_implied_inner() {
    // path = X && Y; inner = X — path implies branch, RPM114.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if X && Y\n%if X\nBuildArch: noarch\n%endif\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("always-true-branch-under-parent"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn dead_elif_warns_when_repeating_prior_branch() {
    // `%if A %elif A` — the second branch's effective condition is
    // `A ∧ ¬A` = ⊥ — RPM115.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if A\nBuildArch: noarch\n%elif A\nBuildArch: x86_64\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("dead-elif-after-parent"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn exhaustive_chain_warns_when_else_is_implicit() {
    // `%if A %elif !A` covers the whole boolean space — the final
    // `%elif` is equivalent to `%else` — RPM116.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if A\nBuildArch: noarch\n%elif !A\nBuildArch: x86_64\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("mutex-branches-spell-out-else"),
        "stderr:\n{stderr}"
    );
}

// =====================================================================
// Per-atom spans for multi-dep — regression tests against span aliasing.
// =====================================================================

#[test]
fn hoist_common_suffix_fires_on_multi_dep() {
    // Before the per-atom-span fix in `rpm-spec`, both branches' single
    // `BuildRequires:` line shared the whole-line span, so RPM098 saw
    // two different source slices and missed the common trailing
    // `gcc-c++`. With per-atom spans the trailing atom is comparable
    // independently across branches.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if A\n\
BuildRequires: alpha, gcc-c++\n\
%else\n\
BuildRequires: beta, gcc-c++\n\
%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("hoist-common-suffix-from-branches"),
        "RPM098 must fire on shared multi-dep atom:\n{stderr}"
    );
}

#[test]
fn leaf_hoist_fires_on_multi_dep_atom_across_nested_arms() {
    // Two-level nesting where every leaf shares one multi-dep atom.
    // RPM119 should now see the atom as a per-item span and find it
    // in every root-to-leaf path.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if outer\n\
BuildRequires: alpha, gcc-c++\n\
%else\n\
%if inner\n\
BuildRequires: beta, gcc-c++\n\
%else\n\
BuildRequires: gamma, gcc-c++\n\
%endif\n\
%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stderr.contains("common-leaf-line-hoistable"),
        "RPM119 must fire on multi-dep atom common to every leaf:\n{stderr}"
    );
}

// =====================================================================
// Phase 8c — macro value propagation (RPM117/118).
// =====================================================================

#[test]
fn macro_makes_if_trivial_warns_when_enabled() {
    // RPM117 defaults to `Allow` because `%define FLAG <default>` is
    // idiomatically a CLI knob (`rpmbuild --define`). Opt in via
    // `--warn` for hygiene passes where genuine dead constants matter.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%global with_python 1\n\
%if %{with_python}\nBuildRequires: python3\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(
        &[
            "lint",
            "--warn",
            "macro-defined-makes-if-trivial",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        stderr.contains("macro-defined-makes-if-trivial"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn unused_global_warns_when_enabled() {
    // RPM118 is Allow by default — opt in via `--warn`.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%global never_used 1\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(
        &[
            "lint",
            "--warn",
            "unused-conditional-global",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        stderr.contains("unused-conditional-global"),
        "stderr:\n{stderr}"
    );
}

#[test]
fn parser_bridge_silenced_by_cli_allow() {
    // Same fixture as `parser_bridge_surfaces_warnings`, but with the
    // bridged rule silenced through `--allow`. The diagnostic must
    // disappear from stderr.
    let spec = write_temp(
        "Name: x\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
Provides: %{\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(
        &[
            "lint",
            "--allow",
            "parse-unterminated-macro",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 0);
    assert!(
        !stderr.contains("parse-unterminated-macro"),
        "diagnostic must be silenced by --allow; stderr: {stderr}"
    );
}

#[test]
fn parser_bridge_surfaces_warnings() {
    // `Provides: %{` is an unterminated macro reference — the parser
    // emits `rpmspec/W0004` which our bridge re-emits as
    // `parse-unterminated-macro`.
    let spec = write_temp(
        "Name: x\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
Provides: %{\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(
        code, 0,
        "warn-level parser diag doesn't fail; stderr={stderr}"
    );
    assert!(
        stderr.contains("parse-unterminated-macro"),
        "expected parser bridge to surface W0004; got: {stderr}"
    );
}

#[test]
fn multiple_changelog_sections_exits_one() {
    let spec = write_temp(
        "Name: x\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%description\nb\n\
%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- A\n\
%changelog\n* Tue Jan 02 2024 b <b@c> - 1-2\n- B\n",
    );
    let (code, _, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 1, "duplicate %changelog is deny");
    assert!(stderr.contains("multiple-changelog-sections"));
}

#[test]
fn shellcheck_fixture_emits_rpm200_or_rpm201() {
    // Verifies the shellcheck umbrella (RPM200) end-to-end via the
    // bundled fixture. We do not assume shellcheck is installed on
    // every CI host — either the rule produces RPM200 findings (when
    // the binary is present) or a single RPM201 unavailable diag (when
    // it is not). Both outcomes prove the pipeline is wired up.
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/bad_shellcheck.spec"
    );
    let (code, _, stderr) = run(&["lint", fixture], None);
    assert_eq!(code, 0, "shellcheck is default-warn, must not fail run");
    assert!(
        stderr.contains("shellcheck") || stderr.contains("RPM200") || stderr.contains("RPM201"),
        "expected shellcheck-related diagnostics; got: {stderr}"
    );
}

#[test]
fn profile_show_default_is_generic() {
    // `profile show` with no config + no args resolves the built-in
    // `generic` baseline. No error, identifies the family explicitly.
    let (code, stdout, _) = run(&["profile", "show"], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("name:     generic"),
        "unexpected stdout: {stdout}"
    );
    assert!(
        stdout.contains("family:   Generic"),
        "unexpected stdout: {stdout}"
    );
}

#[test]
fn profile_show_with_inline_overrides() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join(".rpmspec.toml");
    std::fs::write(
        &cfg,
        r#"
profile = "custom"

[profiles.custom.identity]
family = "alt"
vendor = "acme"
"#,
    )
    .expect("write config");

    let (code, stdout, stderr) = run(
        &["profile", "show", "--config", cfg.to_str().unwrap()],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("family:   Alt"), "stdout={stdout}");
    assert!(stdout.contains("vendor:   acme"), "stdout={stdout}");
    assert!(
        stdout.contains("override"),
        "expected override layer; stdout={stdout}"
    );
}

#[test]
fn profile_macros_filter_narrows_output() {
    // `--filter optflags` keeps only macros whose name contains
    // "optflags" (case-insensitive). RHEL bundles ~700 macros, so an
    // unfiltered call would print hundreds of lines — filtered, single
    // digits.
    let (code, stdout, stderr) = run(
        &["profile", "macros", "rhel-9-x86_64", "--filter", "optflags"],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("optflags"), "stdout={stdout}");
    assert!(
        stdout.contains("matching"),
        "header should mention filter; stdout={stdout}"
    );
    // No unrelated macros leaked through the filter.
    assert!(
        !stdout.contains("\n  dist "),
        "filter let through `dist`; stdout={stdout}"
    );
}

#[test]
fn profile_macro_known_prints_value_and_exits_zero() {
    let (code, stdout, stderr) = run(&["profile", "macro", "dist", "rhel-8-x86_64"], None);
    assert_eq!(code, 0, "stderr={stderr}");
    // RHEL 8 has literal `dist = .el8` in showrc.
    assert!(stdout.starts_with("dist = .el8"), "stdout={stdout}");
    assert!(stdout.contains("[showrc:"), "stdout={stdout}");
}

#[test]
fn profile_macro_unknown_exits_two() {
    let (code, _stdout, stderr) = run(
        &[
            "profile",
            "macro",
            "definitely-not-a-real-macro",
            "rhel-9-x86_64",
        ],
        None,
    );
    assert_eq!(code, 2, "undefined macro must yield exit code 2");
    assert!(
        stderr.contains("not defined") && stderr.contains("rhel-9-x86_64"),
        "stderr={stderr}"
    );
}

#[test]
fn profile_macro_multi_profile_renders_table() {
    // Two or more explicit profiles → comparison table.
    let (code, stdout, stderr) = run(
        &[
            "profile",
            "macro",
            "dist",
            "rhel-8-x86_64",
            "rhel-9-x86_64",
            "altlinux-10-x86_64",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    // Header names the macro and the profile count.
    assert!(stdout.contains("`dist`"), "stdout={stdout}");
    assert!(stdout.contains("3 profile"), "stdout={stdout}");
    // One row per profile.
    assert!(stdout.contains("rhel-8-x86_64"), "stdout={stdout}");
    assert!(stdout.contains("rhel-9-x86_64"), "stdout={stdout}");
    // ALT has no `dist` macro — must be reported as undefined.
    assert!(
        stdout.contains("altlinux-10-x86_64") && stdout.contains("(undefined)"),
        "stdout={stdout}"
    );
}

#[test]
fn profile_common_existence_mode_lists_shared_names() {
    let (code, stdout, stderr) = run(
        &["profile", "common", "rhel-8-x86_64", "rhel-9-x86_64"],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stdout.contains("Common macros across 2 profile(s)"),
        "stdout={stdout}"
    );
    // `__7zip` is present in every RHEL showrc — must appear in the
    // intersection. Output is bare names (no `=`).
    assert!(stdout.contains("__7zip"), "stdout={stdout}");
    // Existence mode renders just names — no `=` on the data lines.
    let data_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("  ") && !l.contains("no common"))
        .collect();
    assert!(!data_lines.is_empty(), "stdout={stdout}");
    assert!(
        data_lines.iter().all(|l| !l.contains('=')),
        "existence mode must not print `=`; stdout={stdout}"
    );
}

#[test]
fn profile_common_by_value_mode_prints_values() {
    let (code, stdout, stderr) = run(
        &[
            "profile",
            "common",
            "--mode",
            "value",
            "rhel-8-x86_64",
            "rhel-9-x86_64",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stdout.contains("Macros with identical values across 2 profile(s)"),
        "stdout={stdout}"
    );
    // By-value mode prints `name = value` (no provenance tag).
    assert!(
        stdout.contains("__7zip") && stdout.contains("/usr/bin/7za"),
        "stdout={stdout}"
    );
}

#[test]
fn profile_common_filter_narrows_output() {
    // Bare invocation gives us the unfiltered total to pin against.
    let (code_bare, stdout_bare, _) = run(
        &["profile", "common", "rhel-8-x86_64", "rhel-9-x86_64"],
        None,
    );
    assert_eq!(code_bare, 0);
    // Header line shape: "# Common macros across 2 profile(s): N"
    let bare_total: usize = stdout_bare
        .lines()
        .next()
        .and_then(|line| line.rsplit(": ").next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("could not parse bare total from header: {stdout_bare}"));
    assert!(
        bare_total > 100,
        "expected sizable intersection, got {bare_total}"
    );

    let (code, stdout, stderr) = run(
        &[
            "profile",
            "common",
            "--filter",
            "build",
            "rhel-8-x86_64",
            "rhel-9-x86_64",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    // Header now reports `X total, Y matching "build"`. The X must
    // match the unfiltered count from above — pins the single-pass
    // intersection guarantee.
    let header = stdout.lines().next().unwrap();
    let expected_prefix = format!("# Common macros across 2 profile(s): {bare_total} total, ");
    assert!(
        header.starts_with(&expected_prefix),
        "header `{header}` should start with `{expected_prefix}`"
    );
    // Parse the "matching" count and assert it is > 0 (there are
    // definitely `___build_*` macros shared between RHEL 8/9).
    let matching: usize = header
        .rsplit_once(", ")
        .and_then(|(_, suffix)| suffix.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("could not parse matching count from header: {header}"));
    assert!(
        matching > 0 && matching < bare_total,
        "matching={matching}, bare={bare_total}"
    );
    // Body shows actual macro names containing the filter.
    assert!(stdout.contains("___build_"), "stdout={stdout}");
}

#[test]
fn profile_common_single_profile_rejected() {
    let (code, _stdout, stderr) = run(&["profile", "common", "rhel-9-x86_64"], None);
    assert_eq!(code, 2, "single-profile intersection should exit 2");
    assert!(
        stderr.contains("at least two") && stderr.contains("common"),
        "stderr={stderr}"
    );
}

#[test]
fn profile_macro_default_shows_all_profiles() {
    // No profile argument → default to a comparison table across every
    // available built-in (plus user-defined profiles from the config,
    // none in this no-config invocation).
    let (code, stdout, stderr) = run(&["profile", "macro", "dist"], None);
    assert_eq!(code, 0, "stderr={stderr}");
    // Header reflects the 24 bundled built-ins.
    assert!(stdout.contains("`dist`"), "stdout={stdout}");
    assert!(stdout.contains("profile(s)"), "stdout={stdout}");
    // Multiple profile rows present.
    assert!(stdout.contains("rhel-9-x86_64"), "stdout={stdout}");
    assert!(stdout.contains("altlinux-10-x86_64"), "stdout={stdout}");
    // generic has no macros — must appear as undefined.
    assert!(
        stdout.contains("generic") && stdout.contains("(undefined)"),
        "stdout={stdout}"
    );
}

#[test]
fn lint_unknown_profile_reports_error() {
    let spec = write_temp(CLEAN_SPEC);
    let (code, _, stderr) = run(
        &[
            "lint",
            "--profile",
            "does-not-exist",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 2, "unknown --profile is an IO/config error");
    assert!(
        stderr.contains("does-not-exist") && stderr.contains("not defined"),
        "expected helpful error in stderr; got: {stderr}"
    );
}

#[test]
fn lint_with_valid_profile_passes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = dir.path().join(".rpmspec.toml");
    std::fs::write(
        &cfg,
        r#"
[profiles.acme]
[profiles.acme.identity]
family = "rhel"
"#,
    )
    .expect("write config");

    let spec_path = dir.path().join("hello.spec");
    std::fs::write(&spec_path, CLEAN_SPEC).expect("write spec");

    let (code, _, stderr) = run(
        &[
            "lint",
            "--profile",
            "acme",
            "--config",
            cfg.to_str().unwrap(),
            spec_path.to_str().unwrap(),
        ],
        None,
    );
    // Clean spec → no diagnostics → exit 0 even with profile flag.
    assert_eq!(code, 0, "stderr={stderr}");
}
