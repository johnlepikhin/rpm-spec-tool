//! RPM385 `optflags-overridden` — `%build` assigns `CFLAGS=`,
//! `CXXFLAGS=`, `LDFLAGS=`, or `FFLAGS=` without preserving the
//! distro's `%{optflags}` / `$RPM_OPT_FLAGS`.
//!
//! Distributions populate `%{optflags}` with security-hardening flags
//! (`-D_FORTIFY_SOURCE=3`, `-fstack-protector-strong`, `-fPIE`,
//! `-Wl,-z,relro,-z,now` via `%{build_ldflags}`, etc.). When a spec
//! says `make CFLAGS=-O2 install`, it overwrites those flags wholesale
//! — the resulting binaries ship without the distro's hardening. Mock,
//! `rpmlint`, and packager review all catch this in CI, but the smell
//! is easy to miss in spec diffs.
//!
//! Detection: the rule fires on any token of the form `CFLAGS=…` (or
//! the other three families) in `%build` whose value text does **not**
//! reference `%{optflags}`, `%{build_cflags}`, `%{build_cxxflags}`,
//! `%{build_ldflags}`, `%{build_fflags}`, or the legacy
//! `$RPM_OPT_FLAGS` / `${RPM_OPT_FLAGS}` env var. We also accept the
//! "append" form `CFLAGS="$CFLAGS …"` and `CFLAGS+=…` since those
//! preserve the prior setting.
//!
//! One diagnostic per `%build` section to avoid noise — a build that
//! overrides CFLAGS three times is one problem, not three.

use rpm_spec::ast::{ShellBody, Span, SpecFile};

use crate::diagnostic::{Diagnostic, LintCategory, Severity};
use crate::lint::{Lint, LintMetadata};
use crate::shell::for_each_buildscript;
use crate::shell::strip_trailing_comment;
use crate::visit::Visit;

pub static METADATA: LintMetadata = LintMetadata {
    id: "RPM385",
    name: "optflags-overridden",
    description: "Build script assigns `CFLAGS=`/`CXXFLAGS=`/`LDFLAGS=`/`FFLAGS=` without \
                  preserving `%{optflags}` (or `$RPM_OPT_FLAGS`). The override drops the \
                  distro's hardening flags (FORTIFY_SOURCE, PIE, RELRO, stack-protector).",
    default_severity: Severity::Warn,
    category: LintCategory::Correctness,
};

/// Lint state for RPM385 `optflags-overridden`. Collects one
/// diagnostic per `%build` section that overrides a hardening flag
/// variable without preserving the distro's defaults.
#[derive(Debug, Default)]
pub struct OptflagsOverridden {
    diagnostics: Vec<Diagnostic>,
}

impl OptflagsOverridden {
    /// Construct an empty lint state.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Flag-family classification. Each variant maps to one or more shell
/// variable names plus its kind-specific `%{build_*}` macro.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlagKind {
    /// `CFLAGS` — C compiler flags. Preserving macro: `%{build_cflags}`.
    C,
    /// `CXXFLAGS` — C++ compiler flags. Preserving macro: `%{build_cxxflags}`.
    Cxx,
    /// `LDFLAGS` — linker flags. Preserving macro: `%{build_ldflags}`.
    Ld,
    /// `FFLAGS` / `FCFLAGS` — Fortran compiler flags. Preserving macro:
    /// `%{build_fflags}` (or `%{build_fcflags}`).
    Fortran,
}

impl FlagKind {
    /// All variables this rule scans for, paired with their kind.
    const VARS: &'static [(&'static str, FlagKind)] = &[
        ("CFLAGS", FlagKind::C),
        ("CXXFLAGS", FlagKind::Cxx),
        ("LDFLAGS", FlagKind::Ld),
        ("FFLAGS", FlagKind::Fortran),
        ("FCFLAGS", FlagKind::Fortran),
    ];

    /// Kind-specific `%{build_*}` markers that count as preserving the
    /// distro defaults for this variable. `%{optflags}` and
    /// `$RPM_OPT_FLAGS` are accepted by every kind separately (see
    /// [`UNIVERSAL_MARKERS`]).
    fn specific_markers(self) -> &'static [&'static str] {
        match self {
            FlagKind::C => &["%{build_cflags}", "%build_cflags"],
            FlagKind::Cxx => &["%{build_cxxflags}", "%build_cxxflags"],
            FlagKind::Ld => &[
                "%{build_ldflags}",
                "%build_ldflags",
                "$RPM_LD_FLAGS",
                "${RPM_LD_FLAGS}",
            ],
            FlagKind::Fortran => &[
                "%{build_fflags}",
                "%build_fflags",
                "%{build_fcflags}",
                "%build_fcflags",
            ],
        }
    }

    /// Human-readable label used in the diagnostic message to point at
    /// the kind-specific preserving macro.
    fn build_macro_label(self) -> &'static str {
        match self {
            FlagKind::C => "%{build_cflags}",
            FlagKind::Cxx => "%{build_cxxflags}",
            FlagKind::Ld => "%{build_ldflags}",
            FlagKind::Fortran => "%{build_fflags}",
        }
    }
}

/// Universal preserving markers — accepted regardless of which flag
/// variable is being assigned. `%{optflags}` is the distro shorthand
/// covering C/C++/Fortran compilation; `$RPM_OPT_FLAGS` is the legacy
/// env-var spelling. Using either is a correct (if coarse) override.
const UNIVERSAL_MARKERS: &[&str] = &[
    "%{optflags}",
    "%optflags",
    "$RPM_OPT_FLAGS",
    "${RPM_OPT_FLAGS}",
];

impl<'ast> Visit<'ast> for OptflagsOverridden {
    fn visit_spec(&mut self, spec: &'ast SpecFile<Span>) {
        for_each_buildscript(spec, |kind, body, section_span| {
            // %install/%check rarely set compile flags; restrict to
            // %build to keep the rule focused.
            if !matches!(kind, rpm_spec::ast::BuildScriptKind::Build) {
                return;
            }
            if let Some(diag) = first_override(body, section_span) {
                self.diagnostics.push(diag);
            }
        });
    }
}

fn first_override(body: &ShellBody, section_span: Span) -> Option<Diagnostic> {
    for line in &body.lines {
        let Some(lit) = line.literal_str() else {
            continue;
        };
        let scan = strip_trailing_comment(lit);
        if let Some((var, kind)) = find_overriding_assignment(scan) {
            let build_macro = kind.build_macro_label();
            return Some(Diagnostic::new(
                &METADATA,
                Severity::Warn,
                format!(
                    "build script assigns `{var}=...` without preserving `%{{optflags}}` / \
                     `$RPM_OPT_FLAGS`; the override drops distro hardening (FORTIFY_SOURCE, \
                     PIE, RELRO, stack-protector). Prefer `{var}=\"{build_macro} ...\"`, \
                     `{var}=\"%{{optflags}} ...\"`, or `{var}+=\"...\"`."
                ),
                section_span,
            ));
        }
    }
    None
}

/// What kind of `VAR…` token we matched in the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssignKind {
    /// `VAR=value` whose value does not contain any preserving marker.
    /// This is the diagnosed case.
    Override,
    /// `VAR=value` whose value references `%{optflags}`,
    /// `$RPM_OPT_FLAGS`, the kind-specific `%{build_*}` macro, or a
    /// self-reference like `$VAR` / `${VAR}` — fine, skip.
    Preserving,
    /// `VAR+=value` — append form always preserves the prior value.
    PlusEquals,
    /// The byte sequence at this offset is not actually an assignment
    /// to `var` (e.g. embedded in a larger identifier, or followed by
    /// neither `=` nor `+=`).
    NotAnAssignment,
}

/// Walk the line text scanning for `FOO=...` assignments where `FOO`
/// is in [`FlagKind::VARS`]. Returns the matched var name and its
/// kind when the assignment's value does not contain any preserving
/// marker.
fn find_overriding_assignment(s: &str) -> Option<(&'static str, FlagKind)> {
    let bytes = s.as_bytes();
    let mut i = 0;
    // Strict `<` here (not `<=`): we always need at least one more
    // byte after the variable name to hold the `=` or `+`, so an
    // exact-end match like `"…CFLAGS"` cannot start an assignment.
    while i < bytes.len() {
        let mut matched_len = 0;
        for &(var, kind) in FlagKind::VARS {
            let var_b = var.as_bytes();
            // See above: we need at least one byte after the name.
            if i + var_b.len() < bytes.len() && &bytes[i..i + var_b.len()] == var_b {
                let left_ok = i == 0 || !is_name_char(bytes[i - 1]);
                if left_ok {
                    match parse_assignment_at(s, i, var, kind) {
                        Some(AssignKind::Override) => return Some((var, kind)),
                        Some(AssignKind::Preserving | AssignKind::PlusEquals) => {
                            // Advance past `VAR=value` or `VAR+=value`
                            // so we don't re-match the same span as a
                            // different kind.
                            matched_len = consume_assignment(s, i, var);
                            break;
                        }
                        Some(AssignKind::NotAnAssignment) | None => {}
                    }
                }
            }
        }
        if matched_len > 0 {
            i += matched_len;
        } else {
            i += 1;
        }
    }
    None
}

/// Classify the byte sequence at offset `i` starting with `var` as a
/// shell assignment. Caller has already verified the left-side word
/// boundary and that `bytes[i..i+var.len()] == var`.
fn parse_assignment_at(s: &str, i: usize, var: &str, kind: FlagKind) -> Option<AssignKind> {
    let bytes = s.as_bytes();
    let after = i + var.len();
    if after >= bytes.len() {
        return Some(AssignKind::NotAnAssignment);
    }
    // `FOO+=` — append form, always preserves.
    if bytes[after] == b'+' && after + 1 < bytes.len() && bytes[after + 1] == b'=' {
        return Some(AssignKind::PlusEquals);
    }
    if bytes[after] == b'=' {
        let value = extract_assignment_value(&s[after + 1..]);
        if value_preserves_optflags(value, var, kind) {
            return Some(AssignKind::Preserving);
        }
        return Some(AssignKind::Override);
    }
    Some(AssignKind::NotAnAssignment)
}

/// Return the number of bytes occupied by `VAR=value` / `VAR+=value`
/// at offset `i`, so the scan loop can step past it.
fn consume_assignment(s: &str, i: usize, var: &str) -> usize {
    let bytes = s.as_bytes();
    let after = i + var.len();
    if after >= bytes.len() {
        return var.len();
    }
    if bytes[after] == b'+' && after + 1 < bytes.len() && bytes[after + 1] == b'=' {
        let value = extract_assignment_value(&s[after + 2..]);
        return var.len() + 2 + value.len();
    }
    if bytes[after] == b'=' {
        let value = extract_assignment_value(&s[after + 1..]);
        return var.len() + 1 + value.len();
    }
    var.len()
}

/// Pull the assignment's value from the slice that starts immediately
/// after `=`. Strips an optional surrounding `"…"` or `'…'`. The value
/// extends to the first unquoted whitespace.
fn extract_assignment_value(rest: &str) -> &str {
    let bytes = rest.as_bytes();
    if bytes.is_empty() {
        return "";
    }
    match bytes[0] {
        b'"' | b'\'' => {
            let quote = bytes[0];
            let mut j = 1;
            while j < bytes.len() && bytes[j] != quote {
                j += 1;
            }
            &rest[1..j.min(bytes.len())]
        }
        _ => {
            let mut j = 0;
            while j < bytes.len() && !bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            &rest[..j]
        }
    }
}

/// `true` when `value` contains a preserving marker for this `kind`
/// (`%{optflags}`, `$RPM_OPT_FLAGS`, the kind-specific `%{build_*}`
/// macro, or `$VAR` self-reference).
fn value_preserves_optflags(value: &str, var: &str, kind: FlagKind) -> bool {
    if UNIVERSAL_MARKERS.iter().any(|m| value.contains(m)) {
        return true;
    }
    if kind.specific_markers().iter().any(|m| value.contains(m)) {
        return true;
    }
    // Self-reference `$CFLAGS` / `${CFLAGS}` — appending to the prior
    // value preserves whatever was there.
    let bare = format!("${var}");
    if value.contains(&bare) {
        return true;
    }
    let braced = format!("${{{var}}}");
    if value.contains(&braced) {
        return true;
    }
    false
}

#[inline]
fn is_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

impl Lint for OptflagsOverridden {
    fn metadata(&self) -> &'static LintMetadata {
        &METADATA
    }
    fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::parse;

    fn run(src: &str) -> Vec<Diagnostic> {
        let outcome = parse(src);
        let mut lint = OptflagsOverridden::new();
        lint.visit_spec(&outcome.spec);
        lint.take_diagnostics()
    }

    #[test]
    fn flags_bare_cflags_override() {
        let src = "Name: x\n%build\nmake CFLAGS=-O2\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM385");
    }

    #[test]
    fn flags_quoted_override() {
        let src = "Name: x\n%build\nmake CFLAGS=\"-O2 -g\"\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_ldflags_override() {
        let src = "Name: x\n%build\nmake LDFLAGS=-Wl,--as-needed\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn flags_export_bare_cflags() {
        // `export CFLAGS="-O2"` in %build with no preserving marker
        // should still fire. The `export` keyword does not change the
        // override semantics.
        let src = "Name: x\n%build\nexport CFLAGS=\"-O2\"\nmake\n";
        let diags = run(src);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].lint_id, "RPM385");
    }

    #[test]
    fn silent_when_optflags_macro_referenced() {
        let src = "Name: x\n%build\nmake CFLAGS=\"%{optflags} -DFOO\"\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_rpm_opt_flags_env_used() {
        let src = "Name: x\n%build\nmake CFLAGS=\"$RPM_OPT_FLAGS -DFOO\"\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_braced_env_used() {
        let src = "Name: x\n%build\nmake CFLAGS=\"${RPM_OPT_FLAGS} -DFOO\"\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_build_cflags_referenced() {
        let src = "Name: x\n%build\nmake CFLAGS=\"%{build_cflags} -DFOO\"\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_self_reference_appended() {
        // `CFLAGS="$CFLAGS …"` preserves the prior setting.
        let src = "Name: x\n%build\nmake CFLAGS=\"$CFLAGS -DFOO\"\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_when_plus_assign_used() {
        // `CFLAGS+=…` always preserves.
        let src = "Name: x\n%build\nexport CFLAGS+=\" -DFOO\"\nmake\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn deduplicates_per_section() {
        // Multiple overriding assignments → one diagnostic.
        let src = "Name: x\n%build\nmake CFLAGS=-O2\nmake CXXFLAGS=-O2\n";
        assert_eq!(run(src).len(), 1);
    }

    #[test]
    fn silent_in_install_section() {
        // Rule is scoped to %build.
        let src = "Name: x\n%install\nmake CFLAGS=-O2 install\n";
        assert!(run(src).is_empty());
    }

    #[test]
    fn silent_for_other_var_with_cflags_substring() {
        // `MY_CFLAGS=` is not the standard variable; word boundary
        // check must prevent a false match.
        let src = "Name: x\n%build\nmake MY_CFLAGS=-O2\n";
        assert!(run(src).is_empty());
    }
}
