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
use rpm_spec_analyzer::profile::{MacroEntry, MacroValue, Profile};

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

    #[command(flatten)]
    pub defines: crate::app::MacroDefinesArg,
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
            .resolve_profile(
                base_dir,
                rpm_spec_analyzer::profile::ResolveOptions::with_override(Some(single))
                    .with_defines(&opts.defines.raw),
            )
            .with_context(|| "failed to resolve profile")?;

        // When `--define` is in play, also resolve the *baseline*
        // profile (same config, no CLI defines) so the renderer can
        // surface what the user's define overwrote. Skipped when no
        // defines were passed — no shadow possible.
        //
        // Cost: bundled showrc parsing is memoised via `OnceLock` in
        // `crates/profile/src/builtin.rs` (`CACHE`), so re-resolving
        // for built-ins doesn't re-parse the dump. **User-supplied**
        // `showrc-file = "..."` paths are NOT cached and will be
        // re-read on the second resolve — acceptable for an
        // interactive `profile macro` invocation, but worth knowing
        // for batch users.
        //
        // Failure handling: if the baseline resolve fails (e.g. the
        // user's showrc became unreadable between the two calls — very
        // unlikely in practice but not impossible), we degrade
        // silently by treating "no shadow" as the rendering choice.
        // Surfacing "failed to resolve baseline profile" *after* the
        // winning resolve already succeeded would confuse users with
        // an error that doesn't affect their actual lookup.
        let shadowed = if opts.defines.raw.is_empty() {
            None
        } else {
            match config.resolve_profile(
                base_dir,
                rpm_spec_analyzer::profile::ResolveOptions::with_override(Some(single)),
            ) {
                Ok(baseline) => {
                    // Only flag a shadow when the baseline actually had a
                    // value for this macro that differs from the winning
                    // one. Equivalence uses `MacroEntry::is_equivalent`
                    // (opts + body, ignoring provenance) — a redundant
                    // `-D NAME same-value` shouldn't render a confusing
                    // identity-line.
                    let winning = profile.macros.get(&opts.name);
                    baseline
                        .macros
                        .get(&opts.name)
                        .cloned()
                        .filter(|prev| winning.map(|w| !w.is_equivalent(prev)).unwrap_or(false))
                }
                Err(e) => {
                    tracing::debug!(
                        profile = %single,
                        macro_name = %opts.name,
                        error = %e,
                        "baseline resolve failed; skipping shadow line"
                    );
                    None
                }
            }
        };

        match render_macro(out, single, &profile, &opts.name, shadowed.as_ref())? {
            MacroLookup::Found => Ok(ExitCode::SUCCESS),
            MacroLookup::Undefined => Ok(ExitCode::from(2)),
        }
    } else {
        let resolved = resolve_many(config, base_dir, &names, &opts.defines.raw)?;
        render_macro_table(out, &opts.name, &resolved)?;
        Ok(ExitCode::SUCCESS)
    }
}

/// Render a single macro's value. Returns `MacroLookup::Undefined`
/// (with stderr msg) when the macro is not present in the resolved
/// registry, so the caller can map it to a distinct exit code.
///
/// `shadowed` carries the entry the winning value supersedes, if any.
/// Only set when `--define` was passed AND the baseline (define-free)
/// resolution had a different entry for `macro_name`; the second line
/// (`  shadows: …`) gives users a "what did I just overwrite?" view
/// without re-running with and without `-D`.
fn render_macro(
    out: &mut impl Write,
    profile_name: &str,
    profile: &Profile,
    macro_name: &str,
    shadowed: Option<&MacroEntry>,
) -> Result<MacroLookup> {
    let Some(entry) = profile.macros.get(macro_name) else {
        eprintln!("error: macro `{macro_name}` is not defined in profile `{profile_name}`");
        return Ok(MacroLookup::Undefined);
    };

    write_macro_line(out, macro_name, entry, "")?;
    // When the winning entry shadows a baseline value, render it
    // beneath the main line with a "shadows:" prefix and one level of
    // indent. Same value-formatting machinery as the main line so a
    // multi-line `Raw` body stays readable.
    if let Some(prev) = shadowed {
        write_macro_line(out, macro_name, prev, "  shadows: ")?;
    }
    Ok(MacroLookup::Found)
}

/// Format one `<prefix>NAME[(opts)] = VALUE  [PROV]` line. Multi-line
/// `Raw` bodies are rendered with the head line ending in `=` and the
/// body indented underneath. Extracted so `render_macro` can reuse the
/// same formatting for the winning entry and its shadowed predecessor.
fn write_macro_line(
    out: &mut impl Write,
    macro_name: &str,
    entry: &MacroEntry,
    prefix: &str,
) -> Result<()> {
    let opts_str = format_opts(entry.opts.as_deref());
    let prov = format_provenance(&entry.provenance);
    match &entry.value {
        MacroValue::Literal(s) => writeln!(out, "{prefix}{macro_name}{opts_str} = {s}  [{prov}]")?,
        MacroValue::Builtin => {
            writeln!(out, "{prefix}{macro_name}{opts_str} = <builtin>  [{prov}]")?
        }
        MacroValue::Raw { body, multiline } => {
            if *multiline {
                writeln!(out, "{prefix}{macro_name}{opts_str} =  [{prov}]")?;
                for line in body.lines() {
                    writeln!(out, "    {line}")?;
                }
            } else {
                writeln!(out, "{prefix}{macro_name}{opts_str} = {body}  [{prov}]")?;
            }
        }
    }
    Ok(())
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
        let result = render_macro(&mut buf, "generic", &profile, "no-such-macro", None).unwrap();
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
        let result = render_macro(&mut buf, "rhel-9-x86_64", &profile, "dist", None).unwrap();
        assert_eq!(result, MacroLookup::Found);
        let out = String::from_utf8(buf).unwrap();
        assert!(out.starts_with("dist = .el9"));
        assert!(out.contains("[showrc:-13]"));
    }

    /// `shadowed = Some(prev)` adds a `  shadows: NAME = OLD  [PROV]`
    /// line beneath the main one. Both lines share the same formatter
    /// so layout stays consistent.
    #[test]
    fn render_macro_with_shadowed_entry_prints_second_line() {
        let mut profile = Profile::default();
        profile
            .macros
            .insert("dist", MacroEntry::literal(".fc40", Provenance::Override));
        let shadowed = MacroEntry::literal(
            ".el9",
            Provenance::Showrc {
                level: -13,
                path: None,
            },
        );
        let mut buf = Vec::new();
        let result =
            render_macro(&mut buf, "rhel-9-x86_64", &profile, "dist", Some(&shadowed)).unwrap();
        assert_eq!(result, MacroLookup::Found);
        let out = String::from_utf8(buf).unwrap();
        // Winning line first.
        assert!(
            out.contains("dist = .fc40  [override]"),
            "missing winning line: {out}"
        );
        // Shadow line second, with explicit `shadows:` prefix and the
        // original provenance label.
        assert!(
            out.contains("  shadows: dist = .el9  [showrc:-13]"),
            "missing shadow line: {out}"
        );
    }
}
