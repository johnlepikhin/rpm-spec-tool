//! Shared URL placeholder interpolation for `repo sync` / `repo show`.
//!
//! Kept in a separate module so `show` doesn't depend on the full
//! sync action surface.
//!
//! Note: unlike dnf, the literal `$$` escape (to produce a single
//! `$` in the output URL) is not supported — `$` is treated as
//! the start of every placeholder reference.

use rpm_spec_analyzer::profile::Profile;

/// Reject placeholder expansion values that could rewrite the
/// resulting URL's host, path, query, or fragment.
///
/// The scheme allowlist in `http.rs` is applied to the final URL
/// string after interpolation; without this check a hostile config
/// like `baseurl = "https://$basearch.example/"` plus
/// `build_arch = "evil.example.com#"` would expand to
/// `https://evil.example.com#.example/...` and `ureq` would dial
/// `evil.example.com`. We therefore restrict expansion values to
/// the chars that are safe inside a single URL *path segment*:
/// alphanumeric plus `.`, `_`, `-`. Empty values are allowed (the
/// resulting URL may 404 but won't redirect the request).
fn validate_placeholder_value(name: &str, value: &str) -> Result<(), String> {
    for c in value.chars() {
        let ok = c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
        if !ok {
            return Err(format!(
                "placeholder ${name} expanded to {value:?} which contains \
                 disallowed character {c:?}; only [A-Za-z0-9._-] are permitted \
                 to prevent URL rewriting via host/path injection"
            ));
        }
    }
    Ok(())
}

/// Resolve `$basearch` / `$arch` / `$releasever` / `$infra` against
/// the profile's identity + arch. Unknown variables → error so
/// typos don't silently produce broken URLs.
///
/// Returned as `Result<String, String>` so unknown placeholders are
/// surfaced as user-fixable errors rather than `io::Error`.
pub(crate) fn interpolate_url(url: &str, profile: &Profile) -> Result<String, String> {
    let basearch = profile.arch.build_arch.as_deref().unwrap_or("");
    // RPM convention: $arch == $basearch (no override exposed in M1).
    let arch = basearch;
    let releasever = profile
        .identity
        .dist_tag
        .as_deref()
        .map(|d| d.trim_start_matches('.').trim_start_matches("el").to_string())
        .unwrap_or_default();
    let infra = "stock"; // dnf default; not configurable in M1

    let mut out = String::with_capacity(url.len());
    let bytes = url.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'$' {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }
        let rest = &url[i + 1..];
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .unwrap_or(rest.len());
        let name = &rest[..end];
        let value = match name {
            "basearch" => basearch.to_string(),
            "arch" => arch.to_string(),
            "releasever" => releasever.clone(),
            "infra" => infra.to_string(),
            "" => {
                out.push('$');
                i += 1;
                continue;
            }
            other => {
                return Err(format!("unknown URL placeholder `${other}` in {url}"));
            }
        };
        validate_placeholder_value(name, &value)?;
        out.push_str(&value);
        i += 1 + end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_analyzer::profile::Profile;

    /// Helper: build a `Profile` with a concrete `build_arch`. All other
    /// fields default. `#[non_exhaustive]` allows construction via
    /// `Default::default()` plus pub-field mutation in the same crate
    /// tree (and `Profile`'s fields are all `pub`).
    fn profile_with_arch(arch: Option<&str>) -> Profile {
        let mut p = Profile::default();
        p.arch.build_arch = arch.map(String::from);
        p
    }

    #[test]
    fn interpolates_basearch_simple() {
        let p = profile_with_arch(Some("x86_64"));
        let got = interpolate_url("https://repo.example/$basearch/os/", &p).unwrap();
        assert_eq!(got, "https://repo.example/x86_64/os/");
    }

    #[test]
    fn rejects_path_traversal_in_placeholder() {
        let p = profile_with_arch(Some("../../etc"));
        let err = interpolate_url("https://repo.example/$basearch/os/", &p)
            .expect_err("path-traversal value must be rejected");
        // The error must call out the disallowed character. `/` is the
        // first disallowed byte the scan trips on.
        assert!(
            err.contains("disallowed character"),
            "error message should mention the disallowed character: {err}"
        );
        assert!(
            err.contains("'/'") || err.contains("'.'"),
            "error message should quote the offending char: {err}"
        );
    }

    #[test]
    fn rejects_host_rewrite_in_placeholder() {
        let p = profile_with_arch(Some("evil.example.com#"));
        let err = interpolate_url("https://$basearch.repo.example/os/", &p)
            .expect_err("value with `#` must be rejected to prevent URL rewriting");
        assert!(
            err.contains("disallowed character"),
            "error message should mention the disallowed character: {err}"
        );
    }

    #[test]
    fn unknown_placeholder_errors() {
        let p = Profile::default();
        let err = interpolate_url("https://repo.example/$wat/os/", &p)
            .expect_err("unknown placeholder must error");
        assert!(
            err.contains("unknown URL placeholder"),
            "error should describe the unknown placeholder: {err}"
        );
        assert!(err.contains("$wat"), "error should name `$wat`: {err}");
    }

    #[test]
    fn bare_dollar_literal() {
        let p = Profile::default();
        // A trailing `$` with no identifier following is preserved verbatim
        // (see the `""` arm in `interpolate_url`).
        let got = interpolate_url("https://repo.example/path/$", &p).unwrap();
        assert_eq!(got, "https://repo.example/path/$");
    }

    #[test]
    fn empty_releasever_when_dist_tag_absent() {
        // No `dist_tag` set → `$releasever` interpolates to "".
        let p = Profile::default();
        let got = interpolate_url("https://repo.example/$releasever/os/", &p).unwrap();
        assert_eq!(got, "https://repo.example//os/");
    }

    #[test]
    fn validate_placeholder_value_accepts_safe_chars() {
        // Alphanumerics plus `.`, `_`, `-` are all permitted.
        assert!(validate_placeholder_value("basearch", "x86_64").is_ok());
        assert!(validate_placeholder_value("releasever", "9.4").is_ok());
        assert!(validate_placeholder_value("infra", "Stock-1_2.3").is_ok());
        // Empty value is allowed (the resulting URL may 404, but the
        // request won't be redirected to a different host).
        assert!(validate_placeholder_value("releasever", "").is_ok());
    }

    #[test]
    fn validate_placeholder_value_rejects_each_unsafe_char() {
        // Table-driven: each char on its own line must trip the guard.
        // Chosen to cover the URL-structural metachars (`:`, `/`, `?`,
        // `#`, `@`), the percent-encoding sigil (`%`), whitespace
        // (` `, `\n`, `\t`), and a couple of common path attack chars.
        let bad_chars = ['/', ':', '?', '#', '@', '%', ' ', '\n', '\t', '\\', '.', '_', '-']
            .into_iter()
            // Keep only the chars that are actually disallowed — `.`,
            // `_`, `-` are the safe-set and must NOT trip the guard.
            .filter(|c| !matches!(c, '.' | '_' | '-'))
            .collect::<Vec<_>>();
        for c in bad_chars {
            let value = format!("a{c}b");
            let err = validate_placeholder_value("basearch", &value).expect_err(
                &format!("char {c:?} should have been rejected (value={value:?})"),
            );
            assert!(
                err.contains("disallowed character"),
                "error for {c:?} should mention `disallowed character`: {err}"
            );
        }
    }
}
