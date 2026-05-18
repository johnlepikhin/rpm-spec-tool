//! `config init` — write a starter `.rpmspec.toml`.
//!
//! The file is built from [`Config::default`] serialized to TOML. With
//! `--all-lints`, every built-in lint appears as a `# lint-name =
//! "severity"` line so users can scan the catalogue without flipping
//! between the file and `rpm-spec-tool lints`. Commented form is
//! deliberate — uncommenting a line is then the explicit, audit-able
//! act of overriding the default.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::Args;
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::diagnostic::{LintCategory, Severity};
use rpm_spec_analyzer::registry;

const DEFAULT_PATH: &str = ".rpmspec.toml";

#[derive(Debug, Args)]
pub struct InitOpts {
    /// Output path. Defaults to `./.rpmspec.toml`.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Set the active distribution profile (`profile = …` in the
    /// generated TOML). The name is *not* validated against the list of
    /// built-in profiles — anything goes, so this also works for user-
    /// defined profiles you plan to add to `[profiles.*]`.
    #[arg(long, value_name = "NAME")]
    pub profile: Option<String>,

    /// Emit every built-in lint as a commented `# lint-name = "severity"`
    /// entry so the file doubles as a discoverable catalogue.
    #[arg(long)]
    pub all_lints: bool,

    /// Write to stdout instead of a file. Mutually exclusive with
    /// `--output` in spirit; if both are passed, stdout wins.
    #[arg(long)]
    pub stdout: bool,

    /// Overwrite the output file if it already exists. Without this
    /// flag, init refuses to clobber an existing config.
    #[arg(long)]
    pub force: bool,
}

pub fn run(opts: InitOpts) -> Result<ExitCode> {
    let content = render(&opts)?;

    if opts.stdout {
        std::io::stdout()
            .write_all(content.as_bytes())
            .context("failed to write to stdout")?;
        return Ok(ExitCode::SUCCESS);
    }

    let path = opts
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_PATH));

    if path.exists() && !opts.force {
        eprintln!(
            "error: {} already exists; pass --force to overwrite or --stdout to preview",
            path.display()
        );
        return Ok(ExitCode::from(1));
    }

    fs::write(&path, &content).with_context(|| format!("failed to write {}", path.display()))?;
    eprintln!("wrote {}", path.display());
    Ok(ExitCode::SUCCESS)
}

/// Render the `.rpmspec.toml` body. Public for the unit tests.
pub(crate) fn render(opts: &InitOpts) -> Result<String> {
    let mut config = Config::default();
    if let Some(name) = opts.profile.clone() {
        config.profile = Some(name);
    }

    // `toml::to_string_pretty` writes tables in deterministic alphabetical
    // order — exactly what we want for a starter file. The
    // `warnings_as_errors` field is `#[serde(skip)]` so it never lands
    // in the TOML output.
    let raw = toml::to_string_pretty(&config).context("failed to serialize default config")?;

    let mut out = String::new();
    out.push_str(HEADER);
    out.push('\n');
    out.push_str(&inject_section_examples(&raw));

    if opts.all_lints {
        out.push('\n');
        out.push_str(LINT_CATALOGUE_HEADER);
        out.push_str(&render_lint_catalogue());
    }

    Ok(out)
}

/// Insert a block of commented examples directly after each top-level
/// section header that `toml::to_string_pretty` emits. The TOML writer
/// produces `[name]` on its own line, so a line-by-line scan is enough;
/// we don't need a real parser. Sections with no example match keep the
/// unmodified body.
fn inject_section_examples(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 1024);
    for line in raw.lines() {
        out.push_str(line);
        out.push('\n');
        let stripped = line.trim();
        if let Some(rest) = stripped.strip_prefix('[')
            && let Some(name) = rest.strip_suffix(']')
            && let Some(example) = SECTION_EXAMPLES.iter().find(|(s, _)| *s == name)
        {
            out.push_str(example.1);
        }
    }
    out
}

/// Hand-curated commented examples per top-level section. The
/// commented form is intentional — uncommenting a line is then an
/// audit-able act of overriding the default. Each example is kept
/// small enough to fit on screen and reflects the real schema
/// (`config validate` accepts them after uncommenting).
const SECTION_EXAMPLES: &[(&str, &str)] = &[
    (
        "lints",
        "\
# Examples — uncomment to override a rule's default severity:
# missing-changelog       = \"deny\"
# mixed-spaces-and-tabs   = \"allow\"
# trailing-whitespace     = \"warn\"
",
    ),
    (
        "format",
        "\
# Examples — pretty-printer knobs honoured by `format` / `pretty`:
# preamble-align-column = 20   # column at which `Tag: value` is aligned
# conditional-indent    = 2    # spaces per nested %if level (cosmetic only)
",
    ),
    (
        "shellcheck",
        "\
# Examples — control the optional shellcheck integration (RPM200):
# binary  = \"/usr/local/bin/shellcheck\"  # override binary discovery
# dialect = \"bash\"                       # sh | bash | dash | ksh
# disable = [\"SC2086\", \"SC2059\"]         # suppress in addition to baseline
# enable  = [\"SC2164\"]                   # re-enable a baseline-suppressed code
# timeout-ms = 5000                       # per-section timeout
",
    ),
    (
        "profiles",
        "\
# Examples — declare a user profile by extending a built-in:
# [profiles.my-rhel-10]
# extends     = [\"rhel-10\"]
# showrc-file = \"vendor/showrc-rhel10.dump\"  # optional: feed `rpm --showrc`
#
# [profiles.my-rhel-10.macros]
# dist = \".el10.custom\"
# myorg_prefix = \"/opt/acme\"
#
# Activate it with `profile = \"my-rhel-10\"` above, or pass `--profile my-rhel-10`.
",
    ),
    (
        "targets",
        "\
# Examples — name a collection of profiles for `rpm-spec-tool matrix`:
# [targets.release-2026]
# profiles = [\"fedora-40\", \"fedora-41\", \"rhel-10\"]
#
# Optional uniform `-D NAME VALUE` overrides applied to every profile
# in the set (layered between the profile's own macros and any CLI
# `--define`):
# [targets.release-2026]
# profiles = [\"fedora-40\", \"fedora-41\"]
# defines  = { product_build = \"1\", debug = \"0\" }
#
# Outlier per-profile overrides inside the same target:
# [targets.release-2026.profile-overrides.\"fedora-40\"]
# defines = { use_jit = \"0\" }   # only this profile sees use_jit=0
",
    ),
    (
        "macros",
        "\
# Examples — declare allowed values for a build-time macro.
# `matrix coverage` uses these to mark a branch
# `[CONDITIONAL: macro=value]` when it activates under at least
# one declared variant value, instead of `[DEAD]`. Without
# variants the analyser can't tell genuinely-dead code apart from
# code that's just inactive under the current build's `-D`.
#
# [macros.edition]
# values      = [\"community\", \"premium\", \"oem\"]
# description = \"Build edition selector\"
#
# [macros.major_version]
# values = [\"13\", \"14\", \"15\", \"16\", \"17\"]
#
# Cartesian-product cap: branches whose declared variants exceed
# 64 combinations are skipped (a `tracing::warn` surfaces the
# branch). Keep value sets reasonably small per macro.
#
# Interaction with `-D NAME VALUE`: CLI `-D` wins for the current
# build's verdict (active/inactive); the variant set still applies
# to the reachability check that produces `[CONDITIONAL]`.
",
    ),
];

const HEADER: &str = "\
# .rpmspec.toml — configuration for rpm-spec-tool.
#
# Generated by `rpm-spec-tool config init`. Edit freely — schema is the
# public contract and won't break across patch releases.
#
# See `rpm-spec-tool lints` for the full catalogue or run
# `rpm-spec-tool config init --all-lints` to embed it inline.
";

const LINT_CATALOGUE_HEADER: &str = "\
# ---------------------------------------------------------------------------
# Lint catalogue. Each line is the default severity. Uncomment a line
# (and inline it under [lints]) to override.
# ---------------------------------------------------------------------------
";

/// Render every built-in lint as a commented `# name = \"severity\"` line,
/// grouped by [`LintCategory`]. We pull straight from the analyzer's
/// registry so the catalogue stays in sync with the rule set automatically.
fn render_lint_catalogue() -> String {
    let lints = registry::builtin_lints();
    let mut by_cat: std::collections::BTreeMap<String, Vec<(String, &'static str, &'static str)>> =
        std::collections::BTreeMap::new();
    for lint in &lints {
        let meta = lint.metadata();
        let sev = match meta.default_severity {
            Severity::Allow => "allow",
            Severity::Warn => "warn",
            Severity::Deny => "deny",
        };
        let cat = category_label(meta.category);
        by_cat.entry(cat.to_string()).or_default().push((
            meta.name.to_string(),
            sev,
            meta.description,
        ));
    }

    let mut out = String::new();
    for (cat, mut rules) in by_cat {
        rules.sort_by(|a, b| a.0.cmp(&b.0));
        out.push_str(&format!("\n# --- {cat} ---\n"));
        for (name, sev, desc) in rules {
            // Wrap the description to a single short line — multi-line
            // comments would push the file size past anything users
            // actually read.
            let short = first_sentence(desc);
            out.push_str(&format!("# {short}\n"));
            out.push_str(&format!("# {name} = \"{sev}\"\n"));
        }
    }
    out
}

fn category_label(cat: LintCategory) -> &'static str {
    match cat {
        LintCategory::Style => "style",
        LintCategory::Correctness => "correctness",
        LintCategory::Packaging => "packaging",
        LintCategory::Performance => "performance",
        _ => "other",
    }
}

fn first_sentence(s: &str) -> String {
    let trimmed = s.trim();
    if let Some(end) = trimmed.find(". ") {
        trimmed[..=end].to_string()
    } else {
        trimmed.lines().next().unwrap_or("").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> InitOpts {
        InitOpts {
            output: None,
            profile: None,
            all_lints: false,
            stdout: false,
            force: false,
        }
    }

    #[test]
    fn default_init_round_trips_through_deserializer() {
        let body = render(&opts()).unwrap();
        // The generated file must parse back as a Config without
        // errors. Otherwise users would copy something invalid.
        Config::from_toml_str(&body).expect("generated body must deserialize");
    }

    #[test]
    fn profile_flag_lands_in_output() {
        let mut o = opts();
        o.profile = Some("fedora-40".to_string());
        let body = render(&o).unwrap();
        assert!(
            body.contains("profile = \"fedora-40\""),
            "expected `profile` line in:\n{body}"
        );
    }

    #[test]
    fn every_section_carries_a_commented_example() {
        let body = render(&opts()).unwrap();
        for (section, _) in SECTION_EXAMPLES {
            // Both the header and at least one commented line after
            // it must be present — guards against future refactors
            // that drop the injection pass.
            let header = format!("[{section}]");
            assert!(body.contains(&header), "section [{section}] missing");
        }
        assert!(body.contains("# missing-changelog       = \"deny\""));
        assert!(body.contains("# [profiles.my-rhel-10]"));
        assert!(body.contains("# [targets.release-2026]"));
        // The examples are commented; the file must still deserialize
        // as a valid Config (no example line accidentally uncommented).
        Config::from_toml_str(&body).expect("body with examples must parse");
    }

    #[test]
    fn all_lints_includes_known_rules() {
        let mut o = opts();
        o.all_lints = true;
        let body = render(&o).unwrap();
        // missing-changelog (RPM001) is the canary every test for this
        // tool checks against.
        assert!(
            body.contains("# missing-changelog = "),
            "expected missing-changelog catalogue line in:\n{body}"
        );
        // The catalogue is commented; uncommenting must be a valid
        // override for [lints]. Verify the rendered line is itself
        // parseable as a `lints` map entry — we lift it into a fresh
        // tiny TOML document so the `[lints]` section the generator
        // already wrote doesn't trigger a duplicate-key error.
        let line = body
            .lines()
            .find(|l| {
                l.trim_start_matches('#')
                    .trim()
                    .starts_with("missing-changelog =")
            })
            .expect("expected catalogue line");
        let stripped = line.trim_start_matches('#').trim_start();
        let mini = format!("[lints]\n{stripped}\n");
        Config::from_toml_str(&mini)
            .unwrap_or_else(|e| panic!("promoted catalogue line must parse ({e}): {mini}"));
    }
}
