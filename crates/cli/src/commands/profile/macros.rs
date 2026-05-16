//! `profile macros` — list a profile's macro registry, with optional
//! filtering by name substring and/or provenance source.

use std::io::Write;

use anyhow::Result;
use clap::Args;
use rpm_spec_analyzer::profile::{MacroEntry, Profile, Provenance};

use super::fmt::{
    MAX_MACRO_LABEL_WIDTH, format_macro_value_inline, format_opts, format_provenance,
};

#[derive(Debug, Args)]
pub struct MacrosOpts {
    /// Profile name. Defaults to the active profile (CLI override →
    /// config `profile = …` → `generic`).
    pub profile: Option<String>,
    /// Case-insensitive substring filter on macro names.
    #[arg(long)]
    pub filter: Option<String>,
    /// Keep only macros that came from this provenance source.
    /// Accepts `builtin`, `showrc`, `override`.
    #[arg(long, value_name = "SRC")]
    pub source: Option<SourceFilter>,
}

/// Provenance-source filter for `profile macros --source`. Mirrors
/// the variants of [`Provenance`] without the per-variant payload —
/// only the source kind is selectable.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum SourceFilter {
    Builtin,
    Showrc,
    Override,
}

impl SourceFilter {
    fn matches(self, prov: &Provenance) -> bool {
        // Match on `prov` (not the tuple) so a future Provenance variant
        // forces an explicit decision here instead of silently filtering
        // to `false`. The inner `matches!` on `self` is safe — `SourceFilter`
        // lives in this crate and clap's `ValueEnum` derive breaks at the
        // same time any new variant is added.
        match prov {
            Provenance::Builtin { .. } => matches!(self, SourceFilter::Builtin),
            Provenance::Showrc { .. } => matches!(self, SourceFilter::Showrc),
            Provenance::Override => matches!(self, SourceFilter::Override),
        }
    }
}

pub(super) fn render_macros(
    out: &mut impl Write,
    profile_name: &str,
    profile: &Profile,
    opts: &MacrosOpts,
) -> Result<()> {
    let total = profile.macros.entries.len();
    let filter_lc = opts.filter.as_deref().map(str::to_ascii_lowercase);

    let matched: Vec<(&String, &MacroEntry)> = profile
        .macros
        .entries
        .iter()
        .filter(|(name, entry)| {
            let name_ok = match &filter_lc {
                Some(needle) => name.to_ascii_lowercase().contains(needle),
                None => true,
            };
            let source_ok = match opts.source {
                Some(src) => src.matches(&entry.provenance),
                None => true,
            };
            name_ok && source_ok
        })
        .collect();

    let header = match (opts.filter.as_deref(), opts.source) {
        (Some(filter), _) => format!("{total} total, {} matching \"{filter}\"", matched.len()),
        (None, Some(_)) => format!("{total} total, {} matching --source", matched.len()),
        (None, None) => format!("{total} total"),
    };
    writeln!(out, "# Macros in {profile_name} ({header})")?;

    if matched.is_empty() {
        writeln!(out)?;
        writeln!(out, "  (no macros)")?;
        return Ok(());
    }

    writeln!(out)?;
    // Align the `=` column on the longest macro-name (incl. opts) so
    // values line up. Capped to avoid pathological alignment for one
    // very long name dragging everything right.
    let name_width = matched
        .iter()
        .map(|(n, e)| n.len() + format_opts(e.opts.as_deref()).len())
        .max()
        .unwrap_or(0)
        .min(MAX_MACRO_LABEL_WIDTH);
    for (name, entry) in matched {
        let opts_str = format_opts(entry.opts.as_deref());
        let label = format!("{name}{opts_str}");
        writeln!(
            out,
            "  {label:<name_width$} = {}  [{}]",
            format_macro_value_inline(&entry.value),
            format_provenance(&entry.provenance),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_filter_matches_correct_variant() {
        let prov_b = Provenance::Builtin {
            profile: "x".into(),
        };
        let prov_s = Provenance::Showrc {
            level: -13,
            path: None,
        };
        let prov_o = Provenance::Override;
        assert!(SourceFilter::Builtin.matches(&prov_b));
        assert!(!SourceFilter::Builtin.matches(&prov_s));
        assert!(SourceFilter::Showrc.matches(&prov_s));
        assert!(!SourceFilter::Showrc.matches(&prov_o));
        assert!(SourceFilter::Override.matches(&prov_o));
    }
}
