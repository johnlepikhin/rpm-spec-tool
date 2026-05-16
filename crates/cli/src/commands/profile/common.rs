//! `profile common` — intersection of macros across two or more
//! profiles, by existence (default) or by value (`--mode value`).

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::profile::{MacroEntry, Profile};

use super::fmt::{MAX_MACRO_LABEL_WIDTH, compact_value, format_opts};
use super::{all_profile_names, resolve_many};

#[derive(Debug, Args)]
pub struct CommonOpts {
    /// Comparison mode. `existence` (default) intersects by macro name
    /// only. `value` adds the requirement that `opts` + macro body
    /// match across every profile; provenance is ignored.
    #[arg(long, value_enum, default_value_t = CommonMode::Existence)]
    pub mode: CommonMode,
    /// Case-insensitive substring filter on macro names.
    #[arg(long)]
    pub filter: Option<String>,
    /// Profiles to intersect. With none, every available profile that
    /// has a non-empty macro registry is used (drops `generic` and
    /// any empty user-defined profiles). At least two are required —
    /// intersecting a single profile is rejected.
    pub profiles: Vec<String>,

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,
}

/// Comparison mode for `profile common`. `Existence` matches by name
/// only; `Value` adds equality of `opts` + body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CommonMode {
    /// Macro is "common" when present in every profile, regardless of value.
    Existence,
    /// Macro is "common" only when present AND the resolved entry matches
    /// (same `opts`, same body — provenance is ignored).
    Value,
}

impl CommonMode {
    /// True when `a` and `b` count as the same entry under this mode.
    fn matches(self, a: &MacroEntry, b: &MacroEntry) -> bool {
        match self {
            CommonMode::Existence => true,
            CommonMode::Value => a.is_equivalent(b),
        }
    }

    fn header_label(self) -> &'static str {
        match self {
            CommonMode::Existence => "Common macros",
            CommonMode::Value => "Macros with identical values",
        }
    }
}

/// Dispatch for `profile common`:
///   0 args → resolve all available, drop empty registries, then intersect
///   1 arg  → reject with exit 2
///   2+     → resolve listed and intersect
pub(super) fn dispatch_common(
    out: &mut impl Write,
    config: &Config,
    base_dir: &Path,
    opts: CommonOpts,
) -> Result<ExitCode> {
    let auto_expanded = opts.profiles.is_empty();
    let resolved: Vec<(String, Profile)> = if auto_expanded {
        default_intersection_set(config, base_dir, &opts.defines.raw)?
    } else {
        resolve_many(config, base_dir, &opts.profiles, &opts.defines.raw)?
    };
    if resolved.len() < 2 {
        if auto_expanded {
            eprintln!(
                "error: `profile common` auto-expanded default set has fewer than 2 non-empty profiles (got {})",
                resolved.len()
            );
        } else {
            eprintln!(
                "error: `profile common` needs at least two profiles to intersect (got {})",
                resolved.len()
            );
        }
        return Ok(ExitCode::from(2));
    }
    render_common(out, &opts, &resolved)?;
    Ok(ExitCode::SUCCESS)
}

/// Default profile set for `profile common` when the user passes no
/// names: every available profile with a non-empty macro registry.
/// Drops `generic` (and any empty user profile) so the default
/// intersection isn't vacuous. Returns the already-resolved profiles
/// so the caller doesn't pay for a second resolve pass.
fn default_intersection_set(
    config: &Config,
    base_dir: &Path,
    defines: &[String],
) -> Result<Vec<(String, Profile)>> {
    let mut out = Vec::new();
    for name in all_profile_names(config) {
        let p = config
            .resolve_profile(
                base_dir,
                rpm_spec_analyzer::profile::ResolveOptions::with_override(Some(&name))
                    .with_defines(defines),
            )
            .with_context(|| format!("failed to resolve profile `{name}` (default common set)"))?;
        if !p.macros.is_empty() {
            out.push((name, p));
        }
    }
    Ok(out)
}

/// Render the intersection of `resolved` macro registries.
///
/// Caller resolves the profiles (see `resolve_many` /
/// `default_intersection_set`). Bails if `resolved.len() < 2` —
/// dispatch is the only legitimate caller and guarantees that.
fn render_common(
    out: &mut impl Write,
    opts: &CommonOpts,
    resolved: &[(String, Profile)],
) -> Result<()> {
    let (_first_name, first) = resolved
        .first()
        .ok_or_else(|| anyhow::anyhow!("render_common: empty profile set"))?;
    let rest = &resolved[1..];
    if rest.is_empty() {
        anyhow::bail!("render_common: needs at least 2 profiles, got 1");
    }

    let filter_lc = opts.filter.as_deref().map(str::to_ascii_lowercase);

    // Single intersection pass. `unfiltered` is the full intersection
    // independent of `--filter`; `common` is `unfiltered` ∩ (name
    // matches filter). This lets the header report both "total" and
    // "matching" counts without redoing the predicate.
    let unfiltered: Vec<(&str, &MacroEntry)> = first
        .macros
        .entries
        .iter()
        .filter(|(name, entry)| intersect_predicate(name, entry, rest, opts.mode))
        .map(|(n, e)| (n.as_str(), e))
        .collect();

    let common: Vec<(&str, &MacroEntry)> = match &filter_lc {
        Some(needle) => unfiltered
            .iter()
            .copied()
            .filter(|(n, _)| n.to_ascii_lowercase().contains(needle))
            .collect(),
        None => unfiltered.clone(),
    };

    let mode_label = opts.mode.header_label();
    let header_suffix = match opts.filter.as_deref() {
        Some(orig) => format!(
            "{} total, {} matching \"{orig}\"",
            unfiltered.len(),
            common.len()
        ),
        None => format!("{}", common.len()),
    };
    writeln!(
        out,
        "# {mode_label} across {} profile(s): {header_suffix}",
        resolved.len()
    )?;
    writeln!(out)?;

    if common.is_empty() {
        writeln!(out, "  (no common macros)")?;
        return Ok(());
    }

    match opts.mode {
        CommonMode::Value => {
            let name_width = common
                .iter()
                .map(|(n, e)| n.len() + format_opts(e.opts.as_deref()).len())
                .max()
                .unwrap_or(0)
                .min(MAX_MACRO_LABEL_WIDTH);
            for (name, entry) in common {
                let opts_str = format_opts(entry.opts.as_deref());
                let label = format!("{name}{opts_str}");
                writeln!(
                    out,
                    "  {label:<name_width$} = {}",
                    compact_value(&entry.value),
                )?;
            }
        }
        CommonMode::Existence => {
            for (name, _) in common {
                writeln!(out, "  {name}")?;
            }
        }
    }
    Ok(())
}

/// True when `entry` (from the first profile) is "present" in every
/// `rest` profile under `mode`'s definition. Reused by both the main
/// `common` collection and unit tests.
fn intersect_predicate(
    name: &str,
    entry: &MacroEntry,
    rest: &[(String, Profile)],
    mode: CommonMode,
) -> bool {
    rest.iter().all(|(_, p)| match p.macros.get(name) {
        Some(other) => mode.matches(entry, other),
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_analyzer::profile::Provenance;

    fn make_profile(macros: &[(&str, &str)]) -> Profile {
        let mut p = Profile::default();
        for (name, body) in macros {
            p.macros
                .insert(*name, MacroEntry::literal(*body, Provenance::Override));
        }
        p
    }

    #[test]
    fn intersect_predicate_existence_mode_ignores_value_difference() {
        let entry = MacroEntry::literal("a", Provenance::Override);
        let rest = vec![("p2".to_string(), make_profile(&[("foo", "b")]))];
        // Same name, different value — Existence accepts, Value rejects.
        assert!(intersect_predicate(
            "foo",
            &entry,
            &rest,
            CommonMode::Existence
        ));
        assert!(!intersect_predicate(
            "foo",
            &entry,
            &rest,
            CommonMode::Value
        ));
    }

    #[test]
    fn intersect_predicate_misses_when_name_absent_in_any_profile() {
        let entry = MacroEntry::literal("a", Provenance::Override);
        let rest = vec![
            ("p2".to_string(), make_profile(&[("foo", "a")])),
            ("p3".to_string(), make_profile(&[("other", "a")])),
        ];
        assert!(!intersect_predicate(
            "foo",
            &entry,
            &rest,
            CommonMode::Existence
        ));
        assert!(!intersect_predicate(
            "foo",
            &entry,
            &rest,
            CommonMode::Value
        ));
    }

    #[test]
    fn render_common_empty_intersection_prints_marker() {
        let resolved = vec![
            ("a".to_string(), make_profile(&[("only_a", "1")])),
            ("b".to_string(), make_profile(&[("only_b", "2")])),
        ];
        let opts = CommonOpts {
            mode: CommonMode::Existence,
            filter: None,
            profiles: Vec::new(),
            defines: crate::app::MacroDefinesArg::default(),
        };
        let mut buf = Vec::new();
        render_common(&mut buf, &opts, &resolved).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("(no common macros)"), "stdout={out}");
        assert!(
            out.contains("Common macros across 2 profile(s): 0"),
            "stdout={out}"
        );
    }

    #[test]
    fn render_common_panics_on_single_profile() {
        // render_common's docstring promises bail-out for <2 profiles
        // — pins that contract so future refactoring doesn't silently
        // degrade to an index-out-of-bounds panic.
        let resolved = vec![("a".to_string(), make_profile(&[("x", "1")]))];
        let opts = CommonOpts {
            mode: CommonMode::Existence,
            filter: None,
            profiles: Vec::new(),
            defines: crate::app::MacroDefinesArg::default(),
        };
        let mut buf = Vec::new();
        let err = render_common(&mut buf, &opts, &resolved).unwrap_err();
        assert!(err.to_string().contains("at least 2"), "err={err}");
    }

    #[test]
    fn default_intersection_set_skips_empty_profiles() {
        // `generic` is the canonical empty builtin — must be dropped.
        let config = Config::default();
        let base = std::env::temp_dir();
        let resolved = default_intersection_set(&config, &base, &[]).unwrap();
        assert!(
            resolved.iter().all(|(_, p)| !p.macros.is_empty()),
            "default set should not contain empty profiles"
        );
        assert!(
            !resolved
                .iter()
                .any(|(n, _)| n == super::super::DEFAULT_PROFILE),
            "generic must be excluded — got: {:?}",
            resolved.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
        // At least the bundled distribution profiles must be present.
        assert!(resolved.iter().any(|(n, _)| n == "rhel-9-x86_64"));
    }
}
