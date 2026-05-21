//! Regression tests for RPM050 (`hardcoded-paths`) context handling.
//!
//! Investigating the QA-corpus run showed RPM050 firing on 24/31 upstream
//! specs (77%). Most of the noise came from three context categories where
//! a literal path is conventional rather than a defect:
//!
//! 1. **Scriptlet bodies** (`%post`, `%pre`, `%postun`, …, plus `%trigger*`).
//!    These run on the target system, where macros like `%{_sysconfdir}` are
//!    not meaningful — paths like `/etc/foo.conf` are the actual runtime
//!    targets. Rewriting them serves no purpose.
//! 2. **Comments with quoted strings**. The pre-existing comment heuristic
//!    bailed out as soon as it saw a `"` or `'` between the `#` and the
//!    matched slash, so lines like
//!    `# 'make install' insists on creating a separate /usr/sbin directory`
//!    in glibc.spec produced a diagnostic. Real specs quote constantly in
//!    documentation prose; the quote-guard cost dwarfed its benefit.
//! 3. **Paths anchored at `%{buildroot}` / `$RPM_BUILD_ROOT`**.
//!    `mkdir -p $RPM_BUILD_ROOT/etc/foo` is the canonical install pattern.
//!    Rewriting the literal `/etc` to `%{_sysconfdir}` produces an
//!    expansion that resolves back to `/etc` on every standard profile —
//!    the warning amounts to "consider rewriting `/etc` to a macro that
//!    expands to `/etc`".
//!
//! Each test pins exactly one of those contexts plus a paired positive
//! ("the rule still fires elsewhere") so a future refactor that accidentally
//! widens the exclusion is caught.

use std::path::Path;

use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::{Diagnostic, analyze_with_profile_at, profile::Profile};

fn diags_for(src: &str) -> Vec<Diagnostic> {
    let cfg = Config::default();
    let profile = Profile::default();
    let (_outcome, diags) = analyze_with_profile_at(src, None::<&Path>, &cfg, profile);
    diags
}

fn has_rpm050(src: &str) -> bool {
    diags_for(src).iter().any(|d| d.lint_id == "RPM050")
}

fn rpm050_count(src: &str) -> usize {
    diags_for(src)
        .iter()
        .filter(|d| d.lint_id == "RPM050")
        .count()
}

// Minimal preamble that satisfies the parser — keep tests focused on the
// bit we actually care about.
const PREAMBLE: &str = "Name: x\nVersion: 1\nRelease: 1\nSummary: x\nLicense: x\n%description\nx\n";

#[test]
fn rpm050_does_not_fire_in_post_scriptlet() {
    // Real-world repro: bind.spec %post body. The path is the runtime
    // target; rewriting to %{_sysconfdir} achieves nothing here.
    let src = format!(
        "{PREAMBLE}%files\n/x\n%post\n[ -x /sbin/restorecon ] && /sbin/restorecon /etc/rndc.* >/dev/null 2>&1\n%changelog\n"
    );
    assert!(
        !has_rpm050(&src),
        "RPM050 must not fire inside %post scriptlet: {:?}",
        diags_for(&src)
    );
}

#[test]
fn rpm050_does_not_fire_in_preun_scriptlet() {
    let src = format!("{PREAMBLE}%files\n/x\n%preun\nrm -f /etc/foo.conf\n%changelog\n");
    assert!(!has_rpm050(&src));
}

#[test]
fn rpm050_does_not_fire_in_postun_scriptlet() {
    let src = format!(
        "{PREAMBLE}%files\n/x\n%postun\nif [ -d /var/lib/foo ]; then rmdir /var/lib/foo; fi\n%changelog\n"
    );
    assert!(!has_rpm050(&src));
}

#[test]
fn rpm050_does_not_fire_in_pretrans_scriptlet() {
    let src = format!("{PREAMBLE}%files\n/x\n%pretrans\necho /etc/whatever\n%changelog\n");
    assert!(!has_rpm050(&src));
}

#[test]
fn rpm050_does_not_fire_in_comment_with_quotes() {
    // glibc.spec line 1610 repro: comment text contains single quotes
    // before the path. The pre-fix heuristic treated the quote as a
    // signal that this wasn't really a comment.
    let src = format!(
        "{PREAMBLE}%install\n# 'make install' insists on creating a separate /usr/sbin directory\n:\n%changelog\n"
    );
    assert!(
        !has_rpm050(&src),
        "comment with quotes must not produce a diagnostic: {:?}",
        diags_for(&src)
    );
}

#[test]
fn rpm050_does_not_fire_in_comment_with_double_quotes() {
    let src = format!(
        "{PREAMBLE}%install\n# Note: \"foo\" lives in /usr/bin by default\n:\n%changelog\n"
    );
    assert!(!has_rpm050(&src));
}

#[test]
fn rpm050_does_not_fire_on_buildroot_prefixed_path() {
    // openssh.spec / systemd.spec repro: `$RPM_BUILD_ROOT/etc/foo`.
    // The literal `/etc` is part of the buildroot install layout; the
    // macro replacement (`%{_sysconfdir}`) would expand back to `/etc`
    // on every supported profile.
    let src = format!("{PREAMBLE}%install\ninstall -d $RPM_BUILD_ROOT/etc/pam.d/\n%changelog\n");
    assert!(
        !has_rpm050(&src),
        "`$RPM_BUILD_ROOT/etc/...` is the canonical install pattern: {:?}",
        diags_for(&src)
    );
}

#[test]
fn rpm050_does_not_fire_on_buildroot_brace_prefixed_path() {
    let src =
        format!("{PREAMBLE}%install\ninstall -d ${{RPM_BUILD_ROOT}}/etc/sysconfig/\n%changelog\n");
    assert!(!has_rpm050(&src));
}

#[test]
fn rpm050_does_not_fire_on_macro_buildroot_prefixed_path() {
    // `%{buildroot}/etc/...` is the modern equivalent of
    // `$RPM_BUILD_ROOT/etc/...` and the same exemption applies.
    let src = format!("{PREAMBLE}%install\ntouch %{{buildroot}}/etc/foo.conf\n%changelog\n");
    assert!(!has_rpm050(&src));
}

// --- Positive controls: the rule still fires where it should ---

#[test]
fn rpm050_still_fires_in_files_list() {
    let src = format!("{PREAMBLE}%files\n/etc/x/x.conf\n%changelog\n");
    assert!(
        has_rpm050(&src),
        "must still fire on %files entries: {:?}",
        diags_for(&src)
    );
}

#[test]
fn rpm050_still_fires_in_install_section_plain() {
    // A plain hardcoded path in %install (not buildroot-prefixed) is
    // still a real complaint.
    let src = format!("{PREAMBLE}%install\nmkdir -p /usr/lib/foo\n%changelog\n");
    assert!(
        has_rpm050(&src),
        "plain hardcoded install path should still fire: {:?}",
        diags_for(&src)
    );
}

#[test]
fn rpm050_count_for_install_with_buildroot_is_zero() {
    // Multiple buildroot-prefixed paths on consecutive lines (the
    // systemd `touch %{buildroot}/etc/systemd/*.conf` block) used to
    // emit one warning per line; under the exemption it must emit none.
    let src = format!(
        "{PREAMBLE}%install\ntouch %{{buildroot}}/etc/crypttab\nchmod 600 %{{buildroot}}/etc/crypttab\n%changelog\n"
    );
    assert_eq!(rpm050_count(&src), 0, "got {:?}", diags_for(&src));
}
