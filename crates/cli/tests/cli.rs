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
    let (code, stdout, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0, "warn-only run must succeed; stderr={stderr}");
    assert!(stdout.contains("missing-changelog"));
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
fn lint_deny_warnings_meta_fails_on_any_warning() {
    let spec = write_temp(MISSING_CHANGELOG_SPEC);
    let (code, stdout, _) = run(
        &["lint", "--deny", "warnings", spec.path().to_str().unwrap()],
        None,
    );
    assert_eq!(code, 1, "`-D warnings` must promote warns to exit 1");
    // The warning still appears in output (now as deny-level), per
    // clippy semantics: `-D warnings` *fails*, it doesn't silence.
    assert!(stdout.contains("missing-changelog"));
}

#[test]
fn lint_deny_warnings_respects_per_lint_allow() {
    // `--allow X --deny warnings` keeps X silent even though every
    // other Warn promotes to Deny. We can't assert exit code 0 here
    // because the fixture trips on plenty of *other* warnings that
    // RPM rightly flags — we assert the targeted lint stays out of
    // the output, while the overall run still fails.
    let spec = write_temp(MISSING_CHANGELOG_SPEC);
    let (code, stdout, _) = run(
        &[
            "lint",
            "--allow",
            "missing-changelog",
            "--deny",
            "warnings",
            spec.path().to_str().unwrap(),
        ],
        None,
    );
    assert_eq!(code, 1, "other warnings still promote to deny");
    assert!(
        !stdout.contains("missing-changelog"),
        "missing-changelog must be silenced by --allow"
    );
}

#[test]
fn lint_allow_override_silences_diagnostic() {
    let spec = write_temp(MISSING_CHANGELOG_SPEC);
    let (code, stdout, _) = run(
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
        !stdout.contains("missing-changelog"),
        "diagnostic should be suppressed; got: {stdout}"
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
    let (code, stdout, _) = run(&["lint", "-"], Some(MISSING_CHANGELOG_SPEC));
    assert_eq!(code, 0);
    assert!(stdout.contains("<stdin>"));
    assert!(stdout.contains("missing-changelog"));
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
    let (code, stdout, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 1, "deny lint must fail: {stderr}");
    assert!(stdout.contains("missing-name-tag"));
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
    let (code, stdout, _) = run(&["lint", path], None);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains("obsolete-tag"),
        "Packager should be gone after --fix, stdout: {stdout}"
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
    let (code, stdout, _) = run(&["lint", path], None);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains("deprecated-clean-section"),
        "%clean must be gone after --fix; stdout: {stdout}"
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 1, "self-obsoletion is deny");
    assert!(stdout.contains("self-obsoletion"));
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 1);
    assert!(
        stdout.contains("self-obsoletion") && stdout.contains("foo"),
        "expected subpackage self-obsoletion mention; stdout: {stdout}"
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

    let (code, stdout, _) = run(&["lint", path], None);
    assert_eq!(code, 0);
    assert!(
        !stdout.contains("useless-explicit-provides"),
        "diagnostic should be gone; stdout: {stdout}"
    );
}

// =====================================================================
// Phase 3 rules — sections & changelog health.
// =====================================================================

#[test]
fn missing_prep_section_warns() {
    // `make install DESTDIR=%{buildroot}` avoids Phase 19's
    // make-install-missing-destdir Deny; the test is only meant to
    // exercise RPM016 missing-prep-section, which is Warn-level.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%build\nmake\n%install\nmake install DESTDIR=%{buildroot}\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, stdout, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0, "warn shouldn't fail; stderr={stderr}");
    assert!(stdout.contains("missing-prep-section"));
}

#[test]
fn duplicate_buildscript_exits_one() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%prep\n%setup -q\n%build\nmake\n%install\nmake install\n%build\nmake more\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 1, "duplicate build is deny");
    assert!(stdout.contains("duplicate-buildscript-section"));
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
    // Use a `%{buildroot}`-prefixed target so the literal path still
    // exercises RPM050 hardcoded-paths (Warn) without simultaneously
    // tripping Phase 19's install-writes-outside-buildroot (Deny).
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: Demo\nLicense: MIT\n\
URL: https://e.org\n\
%install\nmkdir -p %{buildroot}/usr/lib/foo\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(stdout.contains("hardcoded-paths"));
}

#[test]
fn rpm_buildroot_shell_var_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: Demo\nLicense: MIT\n\
URL: https://e.org\n\
%install\nmkdir -p $RPM_BUILD_ROOT/usr/bin\n\
%description\nBody.\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(stdout.contains("rpm-buildroot-shell-var"));
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(stdout.contains("setup-without-q-flag"), "stdout:\n{stdout}");
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("patch-defined-not-applied"),
        "stdout:\n{stdout}"
    );
    assert!(stdout.contains("Patch1"), "stdout:\n{stdout}");
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("deep-conditional-nesting"),
        "stdout:\n{stdout}"
    );
}

#[test]
fn constant_condition_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if 0\nBuildArch: noarch\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(stdout.contains("constant-condition"), "stdout:\n{stdout}");
}

#[test]
fn empty_conditional_branch_warns() {
    // The lint fires when the branch has only blank/comment filler —
    // a truly zero-item body is treated as parser drop-out (see
    // `is_empty_top_body` doc) and stays silent.
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if 0\n\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("empty-conditional-branch"),
        "stdout:\n{stdout}"
    );
}

#[test]
fn unreachable_elif_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if 0\nBuildArch: noarch\n%elif 0\nBuildArch: x86_64\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("unreachable-elif-branch"),
        "stdout:\n{stdout}"
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(stdout.contains("nested-and-collapse"), "stdout:\n{stdout}");
}

#[test]
fn double_negation_warns() {
    let spec = write_temp(
        "Name: hello\nVersion: 1\nRelease: 1\nSummary: s\nLicense: MIT\nURL: https://e.org\n\
%if !!0%{?rhel}\nBuildArch: noarch\n%endif\n\
%description\nb\n%changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
    );
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("double-negation-in-expr"),
        "stdout:\n{stdout}"
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("inequality-contradiction"),
        "stdout:\n{stdout}"
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
    let (code, stdout, _) = run(
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
        stdout.contains("conditional-buildarch"),
        "stdout:\n{stdout}"
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("unreachable-branch-under-parent"),
        "stdout:\n{stdout}"
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("always-true-branch-under-parent"),
        "stdout:\n{stdout}"
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("dead-elif-after-parent"),
        "stdout:\n{stdout}"
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("mutex-branches-spell-out-else"),
        "stdout:\n{stdout}"
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("hoist-common-suffix-from-branches"),
        "RPM098 must fire on shared multi-dep atom:\n{stdout}"
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("common-leaf-line-hoistable"),
        "RPM119 must fire on multi-dep atom common to every leaf:\n{stdout}"
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
    let (code, stdout, _) = run(
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
        stdout.contains("macro-defined-makes-if-trivial"),
        "stdout:\n{stdout}"
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
    let (code, stdout, _) = run(
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
        stdout.contains("unused-conditional-global"),
        "stdout:\n{stdout}"
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
    let (code, stdout, _) = run(
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
        !stdout.contains("parse-unterminated-macro"),
        "diagnostic must be silenced by --allow; stdout: {stdout}"
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
    let (code, stdout, stderr) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(
        code, 0,
        "warn-level parser diag doesn't fail; stderr={stderr}"
    );
    assert!(
        stdout.contains("parse-unterminated-macro"),
        "expected parser bridge to surface W0004; got: {stdout}"
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
    let (code, stdout, _) = run(&["lint", spec.path().to_str().unwrap()], None);
    assert_eq!(code, 1, "duplicate %changelog is deny");
    assert!(stdout.contains("multiple-changelog-sections"));
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
    let (code, stdout, _) = run(&["lint", fixture], None);
    assert_eq!(code, 0, "shellcheck is default-warn, must not fail run");
    assert!(
        stdout.contains("shellcheck") || stdout.contains("RPM200") || stdout.contains("RPM201"),
        "expected shellcheck-related diagnostics; got: {stdout}"
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

/// End-to-end coverage for the family-gated rules (RPM127/128/129).
/// Runs the same spec through profiles representing three distinct
/// distro families and asserts that the diagnostic-id sets differ
/// according to each rule's documented gating. Catches accidental
/// registry-entry removal, gate inversions, and profile-resolution
/// regressions that pure unit tests can't see.
#[test]
fn cross_profile_lint_counts_differ_by_family() {
    // Spec exercises all 3 family-gated triggers:
    //   - `License: GPLv2+`   → RPM127 on Fedora ≥ 40 only.
    //   - missing `Group:`     → RPM128 on openSUSE only.
    //   - `%bcond_with python3` → RPM129 on non-Fedora/non-RHEL only.
    let spec_src = "\
Name:           cross-profile-test
Version:        1
Release:        1
License:        GPLv2+
Summary:        s
URL:            https://e.org

%bcond_with python3

%description
b

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
";
    let spec = write_temp(spec_src);
    let path = spec.path().to_str().unwrap();

    // Use JSON output for a stable, machine-readable diagnostic stream
    // — stderr text format is human-targeted and may change column
    // padding / ANSI codes / grouping without notice.
    let ids_for = |profile: &str| -> std::collections::BTreeSet<String> {
        let (_code, stdout, stderr) = run(
            &["lint", "--format", "json", "--profile", profile, path],
            None,
        );
        let v: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!("invalid JSON from `--format json`: {e}\nstdout={stdout}\nstderr={stderr}")
        });
        let mut ids = std::collections::BTreeSet::new();
        for file in v["files"].as_array().into_iter().flatten() {
            for diag in file["diagnostics"].as_array().into_iter().flatten() {
                if let Some(id) = diag["lint_id"].as_str() {
                    ids.insert(id.to_string());
                }
            }
        }
        ids
    };

    // No bundled `fedora-N` profile yet, but `rhel-9-x86_64` (Family::Rhel)
    // is enough to assert the polarity flips: RPM127 needs Fedora ≥ 40,
    // RPM128 needs Opensuse, RPM129 needs NOT (Fedora|Rhel).
    let rhel = ids_for("rhel-9-x86_64");
    let alt = ids_for("altlinux-10-x86_64");
    let suse = ids_for("sles-15-x86_64");

    // RHEL — none of the three should fire.
    assert!(
        !rhel.contains("RPM127") && !rhel.contains("RPM128") && !rhel.contains("RPM129"),
        "rhel-9 should silence all three family-gated rules; got {rhel:?}"
    );

    // ALT — RPM129 fires (non-Fedora/RHEL with %bcond), others silent.
    assert!(
        alt.contains("RPM129"),
        "alt should flag RPM129; got {alt:?}"
    );
    assert!(
        !alt.contains("RPM127"),
        "alt should NOT flag RPM127; got {alt:?}"
    );
    assert!(
        !alt.contains("RPM128"),
        "alt should NOT flag RPM128; got {alt:?}"
    );

    // SLES (Opensuse family) — RPM128 (missing Group) + RPM129 (%bcond).
    assert!(
        suse.contains("RPM128"),
        "sles should flag RPM128; got {suse:?}"
    );
    assert!(
        suse.contains("RPM129"),
        "sles should flag RPM129; got {suse:?}"
    );
    assert!(
        !suse.contains("RPM127"),
        "sles should NOT flag RPM127; got {suse:?}"
    );

    // Cross-profile delta: the three sets must not all coincide.
    assert_ne!(rhel, alt, "rhel and alt must produce different RPM12x sets");
    assert_ne!(alt, suse, "alt and sles must produce different RPM12x sets");
}

// =====================================================================
// CLI --define / -D coverage (rpmbuild-style ad-hoc macro injection)
// =====================================================================

/// `profile macro NAME PROFILE -D 'NAME BODY'` must surface the
/// user-supplied value as if it were always defined in the profile,
/// with `[override]` provenance. End-to-end sanity that the flag wires
/// argv → ResolveOptions → Profile::macros → CLI lookup output.
#[test]
fn define_visible_via_profile_macro_lookup() {
    // `generic` profile is empty, so `make_build` is genuinely
    // undefined there; the CLI define must be the only source.
    let (code, stdout, stderr) = run(
        &[
            "profile",
            "macro",
            "-D",
            "make_build my-make-call",
            "make_build",
            "generic",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stdout.contains("make_build = my-make-call"),
        "stdout missing injected value: {stdout}"
    );
    assert!(
        stdout.contains("[override]"),
        "stdout missing override provenance: {stdout}"
    );
}

/// `--define` (long form) and `-D` (short form) must be interchangeable
/// — pins the rpmbuild-compatible alias so a future clap reorganisation
/// can't silently drop one.
#[test]
fn define_short_and_long_forms_equivalent() {
    let long = run(
        &["profile", "macro", "--define", "x 42", "x", "generic"],
        None,
    );
    let short = run(&["profile", "macro", "-D", "x 42", "x", "generic"], None);
    assert_eq!(long.0, 0, "long form exited non-zero, stderr={}", long.2);
    assert_eq!(short.0, 0, "short form exited non-zero, stderr={}", short.2);
    assert_eq!(
        long.1, short.1,
        "long and short forms produced different stdout"
    );
}

/// Multiple `-D` flags must all apply and be recorded as a single
/// `cli defines:` layer in `profile show`, preserving CLI order.
#[test]
fn define_multiple_accumulate_into_one_layer() {
    let (code, stdout, stderr) = run(
        &[
            "profile", "show", "generic", "-D", "first 1", "-D", "second 2", "-D", "third 3",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    // The layer line must list every name in CLI order.
    assert!(
        stdout.contains("cli defines: first, second, third"),
        "layer line missing or out of order in:\n{stdout}"
    );
    // And the macros must be in the registry.
    assert!(
        stdout.contains("macros:   3"),
        "macro count wrong: {stdout}"
    );
}

/// CLI `--define` is the last-applied layer; it must beat any
/// `[profiles.X.macros]` value supplied via `.rpmspec.toml`.
#[test]
fn define_overrides_config_macro() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg_path = tmp.path().join(".rpmspec.toml");
    std::fs::write(
        &cfg_path,
        r#"
profile = "X"

[profiles.X]
extends = "generic"

[profiles.X.macros]
flavour = "config-value"
"#,
    )
    .expect("write config");

    let cfg_arg = cfg_path.to_str().unwrap();

    // Sanity: without --define, the config value wins.
    let baseline = run(
        &["profile", "--config", cfg_arg, "macro", "flavour", "X"],
        None,
    );
    assert_eq!(baseline.0, 0, "baseline stderr={}", baseline.2);
    assert!(
        baseline.1.contains("flavour = config-value"),
        "baseline missing config value: {}",
        baseline.1
    );

    // With --define, CLI wins.
    let overridden = run(
        &[
            "profile",
            "--config",
            cfg_arg,
            "macro",
            "-D",
            "flavour cli-value",
            "flavour",
            "X",
        ],
        None,
    );
    assert_eq!(overridden.0, 0, "overridden stderr={}", overridden.2);
    assert!(
        overridden.1.contains("flavour = cli-value"),
        "CLI define did not override config: {}",
        overridden.1
    );
}

/// Malformed `--define` arguments must fail-fast with a non-zero exit
/// code and a stderr message that identifies the offending arg. We
/// cover the three documented failure modes (empty arg, missing
/// value, invalid name) and assert on the exit code + that stderr
/// names what went wrong.
#[test]
fn define_malformed_exits_with_clear_error() {
    let spec = write_temp(CLEAN_SPEC);
    let path = spec.path().to_str().unwrap();

    // Empty arg.
    let (code, _, stderr) = run(&["lint", "-D", "", path], None);
    assert_ne!(code, 0, "empty --define should fail; stderr={stderr}");
    assert!(
        stderr.to_lowercase().contains("empty"),
        "stderr should mention `empty`: {stderr}"
    );

    // No separator → no value.
    let (code, _, stderr) = run(&["lint", "-D", "loneword", path], None);
    assert_ne!(code, 0, "name-only --define should fail; stderr={stderr}");
    assert!(
        stderr.contains("loneword"),
        "stderr should name the offending arg: {stderr}"
    );
    assert!(
        stderr.to_lowercase().contains("missing value"),
        "stderr should mention `missing value`: {stderr}"
    );

    // Name contains `%` — invalid.
    let (code, _, stderr) = run(&["lint", "-D", "%bad value", path], None);
    assert_ne!(code, 0, "%-prefixed name should fail; stderr={stderr}");
    assert!(
        stderr.contains("%bad"),
        "stderr should name the offending name: {stderr}"
    );
    assert!(
        stderr.to_lowercase().contains("invalid"),
        "stderr should mention `invalid`: {stderr}"
    );
}

/// End-to-end demo that `--define` actually flows into the lint pass
/// and changes diagnostic output. Uses RPM050 hardcoded-paths whose
/// `set_profile` rebuilds its path-prefix table from `profile.macros`
/// via `MacroRegistry::expand_to_literal` — so defining `_libdir`
/// inserts a new `<literal-path> → %{_libdir}` row that hadn't been
/// in the fallback table.
///
/// Baseline (no `--define`): the bespoke `/opt/myorg/lib` path isn't
/// in `FALLBACK_PATH_TABLE`, RPM050 doesn't flag it.
/// With `-D '_libdir /opt/myorg/lib'`: same path is now mapped, RPM050
/// flags it and suggests `%{_libdir}`.
#[test]
fn define_extends_hardcoded_paths_table_at_lint_time() {
    // Reference the bespoke path from a place RPM050 actually scans —
    // %install body. Path appears at the start of a token so the
    // boundary check matches.
    let spec_src = "\
Name:           x
Version:        1
Release:        1
License:        MIT
Summary:        s
URL:            https://e.org

%description
b

%install
mkdir -p %{buildroot}/opt/myorg/lib
cp libfoo.so /opt/myorg/lib/

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
";
    let spec = write_temp(spec_src);
    let path = spec.path().to_str().unwrap();

    let ids_for = |extra: &[&str]| -> std::collections::BTreeSet<String> {
        let mut args = vec!["lint", "--format", "json", "--profile", "generic"];
        args.extend_from_slice(extra);
        args.push(path);
        let (_code, stdout, stderr) = run(&args, None);
        let v: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("invalid JSON: {e}\nstdout={stdout}\nstderr={stderr}"));
        let mut ids = std::collections::BTreeSet::new();
        for file in v["files"].as_array().into_iter().flatten() {
            for diag in file["diagnostics"].as_array().into_iter().flatten() {
                if let Some(id) = diag["lint_id"].as_str() {
                    ids.insert(id.to_string());
                }
            }
        }
        ids
    };

    let baseline = ids_for(&[]);
    let with_def = ids_for(&["-D", "_libdir /opt/myorg/lib"]);

    // RPM050 must NOT appear in the baseline — the bespoke path isn't
    // in the fallback table.
    assert!(
        !baseline.contains("RPM050"),
        "baseline should not flag /opt/myorg/lib without a matching macro; got {baseline:?}"
    );
    // …but MUST appear once the define teaches the rule about the path.
    assert!(
        with_def.contains("RPM050"),
        "--define '_libdir /opt/myorg/lib' should make RPM050 flag the path; got {with_def:?}"
    );
}

/// Compose `--define` with the existing family-gating mechanism — pins
/// that injecting a CLI macro doesn't perturb the per-distro diagnostic
/// sets that `cross_profile_lint_counts_differ_by_family` already
/// covers. Concretely: adding `-D 'dist .fc40'` to rhel/alt/sles must
/// preserve each profile's RPM127/128/129 polarity (CLI macros do NOT
/// override `profile.identity.dist_tag` — that's a deliberate
/// contract, since RPM127's Fedora-40 gate reads identity, not macros).
#[test]
fn define_composes_with_family_gating() {
    let spec_src = "\
Name:           compose-test
Version:        1
Release:        1
License:        GPLv2+
Summary:        s
URL:            https://e.org

%bcond_with python3

%description
b

%changelog
* Mon Jan 01 2024 a <a@b> - 1-1
- init
";
    let spec = write_temp(spec_src);
    let path = spec.path().to_str().unwrap();

    let ids_for = |profile: &str, extra: &[&str]| -> std::collections::BTreeSet<String> {
        let mut args = vec!["lint", "--format", "json", "--profile", profile];
        args.extend_from_slice(extra);
        args.push(path);
        let (_code, stdout, stderr) = run(&args, None);
        let v: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("invalid JSON: {e}\nstdout={stdout}\nstderr={stderr}"));
        let mut ids = std::collections::BTreeSet::new();
        for file in v["files"].as_array().into_iter().flatten() {
            for diag in file["diagnostics"].as_array().into_iter().flatten() {
                if let Some(id) = diag["lint_id"].as_str() {
                    ids.insert(id.to_string());
                }
            }
        }
        ids
    };

    // Baselines from the original cross-profile test.
    let rhel_base = ids_for("rhel-9-x86_64", &[]);
    let alt_base = ids_for("altlinux-10-x86_64", &[]);
    let suse_base = ids_for("sles-15-x86_64", &[]);

    // Adding an arbitrary CLI define must leave the polarity intact.
    let rhel_def = ids_for("rhel-9-x86_64", &["-D", "dist .fc40"]);
    let alt_def = ids_for("altlinux-10-x86_64", &["-D", "dist .fc40"]);
    let suse_def = ids_for("sles-15-x86_64", &["-D", "dist .fc40"]);

    // RPM127/128/129 polarity unchanged across the trio.
    for id in ["RPM127", "RPM128", "RPM129"] {
        assert_eq!(
            rhel_base.contains(id),
            rhel_def.contains(id),
            "rhel: {id} polarity should be unaffected by --define"
        );
        assert_eq!(
            alt_base.contains(id),
            alt_def.contains(id),
            "alt: {id} polarity should be unaffected by --define"
        );
        assert_eq!(
            suse_base.contains(id),
            suse_def.contains(id),
            "sles: {id} polarity should be unaffected by --define"
        );
    }
}

/// `profile macro NAME P -D 'NAME new'` must render two lines: the
/// winning value and a `  shadows: ...` line showing what the define
/// overwrote. Lets users debug "what did my define just replace?"
/// without re-running `profile macro` twice (with and without `-D`).
#[test]
fn define_shows_shadowed_value_in_profile_macro_output() {
    // `_vendor` is well-defined in `rhel-9-x86_64`'s bundled showrc
    // (literal "redhat"). Overriding it via -D produces a visible
    // shadow line referencing the original.
    let (code, stdout, stderr) = run(
        &[
            "profile",
            "macro",
            "-D",
            "_vendor acme-corp",
            "_vendor",
            "rhel-9-x86_64",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    // Winning value first, with override provenance.
    assert!(
        stdout.contains("_vendor = acme-corp  [override]"),
        "missing winning line: {stdout}"
    );
    // Shadow line: original showrc value with its showrc provenance.
    assert!(
        stdout.contains("  shadows: _vendor = redhat"),
        "missing shadow line: {stdout}"
    );
    assert!(
        stdout.contains("[showrc:"),
        "shadow line should carry original showrc provenance: {stdout}"
    );
}

/// When the user's `-D` value matches the baseline byte-for-byte (a
/// no-op override) the shadow line must be suppressed — printing
/// `shadows: NAME = same-value` would just be noise.
#[test]
fn define_with_identical_value_suppresses_shadow_line() {
    let (code, stdout, stderr) = run(
        &[
            "profile",
            "macro",
            "-D",
            "_vendor redhat",
            "_vendor",
            "rhel-9-x86_64",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("_vendor = redhat"), "stdout={stdout}");
    assert!(
        !stdout.contains("shadows:"),
        "identity override should NOT print a shadow line: {stdout}"
    );
}

/// `-D` adding a brand-new macro (one the profile didn't define at
/// all) must NOT print a shadow line — there's nothing to shadow.
#[test]
fn define_adding_new_macro_has_no_shadow() {
    let (code, stdout, stderr) = run(
        &[
            "profile",
            "macro",
            "-D",
            "brand_new_macro hello",
            "brand_new_macro",
            "rhel-9-x86_64",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stdout.contains("brand_new_macro = hello"),
        "stdout={stdout}"
    );
    assert!(
        !stdout.contains("shadows:"),
        "macro that didn't exist before -D should have no shadow: {stdout}"
    );
}

/// `profile show -D 'NAME VALUE'` must (a) include `cli defines: NAME`
/// in the layer trail, and (b) bump the macros count by one — both are
/// visible to users debugging "what does my define actually do?".
#[test]
fn define_visible_in_profile_show_layer_trail() {
    let (code, stdout, stderr) = run(
        &["profile", "show", "generic", "-D", "_topdir /opt/work"],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(
        stdout.contains("cli defines: _topdir"),
        "layer line missing in:\n{stdout}"
    );
    // Generic ships with 0 macros; the CLI define adds exactly one.
    assert!(
        stdout.contains("macros:   1"),
        "macro count should be 1: {stdout}"
    );
}

// =====================================================================
// `lints` subcommand
// =====================================================================

#[test]
fn lints_text_lists_known_rule_with_description() {
    let (code, stdout, stderr) = run(&["lints", "--color", "never"], None);
    assert_eq!(code, 0, "stderr={stderr}");
    // Category heading is present.
    assert!(stdout.contains("Correctness ("), "stdout:\n{stdout}");
    // A well-known rule shows up with its name and description.
    assert!(
        stdout.contains("RPM034"),
        "expected RPM034 row in output:\n{stdout}"
    );
    assert!(
        stdout.contains("obsolete-without-provides"),
        "expected rule name in output:\n{stdout}"
    );
}

#[test]
fn lints_markdown_renders_table_header_and_rows() {
    let (code, stdout, stderr) = run(&["lints", "--format", "markdown"], None);
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("# Lint rules reference"));
    assert!(stdout.contains("## Correctness"));
    assert!(stdout.contains("| ID | Name | Severity | Description |"));
    // Each rule's name is wrapped in backticks in the Name column.
    assert!(
        stdout.contains("| `obsolete-without-provides` |"),
        "expected RPM034 row in markdown:\n{stdout}"
    );
}

#[test]
fn lints_filter_by_severity_returns_only_deny_rules() {
    let (code, stdout, stderr) = run(&["lints", "--severity", "deny", "--color", "never"], None);
    assert_eq!(code, 0, "stderr={stderr}");
    // Every emitted rule line must end with the `deny` severity label
    // (followed by two spaces and the description).
    let row_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.trim_start().starts_with("RPM") || l.trim_start().starts_with("parse/"))
        .collect();
    assert!(!row_lines.is_empty(), "no rows in output:\n{stdout}");
    for line in &row_lines {
        assert!(
            line.contains("deny"),
            "expected deny severity on every row, got: {line}"
        );
        assert!(
            !line.contains("warn") && !line.contains("allow"),
            "row should not mention other severities: {line}"
        );
    }
}

#[test]
fn lints_filter_by_category_packaging_only() {
    let (code, stdout, stderr) = run(
        &["lints", "--category", "packaging", "--color", "never"],
        None,
    );
    assert_eq!(code, 0, "stderr={stderr}");
    assert!(stdout.contains("Packaging ("), "stdout:\n{stdout}");
    // Other category headings must not appear.
    assert!(!stdout.contains("Correctness ("), "stdout:\n{stdout}");
    assert!(!stdout.contains("Style ("), "stdout:\n{stdout}");
}

#[test]
fn lints_invalid_severity_value_exits_nonzero_with_clap_diag() {
    // Pin the clap-validated rejection of unknown severity values.
    // Replaces an earlier test that hedged on "empty filter combo"
    // and would self-disable as soon as the registry grew rules into
    // the chosen category × severity bucket.
    let (code, _stdout, stderr) = run(
        &[
            "lints",
            "--severity",
            "fatal", // not in {allow, warn, deny}
        ],
        None,
    );
    assert_ne!(code, 0);
    assert!(
        stderr.contains("invalid value 'fatal'") || stderr.contains("possible values"),
        "expected clap rejection in stderr, got:\n{stderr}"
    );
}

// ---------------------------------------------------------------------------
// matrix subcommand
// ---------------------------------------------------------------------------

mod matrix {
    use super::*;

    /// Spec that triggers a profile-specific rule (RPM125 requires the
    /// rhel family; generic stays silent on it). Used to verify that
    /// aggregation correctly attributes findings to the right subset
    /// of profiles.
    const PROFILE_DIFF_SPEC: &str = "\
Name:           foo
Version:        1.0
Release:        1%{?dist}
Summary:        Test package
License:        MIT
URL:            https://example.invalid/foo
Source0:        foo-1.0.tar.gz

%description
A test package.

%prep
%setup -q

%build
%configure
make %{?_smp_mflags}

%install
%make_install

%files
%license LICENSE
%{_bindir}/foo

%changelog
* Mon May 18 2026 Test <test@example.invalid> - 1.0-1
- init
";

    #[test]
    fn matrix_check_ad_hoc_profiles_lists_each_in_table() {
        // The matrix table renders one row per declared profile; the
        // order must match the --profiles CLI argument order. This
        // catches regressions where the resolver shuffles profiles
        // (alphabetical sort, hash-map iteration, …).
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let (code, stdout, stderr) = run(
            &[
                "matrix",
                "check",
                "--profiles",
                "generic,rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
        assert!(stdout.contains("`<ad-hoc>`"), "ad-hoc label missing");
        let generic_pos = stdout.find("\n  generic").expect("generic row");
        let rhel_pos = stdout.find("\n  rhel-9-x86_64").expect("rhel row");
        assert!(
            generic_pos < rhel_pos,
            "rows must follow --profiles order; got stdout=\n{stdout}"
        );
    }

    #[test]
    fn matrix_check_aggregates_profile_specific_finding() {
        // RPM125 (source-without-url) requires a Source0 entry that
        // isn't a URL. It's only flagged on the rhel family; generic
        // stays silent. The aggregated section must mark it as
        // affecting only rhel-9-x86_64, not generic.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let (code, stdout, _) = run(
            &[
                "matrix",
                "check",
                "--profiles",
                "generic,rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(code, 0);
        let pos = stdout
            .find("RPM125 source-without-url")
            .expect("RPM125 in output");
        let chunk = &stdout[pos..pos.min(stdout.len()).saturating_add(200).min(stdout.len())];
        assert!(
            chunk.contains("affected: rhel-9-x86_64"),
            "RPM125 must be attributed only to rhel; got chunk={chunk}"
        );
        assert!(
            !chunk.contains("affected: generic"),
            "generic must not be in the affected set for RPM125; got chunk={chunk}"
        );
    }

    #[test]
    fn matrix_check_from_target_set_in_config() {
        // Round-trip through `.rpmspec.toml`: target set declared in
        // the config is picked up by --target-set and produces the
        // same matrix view as the ad-hoc form.
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join(".rpmspec.toml");
        std::fs::write(
            &cfg,
            r#"
[targets.smoke]
profiles = ["generic", "rhel-9-x86_64"]
"#,
        )
        .expect("write config");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let (code, stdout, stderr) = run(
            &[
                "matrix",
                "check",
                "--config",
                cfg.to_str().unwrap(),
                "--target-set",
                "smoke",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(code, 0, "stderr={stderr}");
        assert!(stdout.contains("`smoke`"), "target set label missing");
        assert!(stdout.contains("rhel-9-x86_64"));
        assert!(stdout.contains("generic"));
    }

    #[test]
    fn matrix_check_unknown_target_set_exits_2() {
        // Missing config-defined target → friendly error to stderr,
        // exit 2 so CI distinguishes user error from lint failures.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let (code, _, stderr) = run(
            &[
                "matrix",
                "check",
                "--target-set",
                "no-such-target",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(code, 2, "stderr={stderr}");
        assert!(
            stderr.contains("no-such-target") && stderr.contains("not defined"),
            "expected friendly error mentioning the target name; got stderr={stderr}"
        );
    }

    #[test]
    fn matrix_check_requires_target_or_profiles_flag() {
        // ArgGroup contract: at least one of --target-set / --profiles
        // is required. clap returns non-zero with a usage message.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let (code, _, stderr) = run(&["matrix", "check", spec.to_str().unwrap()], None);
        assert_ne!(code, 0, "expected clap rejection; stderr={stderr}");
        assert!(
            stderr.contains("--target-set") || stderr.contains("--profiles"),
            "expected clap to mention the required flags; got stderr={stderr}"
        );
    }

    #[test]
    fn matrix_check_json_output_has_aggregated_and_profile_section() {
        // The JSON shape is part of the public contract; downstream
        // tooling keys off the `target_set`, `per_profile`, and
        // `aggregated` fields plus `affected_profiles` arrays.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let (code, stdout, stderr) = run(
            &[
                "matrix",
                "check",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(code, 0, "stderr={stderr}");
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));
        assert_eq!(parsed["target_set"], "<ad-hoc>");
        let profiles = parsed["profiles"].as_array().expect("profiles array");
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0], "generic");
        let files = parsed["files"].as_array().expect("files array");
        assert_eq!(files.len(), 1);
        assert!(files[0]["per_profile"].is_array());
        let aggregated = files[0]["aggregated"].as_array().expect("aggregated array");
        // RPM125 finding present with affected_profiles = [rhel-9-x86_64].
        let r125 = aggregated
            .iter()
            .find(|a| a["lint_id"] == "RPM125")
            .expect("RPM125 in aggregated");
        let affected = r125["affected_profiles"].as_array().unwrap();
        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0], "rhel-9-x86_64");
        // matrix_signature is the 16-hex-char form documented for Phase 1.
        let sig = r125["matrix_signature"].as_str().expect("sig string");
        assert_eq!(sig.len(), 16);
        assert!(
            sig.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
    }

    #[test]
    fn matrix_check_reads_from_stdin() {
        // matrix check should accept stdin the same way `lint` does
        // (path "-" reads stdin). The display name in output must be
        // `<stdin>` so consumers can tell sources apart.
        let (code, stdout, stderr) = run(
            &["matrix", "check", "--profiles", "generic", "-"],
            Some(PROFILE_DIFF_SPEC),
        );
        assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
        assert!(
            stdout.contains("<stdin>"),
            "expected <stdin> in output, got:\n{stdout}"
        );
    }

    #[test]
    fn matrix_baseline_create_emits_versioned_json_to_stdout() {
        // `matrix baseline create` without --out writes JSON to
        // stdout. The structure (baseline_version: 1, entries: [...])
        // is part of the public contract — pin it here.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let (code, stdout, stderr) = run(
            &[
                "matrix",
                "baseline",
                "create",
                "--profiles",
                "generic,rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(code, 0, "stderr={stderr}\nstdout={stdout}");
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));
        assert_eq!(parsed["baseline_version"], 1);
        let entries = parsed["entries"].as_array().expect("entries");
        assert!(!entries.is_empty(), "expected non-empty baseline");
        let first = &entries[0];
        assert!(first["matrix_signature"].is_string());
        assert!(first["lint_id"].is_string());
        assert!(first["message"].is_string());
        assert!(first["affected_profile_count"].is_number());
    }

    #[test]
    fn matrix_check_marks_baseline_entries_and_fail_on_new_passes() {
        // End-to-end baseline cycle: record current findings, then
        // re-check with the baseline — every aggregated entry must
        // be marked "[baseline]" and `--fail-on new` must exit 0.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let baseline = dir.path().join("base.json");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "baseline",
                "create",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--out",
                baseline.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "baseline create failed: stderr={stderr}");

        let (rc, stdout, _) = run(
            &[
                "matrix",
                "check",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--baseline",
                baseline.to_str().unwrap(),
                "--fail-on",
                "new",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "--fail-on new must pass when no new findings");
        // Every finding from the baseline cycle should be tagged.
        assert!(
            stdout.contains("[baseline]"),
            "expected [baseline] tags in output, got:\n{stdout}"
        );
    }

    #[test]
    fn matrix_check_fail_on_new_detects_regression() {
        // The core baseline gating contract: record a baseline on a
        // narrower spec, then introduce a new finding by adding a
        // spec line that fires a different rule. The added rule's
        // signature isn't in the baseline → --fail-on=new must exit 1.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        let baseline = dir.path().join("base.json");

        // Baseline spec — minimal valid form, fires RPM303 only
        // (missing `%{?dist}`). No Source line → no RPM125 on rhel.
        let baseline_spec = "\
Name:           foo
Version:        1.0
Release:        1
Summary:        Test package
License:        MIT
URL:            https://example.invalid/foo

%description
A test package.

%files
%license LICENSE

%changelog
* Mon May 18 2026 Test <test@example.invalid> - 1.0-1
- init
";
        std::fs::write(&spec, baseline_spec).expect("write baseline spec");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "baseline",
                "create",
                "--profiles",
                "rhel-9-x86_64",
                "--out",
                baseline.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "baseline create failed: stderr={stderr}");

        // Regression spec adds a Source: line whose value isn't a
        // URL — fires RPM125 on rhel, which was NOT in the baseline.
        let regressed_spec = baseline_spec.replace(
            "URL:            https://example.invalid/foo\n",
            "URL:            https://example.invalid/foo\nSource0:        foo-1.0.tar.gz\n",
        );
        std::fs::write(&spec, &regressed_spec).expect("rewrite spec");

        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "check",
                "--profiles",
                "rhel-9-x86_64",
                "--deny",
                "warnings",
                "--baseline",
                baseline.to_str().unwrap(),
                "--fail-on",
                "new",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(
            rc, 1,
            "expected exit 1 for new deny finding; stdout={stdout}\nstderr={stderr}"
        );
        // The new finding must be visible without the [baseline] tag.
        let r125_pos = stdout
            .find("RPM125")
            .unwrap_or_else(|| panic!("expected RPM125 in output:\n{stdout}"));
        let line_end = stdout[r125_pos..]
            .find('\n')
            .map(|n| r125_pos + n)
            .unwrap_or(stdout.len());
        assert!(
            !stdout[r125_pos..line_end].contains("[baseline]"),
            "new RPM125 finding must NOT be tagged [baseline]:\n{}",
            &stdout[r125_pos..line_end]
        );
    }

    #[test]
    fn matrix_check_baseline_with_invalid_signature_is_rejected() {
        // Hand-crafted baseline with a non-hex matrix_signature must
        // be rejected at load time — silently treating it as "no
        // entry matches" would break --fail-on new.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let baseline = dir.path().join("bad.json");
        std::fs::write(
            &baseline,
            r#"{
                "baseline_version": 1,
                "entries": [
                    {
                        "matrix_signature": "NOT-HEX",
                        "lint_id": "RPM055",
                        "message": "msg",
                        "affected_profile_count": 1
                    }
                ]
            }"#,
        )
        .expect("write baseline");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "check",
                "--profiles",
                "generic",
                "--baseline",
                baseline.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_ne!(rc, 0, "must reject malformed baseline");
        assert!(
            stderr.contains("invalid matrix signature"),
            "expected validation error in stderr; got {stderr}"
        );
    }

    #[test]
    fn matrix_check_baseline_with_unsupported_version_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let baseline = dir.path().join("future.json");
        std::fs::write(&baseline, r#"{ "baseline_version": 99, "entries": [] }"#)
            .expect("write baseline");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "check",
                "--profiles",
                "generic",
                "--baseline",
                baseline.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_ne!(rc, 0, "must reject future version baseline");
        assert!(
            stderr.contains("version 99") && stderr.contains("supports version 1"),
            "expected actionable version error; got {stderr}"
        );
    }

    #[test]
    fn matrix_check_fail_on_new_without_baseline_exits_2() {
        // --fail-on new without --baseline is a user-error: would
        // silently degrade to "every deny is new" otherwise.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "check",
                "--profiles",
                "generic",
                "--fail-on",
                "new",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2);
        assert!(
            stderr.contains("--fail-on new") && stderr.contains("--baseline"),
            "expected friendly error mentioning both flags; got stderr={stderr}"
        );
    }

    #[test]
    fn matrix_check_missing_baseline_file_exits_nonzero() {
        // ENOENT on --baseline path must surface as a hard error, not
        // a silent fall-back to an empty known-set. Defends against a
        // refactor that quietly treats a missing baseline as "nothing
        // is known yet" and lets CI pass on a stale config.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let missing = dir.path().join("does-not-exist.json");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "check",
                "--profiles",
                "generic",
                "--baseline",
                missing.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_ne!(rc, 0, "missing baseline must fail loudly");
        assert!(
            stderr.contains("opening baseline"),
            "expected open-failure context in stderr; got {stderr}"
        );
    }

    #[test]
    fn matrix_check_empty_baseline_treats_all_findings_as_new() {
        // An empty baseline asserts intent ("nothing is known-good")
        // and is semantically distinct from no baseline at all. With
        // --fail-on=new every deny finding becomes "new" → exit 1, and
        // no [baseline] tag may appear in the output.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let baseline = dir.path().join("empty.json");
        std::fs::write(&baseline, r#"{ "baseline_version": 1, "entries": [] }"#)
            .expect("write baseline");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "check",
                "--profiles",
                "rhel-9-x86_64",
                "--deny=warnings",
                "--baseline",
                baseline.to_str().unwrap(),
                "--fail-on",
                "new",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(
            rc, 1,
            "empty baseline + --fail-on=new must gate on any deny"
        );
        assert!(
            !stdout.contains("[baseline]"),
            "no entry should be marked as [baseline]-known; got stdout={stdout}"
        );
    }

    #[test]
    fn matrix_check_lint_and_lint_exit_codes_align_on_single_profile() {
        // Sanity contract: matrix check against a single profile must
        // not report deny when single-profile lint against the same
        // profile reports no deny. Catches accidental severity drift.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PROFILE_DIFF_SPEC).expect("write spec");
        let (lint_code, _, _) = run(
            &["lint", "--profile", "rhel-9-x86_64", spec.to_str().unwrap()],
            None,
        );
        let (matrix_code, _, _) = run(
            &[
                "matrix",
                "check",
                "--profiles",
                "rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        // Both should agree on "no deny" / "deny" — exit 0 in either
        // case for this spec, but the assertion checks the parity.
        assert_eq!(
            lint_code, matrix_code,
            "lint and matrix exit codes must agree on the same single-profile run"
        );
    }
}

// ---------------------------------------------------------------------------
// matrix portability
// ---------------------------------------------------------------------------

mod matrix_portability {
    use super::*;

    /// Spec deliberately mixes:
    /// * macros every distro profile defines (`_bindir`, `_unitdir`),
    /// * macros only some define (`dist` — RHEL has it, ALT doesn't),
    /// * a clearly-missing macro (`definitely_not_real`).
    const PORTABILITY_SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1%{?dist}
Summary: Test package

License: MIT
URL:     https://example.invalid/foo

%description
Body.

%files
%{_bindir}/foo
%{_unitdir}/foo.service
%{definitely_not_real}

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn portability_human_groups_by_status() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PORTABILITY_SPEC).expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "portability",
                "--profiles",
                "generic,rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "default fail-on=none must exit 0; stderr={stderr}");
        // Missing rows come first per status_rank ordering. Anchor
        // on the data row (status + ≥2 spaces + macro name) so a
        // format-width regression fails the test instead of being
        // hidden by a substring match on the summary line.
        let missing_pos = stdout
            .find("missing    definitely_not_real")
            .unwrap_or_else(|| panic!("missing data row not found:\n{stdout}"));
        let partial_pos = stdout
            .find("partial    _bindir")
            .unwrap_or_else(|| panic!("partial data row not found:\n{stdout}"));
        assert!(
            missing_pos < partial_pos,
            "missing entries must precede partial:\n{stdout}"
        );
    }

    #[test]
    fn portability_json_shape() {
        // JSON contract: target_set, profiles, files[].entries[]
        // with status enum strings.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PORTABILITY_SPEC).expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "portability",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        assert_eq!(parsed["target_set"], "<ad-hoc>");
        let files = parsed["files"].as_array().expect("files");
        assert_eq!(files.len(), 1);
        let entries = files[0]["entries"].as_array().expect("entries");
        // Each entry has a status from the documented enum.
        for e in entries {
            let s = e["status"].as_str().unwrap_or_default();
            assert!(
                matches!(s, "missing" | "partial" | "portable"),
                "unexpected status `{s}`"
            );
            assert!(e["defined_in"].is_array());
            assert!(e["missing_in"].is_array());
        }
    }

    #[test]
    fn portability_fail_on_partial_returns_1_when_only_partial_present() {
        // Build a spec that uses only macros defined on at least
        // one profile (so nothing is `missing`), but at least one
        // is `partial`. `generic` has no showrc bundle, so any
        // distro macro will be partial in [generic, rhel-9-x86_64].
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        let only_partial_spec = "\
Name:    foo
Version: 1.0
Release: 1
Summary: Test
License: MIT

%description
Body.

%files
%{_bindir}/foo

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        std::fs::write(&spec, only_partial_spec).expect("write spec");
        let (rc_partial, _, _) = run(
            &[
                "matrix",
                "portability",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--fail-on",
                "partial",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(
            rc_partial, 1,
            "--fail-on partial must catch partial-only spec"
        );

        // Same spec with --fail-on missing should pass: there are
        // no `missing` entries.
        let (rc_missing, stdout, stderr) = run(
            &[
                "matrix",
                "portability",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--fail-on",
                "missing",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(
            rc_missing, 0,
            "--fail-on missing must NOT trigger on partial-only spec; stderr={stderr}\nstdout={stdout}"
        );
    }

    #[test]
    fn portability_fail_on_missing_returns_1_when_missing_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PORTABILITY_SPEC).expect("write spec");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "portability",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--fail-on",
                "missing",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(
            rc, 1,
            "definitely_not_real must trigger fail-on=missing; stderr={stderr}"
        );
    }

    #[test]
    fn portability_requires_target_or_profiles_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PORTABILITY_SPEC).expect("write spec");
        let (rc, _, stderr) = run(&["matrix", "portability", spec.to_str().unwrap()], None);
        assert_ne!(rc, 0, "expected clap rejection; stderr={stderr}");
        assert!(
            stderr.contains("--target-set") || stderr.contains("--profiles"),
            "expected clap to mention the required flags; got {stderr}"
        );
    }

    #[test]
    fn portability_from_target_set_in_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cfg = dir.path().join(".rpmspec.toml");
        std::fs::write(
            &cfg,
            r#"
[targets.smoke]
profiles = ["generic", "rhel-9-x86_64"]
"#,
        )
        .expect("write config");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, PORTABILITY_SPEC).expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "portability",
                "--config",
                cfg.to_str().unwrap(),
                "--target-set",
                "smoke",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        assert!(stdout.contains("`smoke`"));
    }
}

// ---------------------------------------------------------------------------
// matrix coverage
// ---------------------------------------------------------------------------

mod matrix_coverage {
    use super::*;

    /// Spec mixes: an always-dead branch, a distro-only branch, and
    /// an architecture-only branch. Together they exercise the
    /// dead / partial / indeterminate paths of the evaluator.
    const COVERAGE_SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1%{?dist}
Summary: Test

License: MIT
URL:     https://example.invalid/foo

%if 0
Requires: never
%endif

%if 0%{?rhel}
Requires: rhel-only
%endif

%ifarch x86_64
Requires: arch-only
%endif

%description
B

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn coverage_human_marks_dead_and_active_branches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, COVERAGE_SPEC).expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "generic,rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "default fail-on=none must exit 0; stderr={stderr}");
        assert!(
            stdout.contains("[DEAD]"),
            "expected DEAD tag on `%if 0`; got:\n{stdout}"
        );
        assert!(
            stdout.contains("active: rhel-9-x86_64"),
            "expected rhel-only branch active on rhel; got:\n{stdout}"
        );
    }

    #[test]
    fn coverage_json_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, COVERAGE_SPEC).expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        assert_eq!(parsed["target_set"], "<ad-hoc>");
        let files = parsed["files"].as_array().expect("files");
        assert_eq!(files.len(), 1);
        let conditionals = files[0]["conditionals"].as_array().expect("conditionals");
        assert!(
            !conditionals.is_empty(),
            "expected non-empty conditionals array"
        );
        // Each branch has the documented schema.
        for c in conditionals {
            let branches = c["branches"].as_array().expect("branches");
            for b in branches {
                assert!(b["display"].is_string());
                assert!(b["is_dead"].is_boolean());
                assert!(b["active_on"].is_array());
                assert!(b["inactive_on"].is_array());
                assert!(b["indeterminate_on"].is_array());
            }
        }
    }

    #[test]
    fn coverage_fail_on_dead_returns_1_when_dead_present() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, COVERAGE_SPEC).expect("write spec");
        let (rc, _, _) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--fail-on",
                "dead",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 1, "`%if 0` must trigger fail-on=dead");
    }

    #[test]
    fn coverage_json_includes_indeterminate_reasons() {
        // Phase 4 contract: JSON output exposes per-profile reasons
        // for Indeterminate verdicts so dashboards can show "why".
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        let indet_spec = "\
Name:    foo
Version: 1.0
Release: 1
Summary: T

License: MIT

%if %{this_macro_is_genuinely_undefined}
Requires: nope
%endif

%description
B

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        std::fs::write(&spec, indet_spec).expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "generic",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}"));
        let branch = &parsed["files"][0]["conditionals"][0]["branches"][0];
        let reasons = branch["indeterminate_reasons"]
            .as_object()
            .expect("indeterminate_reasons object");
        let generic_reason = reasons["generic"].as_str().expect("generic reason");
        assert!(
            generic_reason.contains("not defined")
                || generic_reason.contains("undefined")
                || generic_reason.contains("genuinely_undefined"),
            "reason should explain why: {generic_reason}"
        );
    }

    #[test]
    fn coverage_fail_on_indeterminate_returns_1_when_only_indeterminate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        let only_indet_spec = "\
Name:    foo
Version: 1.0
Release: 1
Summary: T

License: MIT

%if %{this_macro_is_genuinely_undefined}
Requires: nope
%endif

%description
B

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        std::fs::write(&spec, only_indet_spec).expect("write spec");
        let (rc, _, _) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "generic",
                "--fail-on",
                "indeterminate",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(
            rc, 1,
            "indeterminate-only spec must fail with --fail-on indeterminate"
        );
    }

    #[test]
    fn coverage_requires_target_or_profiles_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, COVERAGE_SPEC).expect("write spec");
        let (rc, _, stderr) = run(&["matrix", "coverage", spec.to_str().unwrap()], None);
        assert_ne!(rc, 0);
        assert!(
            stderr.contains("--target-set") || stderr.contains("--profiles"),
            "expected clap to mention required flags; got {stderr}"
        );
    }

    #[test]
    fn coverage_define_expands_macros_inside_string_literals() {
        // Regression for a real-world false-positive: a CLI define
        // like `-D flavour ent` reaches `Profile.macros` (visible via
        // `matrix explain --macro flavour`) but the conditional
        // evaluator's string-literal path used to skip the macro
        // expansion step. `%if "%{flavour}" == "ent"` therefore
        // compared the literal byte string `%{flavour}` to `ent` and
        // mis-classified the branch as DEAD on every profile. Both
        // braced and unbraced forms exercise the same evaluator path,
        // so cover them in one test.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(
            &spec,
            "Name: foo\nVersion: 1\nRelease: 1\nSummary: t\nLicense: MIT\n\
             \n\
             %if \"%{flavour}\" == \"ent\"\n%global a 1\n%endif\n\
             %if \"%flavour\" == \"ent\"\n%global b 1\n%endif\n\
             %if \"%{flavour}\" == \"ent\" || \"%{flavour}\" == \"premium\"\n%global c 1\n%endif\n\
             \n\
             %description\nB\n\
             \n\
             %changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
        )
        .expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "coverage",
                "-D",
                "flavour ent",
                "--profiles",
                "rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        // None of the three branches may surface as DEAD now.
        assert!(
            !stdout.contains("[DEAD]"),
            "expected no [DEAD] tags with flavour=ent; got:\n{stdout}"
        );
        // All three should be ALWAYS active on the single profile.
        let always_count = stdout.matches("[ALWAYS]").count();
        assert_eq!(
            always_count, 3,
            "expected all 3 branches active; got [ALWAYS] count = {always_count} in:\n{stdout}"
        );
    }

    #[test]
    fn coverage_human_groups_indeterminate_by_two_distinct_reasons() {
        // Multi-reason path of write_branch (`by_reason.len() > 1`) is
        // separate from the single-reason fast path: it emits the
        // `indeterminate:` header on its own line and one indented
        // subline per reason. Each subline carries its own profile
        // list. Pin that two distinct reasons render as two
        // sublines, and that profiles are partitioned (not duplicated)
        // between them. A renderer refactor that merges both into
        // one bucket would silently lose information here.
        let dir = tempfile::tempdir().expect("tempdir");
        let configdir = dir.path();
        // First profile sees `foo_macro` undefined; second sees
        // `bar_macro` undefined. Without a custom profile we can't
        // engineer per-profile macro states, so use a config-defined
        // target set with per-profile defines.
        std::fs::write(
            configdir.join(".rpmspec.toml"),
            r#"
[targets.split]
profiles = ["rhel-9-x86_64", "rhel-9-aarch64"]

[targets.split.profile-overrides."rhel-9-x86_64"]
defines = { bar_macro = "1" }

[targets.split.profile-overrides."rhel-9-aarch64"]
defines = { foo_macro = "1" }
"#,
        )
        .expect("write config");
        let spec = configdir.join("foo.spec");
        std::fs::write(
            &spec,
            "Name: foo\nVersion: 1\nRelease: 1\nSummary: t\nLicense: MIT\n\
             \n\
             %if %{foo_macro} && %{bar_macro}\n%global both 1\n%endif\n\
             \n\
             %description\nB\n\
             \n\
             %changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
        )
        .expect("write spec");
        let config_path = configdir.join(".rpmspec.toml");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "--config",
                config_path.to_str().unwrap(),
                "coverage",
                "--target-set",
                "split",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        // The header form (multi-reason ladder) is used, not the
        // inline single-reason form: `indeterminate:` on its own line
        // followed by indented `(reason): profiles` sublines.
        assert!(
            stdout.contains("indeterminate:\n"),
            "expected multi-reason indeterminate header; got:\n{stdout}"
        );
        // Each reason group renders as a single subline matching
        // `(macro `NAME` is not defined`. Counting the leading paren
        // form pins one group per reason without false-matching the
        // reason text mentioned in other places (e.g. the condition
        // display itself).
        let foo_group = stdout.matches("(macro `foo_macro` is not defined").count();
        let bar_group = stdout.matches("(macro `bar_macro` is not defined").count();
        assert_eq!(foo_group, 1, "expected foo_macro group once; got {foo_group} in:\n{stdout}");
        assert_eq!(bar_group, 1, "expected bar_macro group once; got {bar_group} in:\n{stdout}");
    }

    #[test]
    fn coverage_json_indeterminate_groups_pivot_by_reason() {
        // Pin the new `indeterminate_groups` JSON field: array of
        // `{reason, profiles}`, sorted by reason text (BTreeMap
        // order), with profiles partitioned by reason. Skips
        // emission when no profile is indeterminate.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(
            &spec,
            "Name: foo\nVersion: 1\nRelease: 1\nSummary: t\nLicense: MIT\n\
             \n\
             %if %{undefined_macro_xyz} == 10\n%global a 1\n%endif\n\
             \n\
             %description\nB\n\
             \n\
             %changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
        )
        .expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "rhel-9-x86_64,rhel-9-aarch64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));
        let branches = v["files"][0]["conditionals"][0]["branches"][0].clone();
        let groups = branches["indeterminate_groups"].as_array().expect("groups");
        assert_eq!(groups.len(), 1, "expected one reason group; got: {groups:?}");
        let g = &groups[0];
        let reason = g["reason"].as_str().expect("reason");
        assert!(
            reason.contains("undefined_macro_xyz"),
            "expected reason mentioning the missing macro; got {reason}"
        );
        let profiles = g["profiles"].as_array().expect("profiles");
        assert_eq!(profiles.len(), 2, "expected both profiles in the group");
    }

    #[test]
    fn coverage_human_groups_indeterminate_by_reason() {
        // Readability regression: the human renderer used to print
        // the same indeterminate-reason once per profile, producing
        // walls of duplicated text on a 23-profile target set. After
        // the regroup change, a branch with the same reason across
        // every profile renders as one line with a `(all N profiles)`
        // summary. Pin both the grouping shape and the all-profiles
        // collapse so a renderer refactor doesn't regress either.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(
            &spec,
            "Name: foo\nVersion: 1\nRelease: 1\nSummary: t\nLicense: MIT\n\
             \n\
             %if %{undefined_macro_xyz} == 10\n%global a 1\n%endif\n\
             \n\
             %description\nB\n\
             \n\
             %changelog\n* Mon Jan 01 2024 a <a@b> - 1-1\n- init\n",
        )
        .expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "rhel-9-x86_64,rhel-9-aarch64,altlinux-10-x86_64,altlinux-10-aarch64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        // One indeterminate line, with the reason printed exactly once.
        let reason = "macro `undefined_macro_xyz` is not defined in the profile";
        let occurrences = stdout.matches(reason).count();
        assert_eq!(
            occurrences, 1,
            "expected reason to appear exactly once; got {occurrences} in:\n{stdout}"
        );
        // Four profiles in the set, all sharing the reason, must
        // collapse to the "(all 4 profiles)" summary form. The threshold
        // (4) is large enough to skip noise for small fixtures but kicks
        // in cleanly for production target-sets of 20+ profiles.
        assert!(
            stdout.contains("(all 4 profiles)"),
            "expected collapsed profile-count summary; got:\n{stdout}"
        );
    }
}

// ---------------------------------------------------------------------------
// matrix explain
// ---------------------------------------------------------------------------

mod matrix_explain {
    use super::*;

    /// Spec with one straightforward `%ifarch` (so the evaluator
    /// returns a clean active/inactive split) and one nested-style
    /// `%if 0%{?rhel}` (defined-on-rhel only). Avoids `>=` so we
    /// don't depend on the arithmetic-comparison evaluator path.
    const EXPLAIN_SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: Test
License: MIT

%if 0%{?rhel}
Requires: rhel-only
%endif

%ifarch x86_64
%global use_x86 1
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn explain_line_inside_ifarch_lists_active_profiles() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPLAIN_SPEC).expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--line",
                "12",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "explain must succeed; stderr={stderr}");
        assert!(
            stdout.contains("%ifarch x86_64"),
            "expected the ifarch directive in output; got:\n{stdout}"
        );
        assert!(
            stdout.contains("active:")
                && stdout.contains("rhel-9-x86_64")
                && stdout.contains("generic"),
            "both profiles must appear in active list; got:\n{stdout}"
        );
    }

    #[test]
    fn explain_line_outside_any_branch_reports_no_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPLAIN_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "generic",
                "--line",
                "1",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        assert!(
            stdout.contains("no enclosing"),
            "expected 'no enclosing branch' note; got:\n{stdout}"
        );
    }

    #[test]
    fn explain_macro_reports_defined_and_undefined() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPLAIN_SPEC).expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                "--macro",
                "_bindir",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        // Distro profiles populate _bindir from their showrc dump;
        // pin the canonical value so a profile refresh that changes
        // it surfaces here rather than in an unrelated lint test.
        assert!(
            stdout.contains("rhel-9-x86_64 = /usr/bin")
                && stdout.contains("altlinux-10-x86_64 = /usr/bin"),
            "expected /usr/bin on both profiles; got:\n{stdout}"
        );
    }

    #[test]
    fn explain_macro_reports_undefined() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPLAIN_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "generic",
                "--macro",
                "definitely_not_real_macro_xyz",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        assert!(
            stdout.contains("(undefined)"),
            "expected (undefined) tag for unknown macro; got:\n{stdout}"
        );
    }

    #[test]
    fn explain_json_line_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPLAIN_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "generic",
                "--line",
                "12",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        assert_eq!(v["query"], "line");
        assert_eq!(v["line"], 12);
        let branches = v["branches"].as_array().expect("branches");
        assert!(!branches.is_empty());
        // Schema: every branch carries the documented fields.
        let b = &branches[0];
        assert!(b["display"].is_string());
        assert!(b["is_dead"].is_boolean());
        assert!(b["is_universally_active"].is_boolean());
        assert!(b["active_on"].is_array());
        assert!(b["inactive_on"].is_array());
    }

    #[test]
    fn explain_json_macro_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPLAIN_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "rhel-9-x86_64",
                "--macro",
                "_bindir",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        assert_eq!(v["query"], "macro");
        assert_eq!(v["name"], "_bindir");
        let entries = v["entries"].as_array().expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["defined"], true);
        assert_eq!(entries[0]["value"], "/usr/bin");
    }

    #[test]
    fn explain_requires_line_or_macro() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPLAIN_SPEC).expect("write spec");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "generic",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_ne!(rc, 0, "explain without --line/--macro must fail");
        assert!(
            stderr.contains("--line") || stderr.contains("--macro"),
            "clap should mention the missing flag; got {stderr}"
        );
    }

    #[test]
    fn explain_line_and_macro_are_mutually_exclusive() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPLAIN_SPEC).expect("write spec");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "generic",
                "--line",
                "12",
                "--macro",
                "_bindir",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_ne!(rc, 0, "passing both --line and --macro must fail");
        assert!(
            stderr.contains("cannot be used with") || stderr.contains("conflict"),
            "clap should reject the conflict; got {stderr}"
        );
    }

    #[test]
    fn explain_unknown_target_set_exits_2() {
        // resolve_matrix_source must surface unknown-target-set as a
        // user-error (exit 2) rather than a panic or generic anyhow
        // dump. Mirrors matrix_check_unknown_target_set_exits_2 so a
        // regression in shared resolver error propagation is caught
        // for explain too.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPLAIN_SPEC).expect("write spec");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "explain",
                "--target-set",
                "nonexistent-set",
                "--macro",
                "_bindir",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2, "unknown target set must exit 2; stderr={stderr}");
        assert!(
            stderr.contains("nonexistent-set"),
            "expected target-set name in error; got {stderr}"
        );
    }

    #[test]
    fn explain_macro_unexpandable_reason_surfaces() {
        // `make_build` in rhel-9-x86_64 has a body referencing
        // `%{?_smp_mflags}` (conditional) — `expand_to_literal` bails
        // on conditional refs, so we hit the `Unexpandable` arm.
        // Exercises both the human "(defined but ...)" tag and the
        // JSON `unexpandable_reason` field.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPLAIN_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "rhel-9-x86_64",
                "--macro",
                "make_build",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        let entries = v["entries"].as_array().expect("entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0]["defined"], true,
            "make_build is registered on rhel-9"
        );
        assert!(
            entries[0]["value"].is_null(),
            "value must be null for unexpandable macros; got {}",
            entries[0]
        );
        assert!(
            entries[0]["unexpandable_reason"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "expected non-empty unexpandable_reason; got {}",
            entries[0]
        );
    }

    #[test]
    fn explain_json_line_indeterminate_reasons_shape() {
        // `%if 0%{?rhel} >= 8` lands in the arithmetic-Raw path of
        // the evaluator → produces an `indeterminate_on` entry with
        // a populated `indeterminate_reasons` map. Exercises the
        // BTreeMap-as-object serialization shape that the explain
        // human renderer also depends on.
        const ARITH_SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: Test
License: MIT

%if 0%{?rhel} >= 8
BuildRequires: rhel-only
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, ARITH_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "rhel-9-x86_64",
                "--line",
                "8",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        let branches = v["branches"].as_array().expect("branches");
        assert!(!branches.is_empty(), "expected at least one branch");
        let b = &branches[0];
        let inds = b["indeterminate_on"].as_array().expect("indeterminate_on");
        assert!(
            !inds.is_empty(),
            "arithmetic Raw branch must be indeterminate on the profile"
        );
        let reasons = b["indeterminate_reasons"]
            .as_object()
            .expect("indeterminate_reasons must be a JSON object");
        // Map key is profile_id; value is the human reason string.
        let reason = reasons
            .get("rhel-9-x86_64")
            .and_then(|v| v.as_str())
            .expect("rhel-9-x86_64 must appear in indeterminate_reasons");
        assert!(
            !reason.is_empty(),
            "indeterminate reason must be non-empty; got {reason:?}"
        );
    }

    #[test]
    fn explain_line_surfaces_parser_diagnostics_for_broken_spec() {
        // Spec missing %endif — the parser recovers but emits a
        // diagnostic. Without the up-front banner the user would see
        // "(no enclosing branch covers this line)" and have no clue
        // the AST is partial. Pin the banner so a refactor that
        // silently drops it surfaces here.
        const BROKEN_SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: Test
License: MIT

%if 0%{?rhel}
BuildRequires: rhel-only
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BROKEN_SPEC).expect("write spec");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "rhel-9-x86_64",
                "--line",
                "8",
                spec.to_str().unwrap(),
            ],
            None,
        );
        // Explain doesn't abort on parser issues — it produces a
        // best-effort report — but it must announce the degraded
        // state on stderr.
        assert_eq!(rc, 0);
        assert!(
            stderr.contains("parser diagnostic") || stderr.contains("recovered AST"),
            "expected parser-diagnostic banner; got stderr={stderr:?}"
        );
    }

    #[test]
    fn explain_rejects_multiple_input_specs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a.spec");
        let b = dir.path().join("b.spec");
        std::fs::write(&a, EXPLAIN_SPEC).expect("write a");
        std::fs::write(&b, EXPLAIN_SPEC).expect("write b");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "generic",
                "--macro",
                "_bindir",
                a.to_str().unwrap(),
                b.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2, "multiple specs must be rejected; stderr={stderr}");
        assert!(
            stderr.contains("exactly one spec"),
            "expected friendly multi-spec error; got {stderr}"
        );
    }
}

// ---------------------------------------------------------------------------
// matrix bcond (--with / --without flags)
// ---------------------------------------------------------------------------

mod matrix_bcond {
    use super::*;

    /// Spec gating BR on `%{with bootstrap}` (default off) and
    /// `%{without docs}` (default on). End-to-end tests verify the
    /// CLI flags flip these states correctly and the resulting
    /// branch activity matches expectation.
    const BCOND_SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
%bcond_with bootstrap
%bcond_without docs

%if %{with bootstrap}
BuildRequires: bootstrap-pkg
%endif

%if %{with docs}
BuildRequires: doc-pkg
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn coverage_bcond_with_defaults_to_inactive() {
        // `%bcond_with bootstrap` — default off. Without `--with`,
        // `%{with bootstrap}` resolves to 0, so the branch is
        // Inactive on every profile (NOT Indeterminate as it was
        // before Phase 10).
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BCOND_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        // The %if %{with bootstrap} line should be Inactive on rhel.
        // Find the "%if %{with bootstrap}" entry and confirm it
        // resolves to Inactive (not Indeterminate).
        assert!(
            stdout.contains("%if %{with bootstrap}"),
            "expected the bcond %if line; got:\n{stdout}"
        );
        // Pre-Phase-10 this would show "indeterminate: rhel-9-x86_64".
        // Now it must show "inactive: rhel-9-x86_64" instead.
        let bootstrap_idx = stdout
            .find("%if %{with bootstrap}")
            .expect("bootstrap line");
        // Look at the next ~150 chars after the directive — that's
        // the activity block.
        let tail = &stdout[bootstrap_idx..(bootstrap_idx + 200).min(stdout.len())];
        assert!(
            tail.contains("inactive: rhel-9-x86_64"),
            "%bcond_with default-off must resolve to Inactive; got tail:\n{tail}"
        );
    }

    #[test]
    fn coverage_with_flag_flips_bcond_to_active() {
        // `--with bootstrap` flips the default-off bcond to
        // active → `%{with bootstrap}` resolves to 1 → branch
        // becomes Active on every profile.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BCOND_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "rhel-9-x86_64",
                "--with",
                "bootstrap",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let bootstrap_idx = stdout
            .find("%if %{with bootstrap}")
            .expect("bootstrap line");
        let tail = &stdout[bootstrap_idx..(bootstrap_idx + 200).min(stdout.len())];
        assert!(
            tail.contains("active: rhel-9-x86_64"),
            "--with FOO must flip the bcond to Active; got tail:\n{tail}"
        );
    }

    #[test]
    fn coverage_without_flag_flips_bcond_without_to_inactive() {
        // `%bcond_without docs` — default ON. `--without docs` flips
        // it OFF.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BCOND_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "rhel-9-x86_64",
                "--without",
                "docs",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        // `%if %{with docs}` in the spec should now resolve to
        // Inactive (since --without docs turns off the bcond).
        let docs_idx = stdout.find("%if %{with docs}").expect("docs line");
        let tail = &stdout[docs_idx..(docs_idx + 200).min(stdout.len())];
        assert!(
            tail.contains("inactive: rhel-9-x86_64"),
            "--without DOCS must flip the default-ON bcond Inactive; got tail:\n{tail}"
        );
    }

    #[test]
    fn diff_bcond_affects_only_a_only_b() {
        // Two profiles A and B; --with bootstrap (which would make
        // the bcond Active on BOTH) ⇒ bootstrap-pkg appears in
        // `common` for both. Without the flag it appears nowhere.
        // This locks the CLI integration end-to-end.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BCOND_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "diff",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                "--with",
                "bootstrap",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        let br = &v["groups"][0];
        let common: Vec<&str> = br["common"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert!(
            common.contains(&"bootstrap-pkg"),
            "--with bootstrap must surface bootstrap-pkg in common; got common={common:?}"
        );
    }

    #[test]
    fn explain_line_inside_bcond_branch_reports_inactive_without_with_flag() {
        // `%if %{with bootstrap}` defaults off (`%bcond_with`).
        // `matrix explain --line N` where N is inside the body
        // must show the enclosing branch as Inactive on every
        // profile — confirming the bcond pipeline reaches the
        // explain command, not just coverage.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BCOND_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "explain",
                "--profiles",
                "rhel-9-x86_64",
                "--line",
                "10",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        assert!(
            stdout.contains("%if %{with bootstrap}"),
            "expected the bootstrap-gated branch in output; got:\n{stdout}"
        );
        // The branch must NOT be indeterminate (which is what
        // pre-Phase-10 would have shown). It must be inactive on
        // rhel-9-x86_64.
        assert!(
            stdout.contains("inactive:"),
            "branch must resolve to inactive without --with; got:\n{stdout}"
        );
    }

    #[test]
    fn expand_bcond_branch_tagged_inactive_by_default() {
        // `matrix expand` must mark the bootstrap-gated %if as
        // [INACTIVE] when no --with flag is supplied (default off
        // per %bcond_with).
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BCOND_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "expand",
                "--profiles",
                "rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        assert!(
            stdout.contains("%if %{with bootstrap}  [INACTIVE]"),
            "bootstrap %if must be [INACTIVE] by default; got:\n{stdout}"
        );
    }

    #[test]
    fn coverage_marks_bcond_expr_default_as_indeterminate() {
        // `%bcond pcre2 %[...]` (rpm ≥ 4.17.1) — non-literal default
        // cannot be statically evaluated. Without `--with pcre2`,
        // `%if %{with pcre2}` MUST surface as Indeterminate (not
        // silently false). Coverage output exposes the actionable
        // error message from the evaluator.
        const SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
%bcond pcre2 %[0%{?fedora} > 35]

%if %{with pcre2}
BuildRequires: pcre2-devel
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "rhel-9-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        // Walk the conditional/branch tree to find the %{with pcre2}
        // branch and confirm it is Indeterminate on rhel-9-x86_64.
        let files = v["files"].as_array().expect("files");
        let conditionals = files[0]["conditionals"].as_array().expect("conditionals");
        let mut found_indeterminate = false;
        for c in conditionals {
            for b in c["branches"].as_array().unwrap() {
                let display = b["display"].as_str().unwrap_or("");
                if display.contains("%{with pcre2}") {
                    let inds = b["indeterminate_on"].as_array().unwrap();
                    assert!(
                        inds.iter().any(|p| p == "rhel-9-x86_64"),
                        "pcre2 branch must be indeterminate on rhel-9; got {inds:?}"
                    );
                    let reason = b["indeterminate_reasons"]["rhel-9-x86_64"]
                        .as_str()
                        .expect("reason present");
                    assert!(
                        reason.contains("bcond default expression") && reason.contains("--with"),
                        "expected actionable bcond message; got {reason:?}"
                    );
                    found_indeterminate = true;
                }
            }
        }
        assert!(
            found_indeterminate,
            "did not find pcre2 branch in coverage report"
        );
    }

    #[test]
    fn coverage_bcond_expr_default_with_flag_resolves_to_active() {
        // Same spec as above, but `--with pcre2` provides a concrete
        // state — branch must become Active, NOT Indeterminate.
        const SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
%bcond pcre2 %[0%{?fedora} > 35]

%if %{with pcre2}
BuildRequires: pcre2-devel
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "rhel-9-x86_64",
                "--with",
                "pcre2",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        // Find the line and ensure it's active.
        let idx = stdout.find("%{with pcre2}").expect("branch present");
        let tail = &stdout[idx..(idx + 200).min(stdout.len())];
        assert!(
            tail.contains("active: rhel-9-x86_64"),
            "--with pcre2 must collapse Unevaluated → Active; got tail:\n{tail}"
        );
    }

    #[test]
    fn conflicting_with_without_emits_stderr_warning() {
        // --with FOO --without FOO is a usage error in spirit but
        // RPM accepts and picks one. We warn loudly on stderr so the
        // operator sees the silent precedence rule (--with wins).
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BCOND_SPEC).expect("write spec");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "coverage",
                "--profiles",
                "rhel-9-x86_64",
                "--with",
                "bootstrap",
                "--without",
                "bootstrap",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        assert!(
            stderr.contains("--with and --without both specified") && stderr.contains("bootstrap"),
            "expected conflict warning naming `bootstrap`; got stderr:\n{stderr}"
        );
    }

    #[test]
    fn verify_contract_honours_with_flag() {
        // Contract demands `bootstrap-pkg` on rhel. Without --with,
        // bootstrap bcond is off → bootstrap-pkg not collected →
        // contract fails. With --with bootstrap → bootstrap-pkg
        // collected → contract passes.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BCOND_SPEC).expect("write spec");
        let contract = dir.path().join("contract.toml");
        std::fs::write(
            &contract,
            r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["bootstrap-pkg"]
"#,
        )
        .expect("write contract");
        // Without --with: fails.
        let (rc1, _, _) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc1, 1, "default bcond off must fail must_have");
        // With --with bootstrap: passes.
        let (rc2, _, _) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                "--with",
                "bootstrap",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc2, 0, "--with bootstrap must satisfy must_have");
    }
}

// ---------------------------------------------------------------------------
// matrix classes
// ---------------------------------------------------------------------------

mod matrix_classes {
    use super::*;

    const CLASSES_SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: Test
License: MIT
BuildRequires: gcc

%if 0%{?rhel}
BuildRequires: rhel-only
%endif

%if 0%{?suse_version}
BuildRequires: suse-only
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn classes_groups_rhel_family_together() {
        // 4 profiles: 2 RHEL, 1 alt, 1 SLES.
        // Expected: 3 classes (RHEL has 2 members).
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, CLASSES_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "classes",
                "--profiles",
                "rhel-9-x86_64,rhel-8-x86_64,altlinux-10-x86_64,sles-15-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        assert!(
            stdout.contains("4 profiles → 3 class(es)"),
            "expected 4 → 3 collapse; got:\n{stdout}"
        );
        // The RHEL class is the largest (2 members) so sorts first.
        assert!(
            stdout.contains("## Class 1 (2 members,"),
            "first class must have 2 members; got:\n{stdout}"
        );
    }

    #[test]
    fn classes_recommended_build_set_size_equals_class_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, CLASSES_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "classes",
                "--profiles",
                "rhel-9-x86_64,rhel-8-x86_64,altlinux-10-x86_64,sles-15-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        // Minimal representative build set must list exactly 3 profiles
        // (one per class).
        let build_set_idx = stdout
            .find("Minimal representative build set")
            .expect("build-set section");
        let tail = &stdout[build_set_idx..];
        assert!(
            tail.contains("rhel-8-x86_64")
                && tail.contains("altlinux-10-x86_64")
                && tail.contains("sles-15-x86_64"),
            "build set must list one representative per class; got:\n{tail}"
        );
    }

    #[test]
    fn classes_all_identical_profiles_collapse_to_one() {
        // Spec has no conditionals → every profile is equivalent
        // → 1 class containing all 4.
        const FLAT: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, FLAT).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "classes",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64,sles-15-x86_64,generic",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        assert!(
            stdout.contains("4 profiles → 1 class(es)"),
            "no conditionals → 1 class; got:\n{stdout}"
        );
    }

    #[test]
    fn classes_json_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, CLASSES_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "classes",
                "--profiles",
                "rhel-9-x86_64,rhel-8-x86_64,altlinux-10-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        // Envelope shape.
        assert_eq!(v["target_set"], "<ad-hoc>");
        assert_eq!(v["class_count"], 2);
        let classes = v["classes"].as_array().expect("classes");
        assert_eq!(classes.len(), 2);
        // First class is the larger (2 RHEL members).
        assert_eq!(classes[0]["members"].as_array().unwrap().len(), 2);
        // Representative is alphabetically first member.
        assert_eq!(classes[0]["representative"], "rhel-8-x86_64");
        // Representatives array mirrors class order.
        let reps = v["representatives"].as_array().expect("reps");
        assert_eq!(reps.len(), 2);
        assert_eq!(reps[0], "rhel-8-x86_64");
    }

    #[test]
    fn classes_rejects_multiple_specs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a.spec");
        let b = dir.path().join("b.spec");
        std::fs::write(&a, CLASSES_SPEC).expect("a");
        std::fs::write(&b, CLASSES_SPEC).expect("b");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "classes",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                a.to_str().unwrap(),
                b.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2);
        assert!(
            stderr.contains("exactly one spec"),
            "expected multi-spec rejection; got {stderr}"
        );
    }

    #[test]
    fn classes_requires_target_or_profiles_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, CLASSES_SPEC).expect("write spec");
        let (rc, _, stderr) = run(&["matrix", "classes", spec.to_str().unwrap()], None);
        assert_ne!(rc, 0);
        assert!(
            stderr.contains("--target-set") || stderr.contains("--profiles"),
            "expected clap to mention required flags; got {stderr}"
        );
    }

    #[test]
    fn classes_honours_with_flag_for_bcond() {
        // Verify the bcond flag actually affects the signature. We
        // capture the JSON signature hex from two runs (with vs
        // without `--with`) and assert it changed — a regression
        // where `--with` is ignored would produce identical hexes.
        const BCOND: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
%bcond_with bootstrap
BuildRequires: gcc

%if %{with bootstrap}
BuildRequires: bootstrap-pkg
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BCOND).expect("write spec");
        let extract_sig = |stdout: &str| -> String {
            let v: serde_json::Value = serde_json::from_str(stdout).expect("json");
            v["classes"][0]["signature"]
                .as_str()
                .expect("signature str")
                .to_string()
        };
        let (_, stdout1, _) = run(
            &[
                "matrix",
                "classes",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        let sig_off = extract_sig(&stdout1);
        let (_, stdout2, _) = run(
            &[
                "matrix",
                "classes",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                "--with",
                "bootstrap",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        let sig_on = extract_sig(&stdout2);
        assert_ne!(
            sig_off, sig_on,
            "--with bootstrap must change the signature (off={sig_off}, on={sig_on})"
        );
        // Sanity: with --with bootstrap, bootstrap-pkg is in deps.
        assert!(
            stdout2.contains("bootstrap-pkg"),
            "--with bootstrap must surface bootstrap-pkg; got:\n{stdout2}"
        );
    }

    #[test]
    fn classes_surfaces_parser_diagnostics_for_broken_spec() {
        // Broken spec (missing %endif) produces parser diagnostics;
        // CLI must warn on stderr like every sibling matrix command.
        const BROKEN: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT

%if 0%{?rhel}
BuildRequires: gcc
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BROKEN).expect("write spec");
        let (_rc, _, stderr) = run(
            &[
                "matrix",
                "classes",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert!(
            stderr.contains("parser diagnostic") || stderr.contains("recovered AST"),
            "expected parser-diagnostic banner; got stderr={stderr:?}"
        );
    }

    #[test]
    fn classes_human_output_pins_canonical_row_layout() {
        // Lock the human format so operator tooling that greps
        // "representative:" / "members:" / "BuildRequires (n):" keeps
        // working. A field-label rename would silently break it.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, CLASSES_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "classes",
                "--profiles",
                "rhel-9-x86_64,rhel-8-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        // Header line.
        assert!(
            stdout.contains("# Matrix classes: target set"),
            "missing canonical header; got:\n{stdout}"
        );
        // Field-label lines.
        assert!(
            stdout.contains("representative: rhel-8-x86_64"),
            "missing canonical `representative:` line; got:\n{stdout}"
        );
        assert!(
            stdout.contains("members:"),
            "missing canonical `members:` label; got:\n{stdout}"
        );
        // Per-tag dep listing format `BuildRequires (n): ...`.
        assert!(
            stdout.contains("BuildRequires ("),
            "missing canonical per-tag header; got:\n{stdout}"
        );
        // Minimal build set section.
        assert!(
            stdout.contains("## Minimal representative build set"),
            "missing build-set section; got:\n{stdout}"
        );
    }

    #[test]
    fn classes_invariant_with_diff_equivalent_class_has_empty_only_buckets() {
        // Cross-cutting invariant: profiles in the same class produce
        // empty `only_a`/`only_b` buckets in `matrix diff`. Both
        // commands rely on the same dep_walk + Skip-policy primitives;
        // a refactor that desyncs them must surface here.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, CLASSES_SPEC).expect("write spec");
        // Two rhel-family profiles share a class (both pull rhel-only).
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "diff",
                "--profiles",
                "rhel-8-x86_64,rhel-9-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        for group in v["groups"].as_array().expect("groups") {
            let only_a = group["only_a"].as_array().unwrap();
            let only_b = group["only_b"].as_array().unwrap();
            assert!(
                only_a.is_empty() && only_b.is_empty(),
                "in-class diff must have empty only-A/only-B; got group={group}"
            );
        }
    }

    #[test]
    fn classes_json_dep_bucket_uses_tag_field_not_tag_label() {
        // The JSON wire shape must serialise the bucket label under
        // the key "tag" (matching `matrix diff`'s TagDiffJson), even
        // though the Rust field name is `tag_label`. Downstream
        // tooling that read either command's JSON will see the same
        // schema for the per-tag bucket envelope.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, CLASSES_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "classes",
                "--profiles",
                "rhel-9-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        let bucket = &v["classes"][0]["deps_by_tag"][0];
        // Field name on the wire is "tag", not "tag_label".
        assert!(
            bucket.get("tag").is_some(),
            "DepBucket JSON must serialise under `tag`, not `tag_label`; got {bucket}"
        );
        assert!(
            bucket.get("tag_label").is_none(),
            "DepBucket JSON must NOT have a `tag_label` field; got {bucket}"
        );
        assert_eq!(bucket["tag"], "BuildRequires");
    }

    #[test]
    fn classes_indeterminate_branch_skip_drops_dep() {
        // Arithmetic Raw `%if 0%{?rhel} >= 8` is Indeterminate under
        // Skip policy → BR inside contributes to neither class →
        // profiles with that arithmetic see no extra dep. Two
        // profiles where the indeterminate branch is the only
        // difference must therefore land in the same class.
        const ARITH: &str = "\
Name: foo
Version: 1
Release: 1
Summary: t
License: MIT
BuildRequires: gcc

%if 0%{?rhel} >= 8
BuildRequires: maybe-rhel
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, ARITH).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "classes",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        assert_eq!(
            v["class_count"], 1,
            "Skip policy collapses indeterminate-only differences"
        );
        // `maybe-rhel` must not appear in any class's BR bucket.
        let stringy = stdout.clone();
        assert!(
            !stringy.contains("maybe-rhel"),
            "indeterminate-branch dep must be hidden under Skip policy; got:\n{stringy}"
        );
    }
}

// ---------------------------------------------------------------------------
// matrix expand
// ---------------------------------------------------------------------------

mod matrix_expand {
    use super::*;

    /// Spec mixes a literal `%if 0%{?rhel}` (active on rhel,
    /// inactive on alt) with `%ifarch x86_64` (active on both
    /// x86_64 profiles). Avoids `>=` arithmetic to keep the
    /// evaluator on a deterministic path.
    const EXPAND_SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: Test
License: MIT
BuildRequires: gcc

%if 0%{?rhel}
BuildRequires: rhel-pkg
%endif

%ifarch x86_64
%global use_x86 1
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn expand_human_tags_active_and_inactive_branches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPAND_SPEC).expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "expand",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "expand must succeed; stderr={stderr}");
        // rhel-9 section: %if 0%{?rhel} active
        assert!(
            stdout.contains("== Profile rhel-9-x86_64 =="),
            "expected profile header; got:\n{stdout}"
        );
        let rhel_section_start = stdout.find("== Profile rhel-9-x86_64 ==").unwrap();
        let alt_section_start = stdout.find("== Profile altlinux-10-x86_64 ==").unwrap();
        let rhel_section = &stdout[rhel_section_start..alt_section_start];
        let alt_section = &stdout[alt_section_start..];
        assert!(
            rhel_section.contains("%if 0%{?rhel}  [ACTIVE]"),
            "rhel section must mark %if as ACTIVE; got:\n{rhel_section}"
        );
        assert!(
            alt_section.contains("%if 0%{?rhel}  [INACTIVE]"),
            "alt section must mark %if as INACTIVE; got:\n{alt_section}"
        );
    }

    #[test]
    fn expand_json_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPAND_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "expand",
                "--profiles",
                "rhel-9-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        assert_eq!(v["target_set"], "<ad-hoc>");
        let pp = v["per_profile"].as_array().expect("per_profile");
        assert_eq!(pp.len(), 1);
        let branches = pp[0]["branches"].as_array().expect("branches");
        assert_eq!(branches.len(), 2, "expected %if + %ifarch entries");
        // Branch entries are sorted by line ascending.
        assert!(branches[0]["line"].as_u64().unwrap() < branches[1]["line"].as_u64().unwrap());
        // Status discriminator is snake_case.
        for b in branches {
            let status = b["status"].as_str().expect("status");
            assert!(
                matches!(status, "active" | "inactive" | "indeterminate"),
                "unexpected status {status:?}"
            );
        }
    }

    #[test]
    fn expand_json_carries_indeterminate_reason() {
        // Arithmetic-comparison Raw form `>=` lands in the
        // Indeterminate path of the evaluator. Pin the reason
        // serialisation so the JSON wire format stays stable.
        const ARITH: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: t
License: MIT

%if 0%{?rhel} >= 8
BuildRequires: gcc
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, ARITH).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "expand",
                "--profiles",
                "rhel-9-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        let branch = &v["per_profile"][0]["branches"][0];
        assert_eq!(branch["status"], "indeterminate");
        assert!(
            branch["indeterminate_reason"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "expected non-empty indeterminate_reason; got {branch}"
        );
    }

    #[test]
    fn expand_rejects_multiple_specs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a.spec");
        let b = dir.path().join("b.spec");
        std::fs::write(&a, EXPAND_SPEC).expect("a");
        std::fs::write(&b, EXPAND_SPEC).expect("b");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "expand",
                "--profiles",
                "rhel-9-x86_64",
                a.to_str().unwrap(),
                b.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2);
        assert!(
            stderr.contains("exactly one spec"),
            "expected multi-spec error; got {stderr}"
        );
    }

    #[test]
    fn expand_requires_target_or_profiles_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPAND_SPEC).expect("write spec");
        let (rc, _, stderr) = run(&["matrix", "expand", spec.to_str().unwrap()], None);
        assert_ne!(rc, 0);
        assert!(
            stderr.contains("--target-set") || stderr.contains("--profiles"),
            "expected clap to mention required flags; got {stderr}"
        );
    }

    #[test]
    fn expand_preserves_unexpanded_macros_in_body() {
        // Doc claim: "macros are NOT expanded" — body text passes
        // through verbatim. Pin so a future renderer "helpfully
        // expand for the reader" change surfaces here.
        const SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: %{name}-devel

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "expand",
                "--profiles",
                "rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        assert!(
            stdout.contains("BuildRequires: %{name}-devel"),
            "macro must survive verbatim; got:\n{stdout}"
        );
    }

    #[test]
    fn expand_spec_with_no_conditionals_emits_verbatim_no_tags() {
        const SPEC: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: gcc

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "expand",
                "--profiles",
                "rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        // Header rendered, but no [ACTIVE]/[INACTIVE]/[INDETERMINATE]
        // anywhere since there are no branch directives.
        assert!(stdout.contains("== Profile rhel-9-x86_64 =="));
        assert!(
            !stdout.contains("[ACTIVE]")
                && !stdout.contains("[INACTIVE]")
                && !stdout.contains("[INDETERMINATE"),
            "spec with no conditionals must produce no branch tags; got:\n{stdout}"
        );
    }

    #[test]
    fn expand_json_pins_directive_text_and_envelope() {
        // Lock the JSON wire-format envelope keys + the `directive`
        // contract (canonical form from CollectedBranch.display) so
        // a refactor that changes either surfaces here.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, EXPAND_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "expand",
                "--profiles",
                "rhel-9-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        assert_eq!(v["target_set"], "<ad-hoc>");
        assert_eq!(v["profiles"][0], "rhel-9-x86_64");
        assert!(
            v["path"].as_str().unwrap_or("").ends_with("foo.spec"),
            "path must end with spec filename; got {}",
            v["path"]
        );
        let branches = v["per_profile"][0]["branches"]
            .as_array()
            .expect("branches");
        // First branch is %if 0%{?rhel} on line 8 of EXPAND_SPEC.
        assert_eq!(branches[0]["directive"], "%if 0%{?rhel}");
        assert_eq!(branches[0]["line"], 8);
    }

    #[test]
    fn expand_surfaces_parser_diagnostics_for_broken_spec() {
        const BROKEN: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: t
License: MIT

%if 0%{?rhel}
BuildRequires: gcc
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BROKEN).expect("spec");
        let (_rc, _, stderr) = run(
            &[
                "matrix",
                "expand",
                "--profiles",
                "rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert!(
            stderr.contains("parser diagnostic") || stderr.contains("recovered AST"),
            "expected parser-diagnostic banner; got stderr={stderr:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// matrix diff
// ---------------------------------------------------------------------------

mod matrix_diff {
    use super::*;

    const DIFF_SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: Test
License: MIT
BuildRequires: gcc
BuildRequires: make
Requires: glibc

%if 0%{?rhel}
BuildRequires: systemd-rpm-macros
Requires: rhel-only-pkg
%else
BuildRequires: rpm-build-systemd
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    #[test]
    fn diff_human_groups_common_and_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, DIFF_SPEC).expect("write spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "diff",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "diff must succeed; stderr={stderr}");
        // BR groups
        assert!(
            stdout.contains("common (2): gcc, make"),
            "expected common BR line; got:\n{stdout}"
        );
        assert!(
            stdout.contains("only rhel-9-x86_64 (1): systemd-rpm-macros"),
            "expected rhel-only BR line; got:\n{stdout}"
        );
        assert!(
            stdout.contains("only altlinux-10-x86_64 (1): rpm-build-systemd"),
            "expected alt-only BR line; got:\n{stdout}"
        );
        // Requires group
        assert!(
            stdout.contains("Requires"),
            "expected Requires group header; got:\n{stdout}"
        );
        assert!(
            stdout.contains("only rhel-9-x86_64 (1): rhel-only-pkg"),
            "expected rhel-only Requires line; got:\n{stdout}"
        );
    }

    #[test]
    fn diff_json_shape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, DIFF_SPEC).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "diff",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        // Envelope keys — pin both names and types so a serde rename
        // to camelCase or a dropped field surfaces here rather than
        // breaking downstream tooling silently.
        assert_eq!(v["profile_a"], "rhel-9-x86_64");
        assert_eq!(v["profile_b"], "altlinux-10-x86_64");
        assert!(v["path"].as_str().is_some_and(|p| p.ends_with("foo.spec")));
        let groups = v["groups"].as_array().expect("groups");
        // Two compared tags: BuildRequires + Requires.
        assert_eq!(groups.len(), 2);
        // Per-group keys must use exactly these `snake_case` names.
        for g in groups {
            let obj = g.as_object().expect("group is object");
            assert!(obj.contains_key("tag"), "group missing `tag`");
            assert!(obj.contains_key("common"), "group missing `common`");
            assert!(obj.contains_key("only_a"), "group missing `only_a`");
            assert!(obj.contains_key("only_b"), "group missing `only_b`");
        }
        // First group is BuildRequires by COMPARED_TAGS order.
        assert_eq!(groups[0]["tag"], "BuildRequires");
        let common: Vec<&str> = groups[0]["common"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(common.contains(&"gcc"));
        assert!(common.contains(&"make"));
        // Second group is Requires.
        assert_eq!(groups[1]["tag"], "Requires");
    }

    #[test]
    fn diff_empty_bucket_serialises_as_empty_array() {
        // Spec has BuildRequires but NO Requires line — the Requires
        // group must serialise every bucket as `[]`, not omit them.
        // Downstream `jq` pipes rely on the schema being uniform.
        const NO_REQUIRES: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: gcc

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, NO_REQUIRES).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "diff",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        let req = &v["groups"][1];
        assert_eq!(req["tag"], "Requires");
        assert!(
            req["common"]
                .as_array()
                .expect("common is array")
                .is_empty()
        );
        assert!(
            req["only_a"]
                .as_array()
                .expect("only_a is array")
                .is_empty()
        );
        assert!(
            req["only_b"]
                .as_array()
                .expect("only_b is array")
                .is_empty()
        );
    }

    #[test]
    fn diff_rejects_identical_profiles() {
        // Self-diff is degenerate (empty buckets) AND resolver dedups
        // by ID so the internal `targets` vec collapses to length 1,
        // which would panic the indexer. Reject with a clear message.
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, DIFF_SPEC).expect("write spec");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "diff",
                "--profiles",
                "rhel-9-x86_64,rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2);
        assert!(
            stderr.contains("distinct profiles"),
            "expected distinct-profiles error; got {stderr}"
        );
    }

    #[test]
    fn diff_rejects_wrong_profile_count() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, DIFF_SPEC).expect("write spec");
        // One profile.
        let (rc1, _, stderr1) = run(
            &[
                "matrix",
                "diff",
                "--profiles",
                "rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc1, 2, "1 profile must be rejected; stderr={stderr1}");
        assert!(stderr1.contains("exactly two profiles"));
        // Three profiles.
        let (rc3, _, stderr3) = run(
            &[
                "matrix",
                "diff",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64,sles-15-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc3, 2, "3 profiles must be rejected; stderr={stderr3}");
        assert!(stderr3.contains("exactly two profiles"));
    }

    #[test]
    fn diff_rejects_multiple_specs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = dir.path().join("a.spec");
        let b = dir.path().join("b.spec");
        std::fs::write(&a, DIFF_SPEC).expect("write a");
        std::fs::write(&b, DIFF_SPEC).expect("write b");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "diff",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                a.to_str().unwrap(),
                b.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2);
        assert!(
            stderr.contains("exactly one spec"),
            "expected multi-spec rejection; got {stderr}"
        );
    }

    #[test]
    fn diff_skip_policy_drops_indeterminate_branch_deps() {
        // Arithmetic Raw `%if 0%{?rhel} >= 8` → Indeterminate. Under
        // the Skip policy that diff uses, the BR inside is hidden on
        // both profiles → it appears in neither bucket, NOT in
        // only-A or only-B. Documents the conservative semantic.
        const ARITH: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: outside

%if 0%{?rhel} >= 8
BuildRequires: maybe-rhel
%endif

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, ARITH).expect("write spec");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "diff",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        let br = &v["groups"][0];
        let join_all = |arr: &serde_json::Value| -> String {
            arr.as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>()
                .join(",")
        };
        let all_text = format!(
            "{}|{}|{}",
            join_all(&br["common"]),
            join_all(&br["only_a"]),
            join_all(&br["only_b"])
        );
        assert!(
            !all_text.contains("maybe-rhel"),
            "Skip policy must hide indeterminate-branch dep; got buckets={all_text}"
        );
        assert!(
            all_text.contains("outside"),
            "outside-of-cond dep must survive in common; got buckets={all_text}"
        );
    }

    #[test]
    fn diff_surfaces_parser_diagnostics() {
        const BROKEN: &str = "\
Name: foo
Version: 1.0
Release: 1
Summary: t
License: MIT

%if 0%{?rhel}
BuildRequires: gcc
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BROKEN).expect("write spec");
        let (_rc, _, stderr) = run(
            &[
                "matrix",
                "diff",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert!(
            stderr.contains("parser diagnostic") || stderr.contains("recovered AST"),
            "expected parser-diagnostic banner; got stderr={stderr:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// matrix verify-contract
// ---------------------------------------------------------------------------

mod matrix_verify_contract {
    use super::*;

    const VC_SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: gcc
BuildRequires: make

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";

    fn write_contract_and_spec(
        content: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        let contract = dir.path().join("contract.toml");
        std::fs::write(&spec, VC_SPEC).expect("write spec");
        std::fs::write(&contract, content).expect("write contract");
        (dir, spec, contract)
    }

    #[test]
    fn verify_contract_passes_when_all_required_present() {
        let (_dir, spec, contract) = write_contract_and_spec(
            r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc", "make"]
"#,
        );
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        assert!(
            stdout.contains("rhel-9-x86_64: PASS"),
            "expected PASS row; got:\n{stdout}"
        );
    }

    #[test]
    fn verify_contract_fails_on_missing_required() {
        let (_dir, spec, contract) = write_contract_and_spec(
            r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc", "missing-pkg"]
"#,
        );
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 1, "missing required must trigger exit 1");
        assert!(
            stdout.contains("[missing] missing-pkg"),
            "expected [missing] tag in output; got:\n{stdout}"
        );
    }

    #[test]
    fn verify_contract_fails_on_forbidden_present() {
        let (_dir, spec, contract) = write_contract_and_spec(
            r#"
[profiles."rhel-9-x86_64"]
must_not_have_buildrequires = ["gcc"]
"#,
        );
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 1);
        assert!(
            stdout.contains("[forbidden] gcc"),
            "expected [forbidden] tag in output; got:\n{stdout}"
        );
    }

    #[test]
    fn verify_contract_skips_profile_without_contract() {
        let (_dir, spec, contract) = write_contract_and_spec(
            r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc"]
"#,
        );
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "altlinux-10-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "no contract for profile must exit 0");
        // Pin the full user-facing label so a renderer rename
        // surfaces here rather than slipping past a partial-match
        // grep.
        assert!(
            stdout.contains("(no contract — skipping)"),
            "expected exact `(no contract — skipping)` label; got:\n{stdout}"
        );
    }

    #[test]
    fn verify_contract_missing_file_exits_2() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, VC_SPEC).expect("write spec");
        let missing = dir.path().join("does-not-exist.toml");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                "--contract",
                missing.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2, "missing contract file must surface as user-error");
        assert!(
            stderr.contains("opening contract"),
            "expected open-failure context in stderr; got {stderr}"
        );
    }

    #[test]
    fn verify_contract_invalid_toml_exits_2() {
        let (_dir, spec, contract) = write_contract_and_spec("not valid toml = = ===");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2);
        assert!(
            stderr.contains("parsing contract") || stderr.contains("TOML"),
            "expected TOML parse error; got {stderr}"
        );
    }

    #[test]
    fn verify_contract_rejects_unknown_field() {
        let (_dir, spec, contract) = write_contract_and_spec(
            r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc"]
typo_field = "oops"
"#,
        );
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2, "deny_unknown_fields must trigger exit 2");
        assert!(
            stderr.contains("typo_field") || stderr.contains("unknown"),
            "expected unknown-field error; got {stderr}"
        );
    }

    #[test]
    fn verify_contract_json_shape() {
        let (_dir, spec, contract) = write_contract_and_spec(
            r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc", "missing-pkg"]
"#,
        );
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 1);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        assert_eq!(v["target_set"], "<ad-hoc>");
        let files = v["files"].as_array().expect("files");
        let pp = files[0]["per_profile"].as_array().expect("per_profile");
        let row = &pp[0];
        assert_eq!(row["profile_id"], "rhel-9-x86_64");
        assert_eq!(row["status"]["kind"], "violations");
        let violations = row["status"]["violations"].as_array().expect("violations");
        assert!(
            violations
                .iter()
                .any(|v| { v["kind"] == "missing_required" && v["package"] == "missing-pkg" })
        );
    }

    #[test]
    fn verify_contract_requires_contract_flag() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, VC_SPEC).expect("write spec");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_ne!(rc, 0, "missing --contract must fail");
        assert!(
            stderr.contains("--contract"),
            "expected clap mention of --contract; got {stderr}"
        );
    }

    #[test]
    fn verify_contract_matches_macro_bearing_dep_by_surface_form() {
        // `BuildRequires: %{name}-devel` records canonical=surface
        // form. A contract entry writing the same surface form must
        // match; a literal-only form must NOT match.
        const SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: %{name}-devel

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        let contract = dir.path().join("contract.toml");
        std::fs::write(&spec, SPEC).expect("write spec");
        std::fs::write(
            &contract,
            r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["%{name}-devel"]

[profiles."altlinux-10-x86_64"]
# Literal form — must NOT silently match the macro-bearing dep
# because the analyzer doesn't statically expand %{name}.
must_have_buildrequires = ["foo-devel"]
"#,
        )
        .expect("write contract");
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64,altlinux-10-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        // Non-zero because altlinux profile's literal entry should
        // fail to match the macro-bearing dep.
        assert_eq!(rc, 1);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        let pp = v["files"][0]["per_profile"]
            .as_array()
            .expect("per_profile");
        let rhel = pp
            .iter()
            .find(|r| r["profile_id"] == "rhel-9-x86_64")
            .expect("rhel row");
        assert_eq!(rhel["status"]["kind"], "pass");
        let alt = pp
            .iter()
            .find(|r| r["profile_id"] == "altlinux-10-x86_64")
            .expect("alt row");
        assert_eq!(alt["status"]["kind"], "violations");
    }

    #[test]
    fn verify_contract_skips_rich_if_dep() {
        // MVP: `BuildRequires: (libfoo if systemd)` is a BoolDep::If
        // which the collector conservatively skips. Document the
        // behaviour so a future "recurse into rich deps" change
        // surfaces here.
        const SPEC: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: t
License: MIT
BuildRequires: (libfoo if systemd)

%description
B

%files

%changelog
* Mon Jan 01 2024 a <a@b> - 1.0-1
- init
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        let contract = dir.path().join("contract.toml");
        std::fs::write(&spec, SPEC).expect("write spec");
        std::fs::write(
            &contract,
            r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["libfoo"]
"#,
        )
        .expect("write contract");
        let (rc, _, _) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        // Conservative skip → libfoo is NOT registered → missing.
        assert_eq!(rc, 1, "rich If-dep must be conservatively skipped");
    }

    #[test]
    fn verify_contract_json_no_contract_discriminator() {
        // Lock the `no_contract` discriminator on the JSON wire
        // format. Documented in doc/matrix.md.
        let (_dir, spec, contract) = write_contract_and_spec(
            r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc"]
"#,
        );
        let (rc, stdout, _) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "altlinux-10-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0);
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON: {e}\n{stdout}"));
        let row = &v["files"][0]["per_profile"][0];
        assert_eq!(row["profile_id"], "altlinux-10-x86_64");
        assert_eq!(row["status"]["kind"], "no_contract");
    }

    #[test]
    fn verify_contract_surfaces_parser_diagnostics() {
        // Broken spec (missing %endif) produces parser diagnostics;
        // the CLI must warn on stderr before reporting the verdict
        // so the operator knows the contract was checked against a
        // recovered partial AST.
        const BROKEN: &str = "\
Name:    foo
Version: 1.0
Release: 1
Summary: t
License: MIT

%if 0%{?rhel}
BuildRequires: gcc
";
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        let contract = dir.path().join("contract.toml");
        std::fs::write(&spec, BROKEN).expect("write spec");
        std::fs::write(
            &contract,
            r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc"]
"#,
        )
        .expect("write contract");
        let (_rc, _, stderr) = run(
            &[
                "matrix",
                "verify-contract",
                "--profiles",
                "rhel-9-x86_64",
                "--contract",
                contract.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert!(
            stderr.contains("parser diagnostic") || stderr.contains("recovered AST"),
            "expected parser-diagnostic banner; got stderr={stderr:?}"
        );
    }

    #[test]
    fn verify_contract_requires_target_or_profiles_flag() {
        let (_dir, spec, contract) = write_contract_and_spec(
            r#"
[profiles."rhel-9-x86_64"]
must_have_buildrequires = ["gcc"]
"#,
        );
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "verify-contract",
                "--contract",
                contract.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_ne!(rc, 0);
        assert!(
            stderr.contains("--target-set") || stderr.contains("--profiles"),
            "expected clap to mention required flags; got {stderr}"
        );
    }
}

// ---------------------------------------------------------------------------
// matrix impact
// ---------------------------------------------------------------------------

mod matrix_impact {
    use super::*;
    use std::process::Command;

    /// Initialise a fresh git repo at `dir` with sane user identity so
    /// commits succeed in CI sandboxes that don't have a global config.
    /// Disables GPG signing for the same reason.
    fn git_init(dir: &std::path::Path) {
        for args in [
            &["init", "--quiet", "-b", "main"][..],
            &["config", "user.email", "test@example.invalid"][..],
            &["config", "user.name", "Test"][..],
            &["config", "commit.gpgsign", "false"][..],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .status()
                .expect("spawn git");
            assert!(status.success(), "git {args:?} failed");
        }
    }

    fn git_commit_all(dir: &std::path::Path, msg: &str) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["add", "-A"])
            .status()
            .expect("git add");
        assert!(status.success(), "git add failed");
        // --allow-empty so "two identical revisions" tests (no-change
        // case) still produce two distinct SHAs.
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "commit",
                "--quiet",
                "--no-verify",
                "--allow-empty",
                "-m",
                msg,
            ])
            .status()
            .expect("git commit");
        assert!(status.success(), "git commit failed");
    }

    fn head_sha(dir: &std::path::Path) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .expect("rev-parse");
        assert!(out.status.success(), "rev-parse failed");
        String::from_utf8(out.stdout)
            .expect("utf8 sha")
            .trim()
            .to_string()
    }

    /// Set up a repo with two commits of `foo.spec`. Returns
    /// `(dir, spec_path, from_sha, to_sha)`.
    fn two_commit_repo(
        from_body: &str,
        to_body: &str,
    ) -> (tempfile::TempDir, std::path::PathBuf, String, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        git_init(dir.path());
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, from_body).expect("write from spec");
        git_commit_all(dir.path(), "initial");
        let from_sha = head_sha(dir.path());
        std::fs::write(&spec, to_body).expect("write to spec");
        git_commit_all(dir.path(), "bump deps");
        let to_sha = head_sha(dir.path());
        (dir, spec, from_sha, to_sha)
    }

    const BASE_SPEC: &str = "\
Name:           foo
Version:        1.0
Release:        1
Summary:        Test
License:        MIT

BuildRequires:  gcc
BuildRequires:  make

%description
B

%files

%changelog
* Mon May 18 2026 t <t@e.invalid> - 1.0-1
- init
";

    #[test]
    fn impact_human_reports_added_and_removed_per_profile() {
        let to_spec = BASE_SPEC.replace(
            "BuildRequires:  make\n",
            "BuildRequires:  make\nBuildRequires:  cmake\n",
        );
        let (_dir, spec, from, to) = two_commit_repo(BASE_SPEC, &to_spec);
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "impact",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--from",
                &from,
                "--to",
                &to,
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}\nstdout={stdout}");
        assert!(
            stdout.contains("cmake"),
            "expected added dep to surface; got {stdout}"
        );
        assert!(
            stdout.contains("added") || stdout.contains("+1"),
            "expected added marker; got {stdout}"
        );
    }

    #[test]
    fn impact_json_shape_has_expected_top_level_keys() {
        let to_spec = BASE_SPEC.replace(
            "BuildRequires:  make\n",
            "BuildRequires:  make\nBuildRequires:  cmake\n",
        );
        let (_dir, spec, from, to) = two_commit_repo(BASE_SPEC, &to_spec);
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "impact",
                "--profiles",
                "generic",
                "--from",
                &from,
                "--to",
                &to,
                "--format",
                "json",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}\nstdout={stdout}");
        let v: serde_json::Value =
            serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("invalid JSON: {e}\n{stdout}"));
        assert_eq!(v["from"], from.as_str());
        assert_eq!(v["to"], to.as_str());
        assert!(v["target_set"].is_string());
        assert!(v["profiles"].is_array());
        assert!(v["affected_profile_count"].is_number());
        assert!(v["per_profile"].is_array());
    }

    #[test]
    fn impact_no_change_reports_clean() {
        // Identical from/to -> no movement on any profile. The human
        // renderer's "(no change on any profile)" line is the
        // contract; pin it so a refactor of the renderer can't quietly
        // hide a passing diff.
        let (_dir, spec, from, to) = two_commit_repo(BASE_SPEC, BASE_SPEC);
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "impact",
                "--profiles",
                "generic,rhel-9-x86_64",
                "--from",
                &from,
                "--to",
                &to,
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        assert!(
            stdout.contains("no change"),
            "expected no-change message; got {stdout}"
        );
    }

    #[test]
    fn impact_unknown_rev_exits_2() {
        let (_dir, spec, _from, _to) = two_commit_repo(BASE_SPEC, BASE_SPEC);
        let (rc, _stdout, stderr) = run(
            &[
                "matrix",
                "impact",
                "--profiles",
                "generic",
                "--from",
                "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2, "unknown rev must exit 2; stderr={stderr}");
        assert!(
            stderr.contains("git show")
                || stderr.contains("unknown")
                || stderr.contains("deadbeef"),
            "expected diagnostic mentioning git/rev; got {stderr}"
        );
    }

    #[test]
    fn impact_file_missing_at_from_rev_treats_as_empty() {
        // PR adds a new spec file: it doesn't exist at the `from` rev.
        // The CLI maps "file does not exist at REV" to an empty
        // baseline so every dep surfaces as `added` -- natural
        // semantics for "new file in this PR".
        let dir = tempfile::tempdir().expect("tempdir");
        git_init(dir.path());
        std::fs::write(dir.path().join("README"), "placeholder").expect("write readme");
        git_commit_all(dir.path(), "init");
        let from_sha = head_sha(dir.path());
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BASE_SPEC).expect("write spec");
        git_commit_all(dir.path(), "add foo.spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "impact",
                "--profiles",
                "generic",
                "--from",
                &from_sha,
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        assert!(
            stdout.contains("gcc") && stdout.contains("make"),
            "expected gcc/make as added deps; got {stdout}"
        );
    }

    #[test]
    fn impact_rejects_stdin() {
        let (rc, _stdout, stderr) = run(
            &[
                "matrix",
                "impact",
                "--profiles",
                "generic",
                "--from",
                "HEAD",
                "-",
            ],
            Some(BASE_SPEC),
        );
        assert_eq!(rc, 2);
        assert!(
            stderr.contains("stdin"),
            "expected stdin rejection; got {stderr}"
        );
    }

    #[test]
    fn impact_spec_outside_git_repo_exits_2() {
        let dir = tempfile::tempdir().expect("tempdir");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BASE_SPEC).expect("write spec");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "impact",
                "--profiles",
                "generic",
                "--from",
                "HEAD",
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2);
        assert!(
            stderr.contains("not a git repository") || stderr.contains("git"),
            "expected git-repo error; got {stderr}"
        );
    }

    #[test]
    fn impact_multiple_specs_rejected() {
        let (_dir, spec, from, _to) = two_commit_repo(BASE_SPEC, BASE_SPEC);
        let second = spec.parent().unwrap().join("bar.spec");
        std::fs::write(&second, BASE_SPEC).expect("write second");
        let (rc, _, stderr) = run(
            &[
                "matrix",
                "impact",
                "--profiles",
                "generic",
                "--from",
                &from,
                spec.to_str().unwrap(),
                second.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 2);
        assert!(
            stderr.contains("exactly one") || stderr.contains("one spec"),
            "expected single-spec error; got {stderr}"
        );
    }

    #[test]
    fn impact_file_missing_at_to_rev_treats_as_empty() {
        // Mirror of `impact_file_missing_at_from_rev_treats_as_empty`:
        // the spec exists at `from` but has been deleted at `to`
        // (a PR that removes a package). The CLI must treat the
        // missing `to` side as empty so deps surface as `removed`.
        let dir = tempfile::tempdir().expect("tempdir");
        git_init(dir.path());
        // Seed an unrelated file so a "remove-only" commit still has
        // tree content to diff against.
        std::fs::write(dir.path().join("README"), "placeholder").expect("write readme");
        let spec = dir.path().join("foo.spec");
        std::fs::write(&spec, BASE_SPEC).expect("write spec");
        git_commit_all(dir.path(), "add foo.spec");
        let from_sha = head_sha(dir.path());
        // `git rm` then commit — spec is absent at HEAD.
        let status = Command::new("git")
            .arg("-C")
            .arg(dir.path())
            .args(["rm", "--quiet", "foo.spec"])
            .status()
            .expect("git rm");
        assert!(status.success(), "git rm failed");
        git_commit_all(dir.path(), "drop foo.spec");
        let to_sha = head_sha(dir.path());
        // The CLI canonicalises the spec path to locate the repo
        // root, so the file must exist on disk even though it's
        // absent from HEAD. Re-create an untracked copy — git show
        // reads from the rev, not the worktree.
        std::fs::write(&spec, BASE_SPEC).expect("rewrite untracked spec");
        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "impact",
                "--profiles",
                "generic",
                "--from",
                &from_sha,
                "--to",
                &to_sha,
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(rc, 0, "stderr={stderr}");
        assert!(
            stdout.contains("removed"),
            "expected `removed` marker for deps gone at `to` side; got {stdout}"
        );
        assert!(
            stdout.contains("gcc") && stdout.contains("make"),
            "expected gcc/make in removed deps; got {stdout}"
        );
        assert!(
            !stdout.contains("added ("),
            "expected no `added (N)` lines (deps only disappear); got {stdout}"
        );
    }

    #[test]
    fn impact_surfaces_parser_diagnostics_for_broken_spec() {
        // Mismatched %if/%endif at `from` rev, fixed at `to`. The CLI
        // must surface the from-side parser diagnostic on stderr but
        // still produce a useful (recovered-AST) impact report.
        //
        // Robust to wording changes: match on stable substrings
        // (`parser diagnostic` + the side label `from`) rather than
        // pinning the full sentence.
        const BROKEN: &str = "\
Name:           foo
Version:        1.0
Release:        1
Summary:        Test
License:        MIT

%if 0%{?rhel}
BuildRequires:  gcc

%description
B

%files

%changelog
* Mon May 18 2026 t <t@e.invalid> - 1.0-1
- init
";
        let (_dir, spec, from, to) = two_commit_repo(BROKEN, BASE_SPEC);
        let (rc, _stdout, stderr) = run(
            &[
                "matrix",
                "impact",
                "--profiles",
                "generic",
                "--from",
                &from,
                "--to",
                &to,
                spec.to_str().unwrap(),
            ],
            None,
        );
        // rc=0: the CLI surfaces diagnostics as warnings, not hard
        // failures (matches sibling matrix commands).
        assert_eq!(rc, 0, "stderr={stderr}");
        assert!(
            stderr.contains("parser diagnostic"),
            "expected `parser diagnostic` substring in stderr; got {stderr:?}"
        );
        assert!(
            stderr.contains("from"),
            "expected `from`-side label in stderr; got {stderr:?}"
        );
    }

    #[test]
    fn impact_honours_git_cmd_override() {
        // The `--git-cmd` flag must reach every git invocation
        // (rev-parse, rev-parse --verify, show). We point it at a
        // tiny shell stub that records its argv to a sentinel file
        // then exec's the real `git`, so the impact run still
        // succeeds end-to-end and we can grep the recording to
        // confirm the stub was actually used.
        use std::os::unix::fs::PermissionsExt;

        let to_spec = BASE_SPEC.replace(
            "BuildRequires:  make\n",
            "BuildRequires:  make\nBuildRequires:  cmake\n",
        );
        let (dir, spec, from, to) = two_commit_repo(BASE_SPEC, &to_spec);

        // Resolve the real git up-front so the stub can exec it
        // without relying on $PATH inside the child (the CLI scrubs
        // very little, but being explicit makes the test robust).
        let real_git = {
            let out = Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .expect("locate git");
            assert!(
                out.status.success(),
                "could not locate `git` for the stub to exec"
            );
            String::from_utf8(out.stdout)
                .expect("utf8")
                .trim()
                .to_string()
        };

        let trace = dir.path().join("git-stub-trace.log");
        let stub = dir.path().join("git-stub.sh");
        let script = format!(
            "#!/bin/sh\n\
             # Record argv (one arg per line) so the test can verify\n\
             # which git subcommands the CLI invoked.\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\" >> \"{trace}\"; done\n\
             printf -- '---\\n' >> \"{trace}\"\n\
             exec \"{git}\" \"$@\"\n",
            trace = trace.display(),
            git = real_git,
        );
        std::fs::write(&stub, script).expect("write stub");
        let mut perms = std::fs::metadata(&stub).expect("stat stub").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).expect("chmod stub");

        let (rc, stdout, stderr) = run(
            &[
                "matrix",
                "impact",
                "--profiles",
                "generic",
                "--from",
                &from,
                "--to",
                &to,
                "--git-cmd",
                stub.to_str().unwrap(),
                spec.to_str().unwrap(),
            ],
            None,
        );
        assert_eq!(
            rc, 0,
            "impact must succeed via stub; stderr={stderr}\nstdout={stdout}"
        );
        // Sanity: the run did the actual work end-to-end.
        assert!(
            stdout.contains("cmake"),
            "expected added dep to surface through the stubbed git; got {stdout}"
        );

        let recorded = std::fs::read_to_string(&trace).expect("stub never wrote its trace file");
        assert!(
            recorded.contains("rev-parse"),
            "stub trace missing `rev-parse`; got:\n{recorded}"
        );
        assert!(
            recorded.contains("show"),
            "stub trace missing `show`; got:\n{recorded}"
        );
    }
}
