//! Epoch-Version-Release tuple with the rpm `rpmvercmp` algorithm.
//!
//! The comparison is a verbatim port of the algorithm documented in
//! `rpmio/rpmvercmp.c` of upstream rpm. It splits each string into
//! runs of digits or letters, compares same-kind runs numerically or
//! lexicographically, and treats `~` as "older than empty" (used for
//! pre-releases like `1.0~rc1`) and `^` as "newer than empty" (used
//! for post-release snapshots like `1.0^20240101`).
//!
//! Epoch is the first compared component and is treated as `0` when
//! absent. Two EVRs with equal epoch fall through to version, then
//! release.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};

/// Epoch-Version-Release. `epoch` defaults to `0` when missing in the
/// source (RPM treats absent and `0` as equivalent).
#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct EVR {
    #[serde(default)]
    pub epoch: u32,
    pub version: String,
    pub release: String,
}

impl EVR {
    /// Construct from parts. `epoch = None` is normalised to `0`.
    #[must_use]
    pub fn new(epoch: Option<u32>, version: impl Into<String>, release: impl Into<String>) -> Self {
        Self {
            epoch: epoch.unwrap_or(0),
            version: version.into(),
            release: release.into(),
        }
    }

    /// Parse an EVR string of the form `[E:]V-R` (e.g. `1:2.3-4.el9`).
    /// Returns `None` on shapes the rpm naming convention disallows
    /// (missing release, multi-colon epoch, empty version).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let (epoch, rest) = match s.split_once(':') {
            Some((e, r)) => (e.parse::<u32>().ok()?, r),
            None => (0, s),
        };
        let (version, release) = rest.rsplit_once('-')?;
        if version.is_empty() || release.is_empty() {
            return None;
        }
        Some(Self {
            epoch,
            version: version.to_string(),
            release: release.to_string(),
        })
    }

    /// Full ordering using the rpm vercmp algorithm. Epoch is compared
    /// first as an integer, then version, then release using
    /// [`rpmvercmp`].
    ///
    /// ALT-style `set:`-versions short-circuit to [`Ordering::Equal`]
    /// when **both** sides carry the prefix. These versions encode an
    /// ABI symbol-set hash (used by ALT's rpm-build to express strict
    /// ELF soname/symbol-subset constraints) and ALT's own
    /// `rpmevrcmp` resolves them with a subset test that requires
    /// base64-decoding the payload — not regular vercmp. Without the
    /// short-circuit, every ALT soname-constraint like
    /// `libfoo.so.0()(64bit) >= set:XXX` evaluates as Less/Greater
    /// against the provider's `set:YYY` and produces false-positive
    /// UNSAT. Dropping to equality means a name match alone counts
    /// as satisfied (the same behaviour dnf+yum effectively exhibit
    /// on rpm-md repos, which don't carry set-versions at all).
    ///
    /// Empty-release side: if either operand carries an empty
    /// `release`, the release leg of the comparison is skipped.
    /// Real-world requires like `Requires: foo = 2.84.4` (no dash,
    /// no dist tag) are intentionally version-only — dnf/yum treat
    /// them as "any release with this version". Without this leg
    /// skip, every such require false-fails against an actual repo
    /// provider whose release is non-empty (e.g. `alt1.p11`).
    #[must_use]
    pub fn compare_rpm(&self, other: &Self) -> Ordering {
        if self.version.starts_with("set:") && other.version.starts_with("set:") {
            return Ordering::Equal;
        }
        match self.epoch.cmp(&other.epoch) {
            Ordering::Equal => {}
            ne => return ne,
        }
        match rpmvercmp(&self.version, &other.version) {
            Ordering::Equal => {}
            ne => return ne,
        }
        // Either side missing a release → version-only match. The
        // common case is a spec written `Requires: foo = X.Y` against
        // a repo provider with full `X.Y-altN.p11`; that's not a
        // mismatch, that's the user saying "any release of X.Y".
        if self.release.is_empty() || other.release.is_empty() {
            return Ordering::Equal;
        }
        rpmvercmp(&self.release, &other.release)
    }
}

impl Ord for EVR {
    fn cmp(&self, other: &Self) -> Ordering {
        self.compare_rpm(other)
    }
}

impl PartialOrd for EVR {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for EVR {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.epoch == 0 {
            write!(f, "{}-{}", self.version, self.release)
        } else {
            write!(f, "{}:{}-{}", self.epoch, self.version, self.release)
        }
    }
}

/// Port of upstream rpm's `rpmvercmp`. ASCII only — non-ASCII bytes
/// fall through `is_ascii_alphanumeric` as "neither" and split runs
/// the same way the C implementation does (`isalpha`/`isdigit` are
/// locale-influenced in C; we pin to ASCII for portability).
///
/// Algorithm summary:
/// 1. Skip leading non-alphanumeric bytes in both strings, except `~`
///    (older than empty) and `^` (newer than empty).
/// 2. If either side starts with `~`, that side sorts older.
/// 3. If only one side starts with `^`, the `^` side sorts older when
///    facing end-of-string but newer when facing a non-end character.
/// 4. Otherwise extract a run of digits or letters from both sides
///    (must be the same kind — digit runs always beat letter runs).
/// 5. Numeric runs: strip leading zeros; longer string wins; otherwise
///    lexicographic. Letter runs: lexicographic.
/// 6. Continue until one side is exhausted.
#[must_use]
pub fn rpmvercmp(a: &str, b: &str) -> Ordering {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let (mut i, mut j) = (0usize, 0usize);

    while i < a.len() || j < b.len() {
        // Skip non-alnum separators (but treat `~` and `^` specially).
        while i < a.len() && !is_alnum(a[i]) && a[i] != b'~' && a[i] != b'^' {
            i += 1;
        }
        while j < b.len() && !is_alnum(b[j]) && b[j] != b'~' && b[j] != b'^' {
            j += 1;
        }

        // Tilde: "older than empty". Any segment starting with `~` is
        // older than the same segment without one.
        let a_tilde = i < a.len() && a[i] == b'~';
        let b_tilde = j < b.len() && b[j] == b'~';
        if a_tilde || b_tilde {
            if !a_tilde {
                return Ordering::Greater;
            }
            if !b_tilde {
                return Ordering::Less;
            }
            i += 1;
            j += 1;
            continue;
        }

        // Caret: post-release snapshot marker. Semantics from
        // upstream `rpmvercmp`:
        //   - `1.0^` vs `1.0`   → `1.0^` is *newer* (Greater): a side
        //     that still has any content beats one that's empty.
        //   - `1.0^x` vs `1.0`  → `1.0^x` Greater for the same reason.
        //   - `1.0^x` vs `1.0.1` → `1.0^x` is *older* (Less): a `^`
        //     run loses to any plain alnum run on the other side.
        // The end-of-string checks must run before the "only one side
        // has caret" branches because position alone doesn't tell us
        // which side is empty.
        let a_caret = i < a.len() && a[i] == b'^';
        let b_caret = j < b.len() && b[j] == b'^';
        if a_caret || b_caret {
            if i == a.len() {
                return Ordering::Less;
            }
            if j == b.len() {
                return Ordering::Greater;
            }
            if !b_caret {
                return Ordering::Less;
            }
            if !a_caret {
                return Ordering::Greater;
            }
            i += 1;
            j += 1;
            continue;
        }

        if i >= a.len() || j >= b.len() {
            break;
        }

        // Extract runs of digits or letters (same kind on both sides).
        let a_digit = a[i].is_ascii_digit();
        let b_digit = b[j].is_ascii_digit();

        if a_digit != b_digit {
            // Digit run always beats letter run.
            return if a_digit { Ordering::Greater } else { Ordering::Less };
        }

        let a_end = if a_digit {
            i + a[i..].iter().take_while(|c| c.is_ascii_digit()).count()
        } else {
            i + a[i..].iter().take_while(|c| c.is_ascii_alphabetic()).count()
        };
        let b_end = if b_digit {
            j + b[j..].iter().take_while(|c| c.is_ascii_digit()).count()
        } else {
            j + b[j..].iter().take_while(|c| c.is_ascii_alphabetic()).count()
        };

        let (mut a_seg, mut b_seg) = (&a[i..a_end], &b[j..b_end]);

        if a_digit {
            // Strip leading zeros, then compare by length; equal length
            // → byte-wise (which is decimal-correct since both are
            // pure digit runs of identical length).
            while a_seg.len() > 1 && a_seg[0] == b'0' {
                a_seg = &a_seg[1..];
            }
            while b_seg.len() > 1 && b_seg[0] == b'0' {
                b_seg = &b_seg[1..];
            }
            match a_seg.len().cmp(&b_seg.len()) {
                Ordering::Equal => match a_seg.cmp(b_seg) {
                    Ordering::Equal => {}
                    ne => return ne,
                },
                ne => return ne,
            }
        } else {
            match a_seg.cmp(b_seg) {
                Ordering::Equal => {}
                ne => return ne,
            }
        }

        i = a_end;
        j = b_end;
    }

    // Match upstream `rpmvercmp` end-of-loop semantics: after both
    // positions have been advanced past all alnum and tilde/caret
    // content, neither side having more *meaningful* content means
    // equal; only one side having more means that side is newer
    // (more content == longer version string == newer). We compare
    // by scanning for residual alnum / tilde / caret rather than by
    // total length so leading-zero-equivalent strings like "0.99"
    // and "00.99" compare equal despite differing byte lengths.
    let a_has_more = (i..a.len()).any(|k| is_alnum(a[k]) || a[k] == b'~' || a[k] == b'^');
    let b_has_more = (j..b.len()).any(|k| is_alnum(b[k]) || b[k] == b'~' || b[k] == b'^');
    match (a_has_more, b_has_more) {
        (false, false) => Ordering::Equal,
        (false, true) => Ordering::Less,
        (true, false) => Ordering::Greater,
        (true, true) => Ordering::Equal,
    }
}

#[inline]
fn is_alnum(c: u8) -> bool {
    c.is_ascii_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering::{Equal, Greater, Less};

    /// Test vectors derived from rpm's own `tests/rpmvercmp.at` plus
    /// extras for tilde and caret semantics introduced in rpm 4.10+.
    /// Each row is `(a, b, expected)`.
    const VECTORS: &[(&str, &str, Ordering)] = &[
        // Trivially equal
        ("1.0", "1.0", Equal),
        ("1.0", "1.0", Equal),
        // Numeric ordering
        ("1.0", "2.0", Less),
        ("2.0", "1.0", Greater),
        ("2.0.1", "2.0.1", Equal),
        ("2.0", "2.0.1", Less),
        ("2.0.1", "2.0", Greater),
        // Mixed alpha and numeric
        ("2.0.1a", "2.0.1a", Equal),
        ("2.0.1a", "2.0.1", Greater),
        ("2.0.1", "2.0.1a", Less),
        // Letters
        ("5.5p1", "5.5p1", Equal),
        ("5.5p1", "5.5p2", Less),
        ("5.5p2", "5.5p1", Greater),
        ("5.5p10", "5.5p10", Equal),
        ("5.5p1", "5.5p10", Less),
        ("5.5p10", "5.5p1", Greater),
        // Numeric vs alpha at run boundary (digit run wins)
        ("10abc", "10.1abc", Less),
        // Leading zeros
        ("0.99", "00.99", Equal),
        ("1.0010", "1.10", Equal),
        // Tilde: older than empty
        ("1.0~rc1", "1.0", Less),
        ("1.0", "1.0~rc1", Greater),
        ("1.0~rc1", "1.0~rc2", Less),
        ("1.0~rc1~git1", "1.0~rc1", Less),
        ("1.0~rc1", "1.0~rc1~git1", Greater),
        // Caret: post-release snapshot
        ("1.0^", "1.0", Greater),
        ("1.0^", "1.0^", Equal),
        ("1.0^git1", "1.0^git1", Equal),
        ("1.0^git1", "1.0^git2", Less),
        ("1.0^git1", "1.0", Greater),
        ("1.0", "1.0^git1", Less),
        ("1.0^20240101", "1.0.1", Less),
        ("1.0.1", "1.0^20240101", Greater),
        ("1.0^20240101^git1", "1.0^20240101", Greater),
        // Separators are not part of the comparison
        ("1.0", "1_0", Equal),
        ("1.0-1", "1.0-1", Equal),
    ];

    #[test]
    fn rpmvercmp_vectors() {
        for &(a, b, expected) in VECTORS {
            let got = rpmvercmp(a, b);
            assert_eq!(got, expected, "rpmvercmp({a:?}, {b:?}) = {got:?}, want {expected:?}");
        }
    }

    #[test]
    fn evr_parse_with_epoch() {
        let e = EVR::parse("1:2.3-4.el9").unwrap();
        assert_eq!(e.epoch, 1);
        assert_eq!(e.version, "2.3");
        assert_eq!(e.release, "4.el9");
    }

    #[test]
    fn evr_parse_without_epoch() {
        let e = EVR::parse("2.3-4.el9").unwrap();
        assert_eq!(e.epoch, 0);
        assert_eq!(e.version, "2.3");
        assert_eq!(e.release, "4.el9");
    }

    #[test]
    fn evr_parse_release_keeps_dist_dashes() {
        // `version` is everything before the LAST `-`. Real-world
        // releases include dist tags with dots, never dashes.
        let e = EVR::parse("1.2.3-4.el9").unwrap();
        assert_eq!(e.version, "1.2.3");
        assert_eq!(e.release, "4.el9");
    }

    #[test]
    fn evr_parse_rejects_missing_release() {
        assert!(EVR::parse("1.0").is_none());
        assert!(EVR::parse("1:1.0").is_none());
    }

    #[test]
    fn evr_epoch_wins() {
        let a = EVR::new(Some(0), "2.0", "1");
        let b = EVR::new(Some(1), "1.0", "1");
        assert!(a < b, "epoch 0 < epoch 1 regardless of version");
    }

    #[test]
    fn evr_display_omits_zero_epoch() {
        assert_eq!(EVR::new(Some(0), "1.0", "1").to_string(), "1.0-1");
        assert_eq!(EVR::new(Some(2), "1.0", "1").to_string(), "2:1.0-1");
    }

    #[test]
    fn alt_set_versions_compare_equal_on_both_sides() {
        // ALT's content-addressable `set:...` ABI hashes don't have
        // a meaningful byte ordering — they're symbol-subset markers
        // resolved by ALT's bespoke rpmevrcmp via base64-decoded
        // subset tests. Short-circuiting to Equal makes constraints
        // like `libfoo.so.0()(64bit) >= set:XXX` resolve against a
        // provider's `= set:YYY` on name-match alone, mirroring how
        // dnf/yum effectively handle sonames on non-ALT repos.
        let a = EVR::new(Some(0), "set:kdafcrS9Ku7yZIO", "");
        let b = EVR::new(Some(0), "set:kgJAZn6CpJkWxvKY7JXz46IBf", "");
        assert_eq!(a.compare_rpm(&b), Ordering::Equal);
        assert_eq!(b.compare_rpm(&a), Ordering::Equal);
    }

    #[test]
    fn empty_release_treated_as_wildcard_match() {
        // `Requires: foo = 2.84.4` (no release) must satisfy against
        // a provider whose release is non-empty. dnf/yum behave the
        // same way; strict rpmvercmp on the release leg would fail
        // every dist-tag-bearing repo.
        let provider = EVR::new(Some(0), "2.84.4", "alt1.p11");
        let require_versionless = EVR::new(Some(0), "2.84.4", "");
        assert_eq!(provider.compare_rpm(&require_versionless), Ordering::Equal);
        assert_eq!(require_versionless.compare_rpm(&provider), Ordering::Equal);

        // Sanity: when neither side has an empty release, normal
        // rpmvercmp on the release leg still applies.
        let require_full = EVR::new(Some(0), "2.84.4", "alt2.p11");
        // alt1 < alt2 lexicographically (and by rpmvercmp).
        assert_eq!(provider.compare_rpm(&require_full), Ordering::Less);
    }

    #[test]
    fn set_version_against_normal_version_falls_through() {
        // One side `set:` and the other plain — well-formed ALT
        // pkglists never mix these for the same capability, but if
        // we see it, fall back to byte-by-byte rpmvercmp (the
        // historic behaviour). The point of the short-circuit is
        // ALT's symmetric set-vs-set case, not to leak `set:`
        // semantics into rpm-md repos.
        let set_side = EVR::new(Some(0), "set:abc", "");
        let plain = EVR::new(Some(0), "1.0", "");
        // Just verify it doesn't panic and is consistent across the
        // two orderings — the exact ordering of `set:abc` vs `1.0`
        // is rpmvercmp's call and not load-bearing for the lint.
        let ab = set_side.compare_rpm(&plain);
        let ba = plain.compare_rpm(&set_side);
        assert_eq!(ab, ba.reverse());
    }
}
