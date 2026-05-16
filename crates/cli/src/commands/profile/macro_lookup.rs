//! `profile macro` — arity-polymorphic macro lookup.
//!
//! With zero profile arguments we render a table across every available
//! profile; with one, the value is printed compactly (exit 2 if
//! undefined); with two or more we compare only the named profiles.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::profile::{MacroValue, Profile};

use super::fmt::{MAX_PROFILE_NAME_WIDTH, compact_value, format_opts, format_provenance};
use super::{all_profile_names, resolve_many};

#[derive(Debug, Args)]
#[command(after_help = "\
Modes (chosen by number of PROFILES arguments):
  macro NAME                 — table of NAME across every available profile
                                (always exit 0)
  macro NAME P               — single profile, compact value with multiline body
                                expanded (exit 2 if NAME is undefined in P)
  macro NAME P1 P2 [P3 …]    — comparison table across listed profiles
                                (always exit 0)")]
pub struct MacroOpts {
    /// Macro name to look up (without the `%` prefix).
    pub name: String,
    /// Profiles to look the macro up in. See modes above for behaviour
    /// by argument count.
    pub profiles: Vec<String>,
}

/// Outcome of a single-profile macro lookup. Lifted out of `Result<bool>`
/// so the call site can branch on the semantic state explicitly and a
/// future `Found` variant carrying extra data wouldn't be a footgun.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacroLookup {
    Found,
    Undefined,
}

/// Arity-polymorphic dispatch for `profile macro`:
///   0 args  → every available profile (table, always exit 0)
///   1 arg   → single profile (compact; exit 2 if macro undefined)
///   2+ args → named profiles only (table, always exit 0)
pub(super) fn dispatch_macro(
    out: &mut impl Write,
    config: &Config,
    base_dir: &Path,
    opts: MacroOpts,
) -> Result<ExitCode> {
    let names: Vec<String> = if opts.profiles.is_empty() {
        all_profile_names(config)
    } else {
        opts.profiles
    };
    if names.len() == 1 {
        let single = &names[0];
        let profile = config
            .resolve_profile(base_dir, Some(single))
            .with_context(|| "failed to resolve profile")?;
        match render_macro(out, single, &profile, &opts.name)? {
            MacroLookup::Found => Ok(ExitCode::SUCCESS),
            MacroLookup::Undefined => Ok(ExitCode::from(2)),
        }
    } else {
        let resolved = resolve_many(config, base_dir, &names)?;
        render_macro_table(out, &opts.name, &resolved)?;
        Ok(ExitCode::SUCCESS)
    }
}

/// Render a single macro's value. Returns `MacroLookup::Undefined`
/// (with stderr msg) when the macro is not present in the resolved
/// registry, so the caller can map it to a distinct exit code.
fn render_macro(
    out: &mut impl Write,
    profile_name: &str,
    profile: &Profile,
    macro_name: &str,
) -> Result<MacroLookup> {
    let Some(entry) = profile.macros.get(macro_name) else {
        eprintln!("error: macro `{macro_name}` is not defined in profile `{profile_name}`");
        return Ok(MacroLookup::Undefined);
    };

    let opts_str = format_opts(entry.opts.as_deref());
    let prov = format_provenance(&entry.provenance);
    match &entry.value {
        MacroValue::Literal(s) => writeln!(out, "{macro_name}{opts_str} = {s}  [{prov}]")?,
        MacroValue::Builtin => writeln!(out, "{macro_name}{opts_str} = <builtin>  [{prov}]")?,
        MacroValue::Raw { body, multiline } => {
            if *multiline {
                // Render the head line empty (matches showrc layout) and
                // indent the body, so the full text is visible without
                // wrapping concerns.
                writeln!(out, "{macro_name}{opts_str} =  [{prov}]")?;
                for line in body.lines() {
                    writeln!(out, "    {line}")?;
                }
            } else {
                writeln!(out, "{macro_name}{opts_str} = {body}  [{prov}]")?;
            }
        }
    }
    Ok(MacroLookup::Found)
}

/// Render a macro's value across N profiles as a one-row-per-profile
/// table. Long values are truncated; multiline bodies collapse to a
/// `<multiline …>` marker — for the full body use `profile macro <name>
/// <profile>` with a single profile (compact mode renders multiline).
///
/// Caller is responsible for resolving the profiles (see `resolve_many`).
fn render_macro_table(
    out: &mut impl Write,
    macro_name: &str,
    resolved: &[(String, Profile)],
) -> Result<()> {
    writeln!(
        out,
        "# Macro `{}` across {} profile(s)",
        macro_name,
        resolved.len()
    )?;
    writeln!(out)?;

    let name_width = resolved
        .iter()
        .map(|(n, _)| n.len())
        .max()
        .unwrap_or(0)
        .min(MAX_PROFILE_NAME_WIDTH);

    for (profile_name, profile) in resolved {
        match profile.macros.get(macro_name) {
            Some(entry) => {
                let val = compact_value(&entry.value);
                let prov = format_provenance(&entry.provenance);
                writeln!(out, "  {profile_name:<name_width$} = {val}  [{prov}]")?;
            }
            None => {
                writeln!(out, "  {profile_name:<name_width$} = (undefined)")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpm_spec_analyzer::profile::{MacroEntry, Provenance};

    #[test]
    fn render_macro_returns_undefined_for_unknown() {
        let profile = Profile::default();
        let mut buf = Vec::new();
        let result = render_macro(&mut buf, "generic", &profile, "no-such-macro").unwrap();
        assert_eq!(result, MacroLookup::Undefined);
        // stdout buffer untouched on the "not found" path — error goes to stderr.
        assert!(buf.is_empty());
    }

    #[test]
    fn render_macro_returns_found_and_writes_for_known() {
        let mut profile = Profile::default();
        profile.macros.insert(
            "dist",
            MacroEntry::literal(
                ".el9",
                Provenance::Showrc {
                    level: -13,
                    path: None,
                },
            ),
        );
        let mut buf = Vec::new();
        let result = render_macro(&mut buf, "rhel-9-x86_64", &profile, "dist").unwrap();
        assert_eq!(result, MacroLookup::Found);
        let out = String::from_utf8(buf).unwrap();
        assert!(out.starts_with("dist = .el9"));
        assert!(out.contains("[showrc:-13]"));
    }
}
