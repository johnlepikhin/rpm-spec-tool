//! Phase 1 "missing required tag" lints.
//!
//! Six near-identical rules (RPM010–RPM015) — one per mandatory preamble
//! tag. They share a generic visit body that fires when no top-level
//! `PreambleItem` carries the named [`Tag`]. The body lives in
//! [`crate::declare_missing_tag_lint`]; each entry below is just
//! metadata + matcher + the fixtures used by the auto-generated tests.

// `Tag` and `Severity` look unused here because they appear inside macro
// invocations only — they expand into the generated nested modules,
// each of which brings its own `use`s.
use crate::declare_missing_tag_lint;

declare_missing_tag_lint! {
    mod name_tag,
    struct MissingNameTag,
    id: "RPM010",
    name: "missing-name-tag",
    description: "Spec file must declare a top-level Name: tag.",
    severity: Severity::Deny,
    tag: Tag::Name,
    message: "spec is missing the Name: tag",
    good_fixture: "Name: hello\nVersion: 1\n",
    bad_fixture: "Version: 1\n",
}

declare_missing_tag_lint! {
    mod version_tag,
    struct MissingVersionTag,
    id: "RPM011",
    name: "missing-version-tag",
    description: "Spec file must declare a top-level Version: tag.",
    severity: Severity::Deny,
    tag: Tag::Version,
    message: "spec is missing the Version: tag",
    good_fixture: "Name: x\nVersion: 1\n",
    bad_fixture: "Name: x\n",
}

declare_missing_tag_lint! {
    mod release_tag,
    struct MissingReleaseTag,
    id: "RPM012",
    name: "missing-release-tag",
    description: "Spec file must declare a top-level Release: tag.",
    severity: Severity::Deny,
    tag: Tag::Release,
    message: "spec is missing the Release: tag",
    good_fixture: "Name: x\nVersion: 1\nRelease: 1\n",
    bad_fixture: "Name: x\nVersion: 1\n",
}

declare_missing_tag_lint! {
    mod license_tag,
    struct MissingLicenseTag,
    id: "RPM013",
    name: "missing-license-tag",
    description: "Spec file must declare a top-level License: tag.",
    severity: Severity::Deny,
    tag: Tag::License,
    message: "spec is missing the License: tag",
    good_fixture: "Name: x\nVersion: 1\nRelease: 1\nLicense: MIT\n",
    bad_fixture: "Name: x\nVersion: 1\nRelease: 1\n",
}

declare_missing_tag_lint! {
    mod summary_tag,
    struct MissingSummaryTag,
    id: "RPM014",
    name: "missing-summary-tag",
    description: "Spec file must declare a top-level Summary: tag.",
    severity: Severity::Deny,
    tag: Tag::Summary,
    message: "spec is missing the Summary: tag",
    good_fixture: "Name: x\nSummary: hello world\n",
    bad_fixture: "Name: x\n",
}

declare_missing_tag_lint! {
    mod url_tag,
    struct MissingUrlTag,
    id: "RPM015",
    name: "missing-url-tag",
    description: "Spec file should declare a top-level URL: tag.",
    severity: Severity::Warn,
    tag: Tag::URL,
    message: "spec is missing the URL: tag",
    good_fixture: "Name: x\nURL: https://example.com\n",
    bad_fixture: "Name: x\n",
}

// Re-export the rule structs so consumers (registry, future tests) can
// reach them as `rules::missing_tag::MissingNameTag` instead of the
// nested module path.
pub use license_tag::MissingLicenseTag;
pub use name_tag::MissingNameTag;
pub use release_tag::MissingReleaseTag;
pub use summary_tag::MissingSummaryTag;
pub use url_tag::MissingUrlTag;
pub use version_tag::MissingVersionTag;
