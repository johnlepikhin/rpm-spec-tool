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
    assert!(stderr.contains("patch-defined-not-applied"), "stderr:\n{stderr}");
    assert!(stderr.contains("Patch1"), "stderr:\n{stderr}");
}

// =====================================================================
// format subcommand — indent override
// =====================================================================

const SPEC_WITH_IF: &str =
    "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if 0%{?rhel}\nRequires: rhel-pkg\n%endif\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n";

const SPEC_NO_IF: &str =
    "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n";

#[test]
fn format_default_keeps_conditionals_flush_left() {
    let spec = write_temp(SPEC_WITH_IF);
    let (code, stdout, stderr) = run(&["format", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    // Default indent = 0 → Requires: line is at column 1, no leading spaces.
    assert!(stdout.contains("\nRequires:"), "expected flush-left Requires:\n{stdout}");
    // No cosmetic warning when indent is not requested.
    assert!(!stderr.contains("cosmetic"), "stderr should not warn: {stderr}");
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
    assert!(!stderr.contains("cosmetic"), "no warning expected: {stderr}");
}

#[test]
fn format_indent_rejects_huge_value() {
    // clap's value_parser range caps --indent at MAX_INDENT (64); a
    // larger value must fail at argument parsing, not blow up the
    // printer with billions of spaces.
    let spec = write_temp(SPEC_WITH_IF);
    let (code, _, stderr) = run(
        &["format", "--indent", "9999999", spec.path().to_str().unwrap()],
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
    std::fs::write(&cfg_path, "[format]\nconditional-indent = 4\n")
        .expect("write config");
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
    assert!(stderr.contains("cosmetic only"), "expected warning: {stderr}");
    assert!(stderr.contains("would reformat"), "expected check-diff: {stderr}");
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
    assert!(stderr.contains("deep-conditional-nesting"), "stderr:\n{stderr}");
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
    assert!(stderr.contains("empty-conditional-branch"), "stderr:\n{stderr}");
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
    assert!(stderr.contains("unreachable-elif-branch"), "stderr:\n{stderr}");
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
    assert!(stderr.contains("double-negation-in-expr"), "stderr:\n{stderr}");
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
    assert!(stderr.contains("inequality-contradiction"), "stderr:\n{stderr}");
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
    assert!(stderr.contains("conditional-buildarch"), "stderr:\n{stderr}");
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
    let (code, _, stderr) =
        run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0, "warn-level parser diag doesn't fail; stderr={stderr}");
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
