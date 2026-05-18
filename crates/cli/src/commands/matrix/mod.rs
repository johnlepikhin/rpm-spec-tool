//! `matrix` subcommand — multi-profile (release matrix) analysis.
//!
//! Phase 1 shipped `matrix check` + `matrix baseline create`. Phase 2
//! added `matrix portability` and `matrix coverage`. Phase 3 added
//! `matrix explain`. Phase 13 closes the diff-quartet with
//! `matrix impact` (per-profile delta between two git revisions of
//! one spec).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use rpm_spec_analyzer::config::Config;
use rpm_spec_analyzer::profile::{
    ProfileSection, ResolveOptions, ResolvedTargetSet, TargetEntry, resolve_target_set,
};
use rpm_spec_analyzer::{ParseOutcome, ParserSeverity};

pub mod baseline;
pub mod check;
pub mod classes;
pub mod coverage;
pub(crate) mod coverage_style;
pub mod diff;
pub mod expand;
pub mod explain;
pub mod impact;
pub mod portability;
pub mod verify_contract;

pub use check::{AD_HOC_TARGET_SET_ID, CheckOpts};

/// Bundle of artifacts every `matrix` subcommand needs before running
/// its main loop: a resolved config and the fully-resolved target set.
///
/// Built by [`prepare_matrix`] so each subcommand goes from "raw CLI
/// flags" to "ready to compute" in a single call. Replaces the
/// ~12-line load-validate-resolve dance that was previously copied
/// into every command.
///
/// Visibility: the type itself is `pub(crate)` so the
/// `commands::matrix::Cmd` dispatcher and its descendants can pass
/// it around. Fields are tighter — `pub(in crate::commands::matrix)`
/// — so the only sanctioned way to construct one is through
/// [`prepare_matrix`], which guarantees the validation prelude ran.
/// Sibling subcommands (check, baseline, coverage, …) live inside
/// `crate::commands::matrix::*` and read fields normally.
#[derive(Debug)]
pub(crate) struct MatrixContext {
    pub(in crate::commands::matrix) config: Config,
    pub(in crate::commands::matrix) resolved: ResolvedTargetSet,
}

/// Error returned by [`prepare_matrix`]. The two variants encode the
/// two failure modes distinctly so callers can route user errors to
/// `ExitCode::from(2)` while internal errors propagate via `anyhow`
/// for full backtraces.
///
/// **Convention**, not a compiler guarantee: every `prepare_matrix`
/// caller should call [`Self::into_exit`] on the error path. A
/// caller that manually destructures `UserInputReported` and returns
/// `Ok(ExitCode::SUCCESS)` would silently mask the diagnostic — the
/// variant name is the only safeguard.
#[derive(Debug)]
pub(crate) enum MatrixPrepareError {
    /// User-input error (invalid defines, unknown target set, etc).
    /// The friendly message has **already been printed to stderr**
    /// inside `prepare_matrix`. The variant name makes the contract
    /// self-documenting: a caller propagating this without invoking
    /// [`Self::into_exit`] (e.g. wrapping with `?` into a stricter
    /// `anyhow::Error` chain) silently loses the diagnostic.
    UserInputReported,
    /// Internal I/O or config-parse failure. Carries the anyhow chain
    /// so the caller's `?` propagation surfaces the full context.
    Internal(anyhow::Error),
}

impl From<anyhow::Error> for MatrixPrepareError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

impl MatrixPrepareError {
    /// Convert into the caller's `Result<ExitCode>` return shape.
    /// `UserInputReported` becomes `Ok(ExitCode::from(2))` (silent —
    /// the message is already on stderr); `Internal` propagates as
    /// an anyhow error.
    pub(crate) fn into_exit(self) -> Result<ExitCode> {
        match self {
            Self::UserInputReported => Ok(ExitCode::from(2)),
            Self::Internal(e) => Err(e),
        }
    }
}

/// Standard prelude every `matrix` subcommand runs:
///
/// 1. Validate `--define` syntax.
/// 2. Load `.rpmspec.toml` from `config_override` or the cwd walk-up.
/// 3. Resolve `--target-set NAME` / `--profiles a,b,c` into one
///    [`ResolvedTargetSet`] via [`resolve_matrix_source`].
///
/// Returns the populated context on success or a typed
/// [`MatrixPrepareError`] on failure — callers fold both error
/// variants into their `Result<ExitCode>` return via
/// [`MatrixPrepareError::into_exit`].
pub(crate) fn prepare_matrix(
    config_override: Option<&Path>,
    target_set: Option<&str>,
    profiles: &[String],
    defines: &crate::app::MacroDefinesArg,
) -> std::result::Result<MatrixContext, MatrixPrepareError> {
    if let Err(e) = defines.validate() {
        eprintln!("error: {e}");
        return Err(MatrixPrepareError::UserInputReported);
    }
    let (config, base_dir) = crate::commands::config_loader::load_config(config_override)?;
    let resolved =
        match resolve_matrix_source(&config, &base_dir, target_set, profiles, &defines.raw) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("error: {e:#}");
                return Err(MatrixPrepareError::UserInputReported);
            }
        };
    Ok(MatrixContext { config, resolved })
}

/// Resolve a matrix source — either a config-defined `[targets.<name>]`
/// set or an ad-hoc `--profiles a,b,c` list — into one
/// [`ResolvedTargetSet`]. Shared by every `matrix` subcommand so the
/// resolution policy and observability events stay consistent.
///
/// `target_set` and `profiles` correspond directly to the clap-parsed
/// CLI flags; the caller is responsible for the `ArgGroup` invariant
/// that exactly one of them is set.
pub(crate) fn resolve_matrix_source(
    config: &Config,
    base_dir: &Path,
    target_set: Option<&str>,
    profiles: &[String],
    defines: &[String],
) -> Result<ResolvedTargetSet> {
    let section = ProfileSection::new(config.profile.clone(), config.profiles.clone());
    let resolve_opts = ResolveOptions::default().with_defines(defines);

    if let Some(name) = target_set {
        tracing::info!(branch = "target_set", name = %name, "resolving matrix");
        let target = config
            .targets
            .get(name)
            .with_context(|| format!("target set `{name}` is not defined in .rpmspec.toml"))?;
        resolve_target_set(&section, name, target, base_dir, resolve_opts)
            .with_context(|| format!("failed to resolve target set `{name}`"))
    } else {
        tracing::info!(
            branch = "ad-hoc",
            profiles = ?profiles,
            "resolving matrix from CLI --profiles"
        );
        let target = TargetEntry::from_profiles(profiles.to_vec());
        resolve_target_set(
            &section,
            AD_HOC_TARGET_SET_ID,
            &target,
            base_dir,
            resolve_opts,
        )
        .with_context(|| "failed to resolve ad-hoc target set from --profiles")
    }
}

/// Per-call-site context for [`surface_parser_diagnostics`]. Each
/// variant carries the bit of variant text its `matrix` subcommand
/// needs (the subject of the warning prefix) and selects the trailing
/// clause through [`Self::trailing_clause`].
///
/// The per-command trailing wording is preserved exactly — operators
/// have come to associate phrases like "the diff below" or "contract
/// verdict below" with the originating subcommand, so we do **not**
/// unify them.
#[derive(Debug)]
pub(crate) enum ParseDiagnosticContext<'a> {
    /// `matrix impact` — one side of the rev-vs-rev compare. Renders
    /// the subject as `{label}-side spec ({rev})` (e.g.
    /// `from-side spec (HEAD~1)`).
    ImpactSide { label: &'a str, rev: &'a str },
    /// `matrix diff`.
    Diff { display_name: &'a str },
    /// `matrix expand`.
    Expand { display_name: &'a str },
    /// `matrix classes`.
    Classes { display_name: &'a str },
    /// `matrix verify-contract`.
    VerifyContract { display_name: &'a str },
    /// `matrix explain` — single-spec, no display name in the banner.
    Explain,
}

impl ParseDiagnosticContext<'_> {
    /// Subject of the warning sentence — the bit that goes between
    /// `warning: ` and ` produced N parser diagnostic(s)…`.
    fn subject(&self) -> String {
        match self {
            Self::ImpactSide { label, rev } => format!("{label}-side spec ({rev})"),
            Self::Diff { display_name }
            | Self::Expand { display_name }
            | Self::Classes { display_name }
            | Self::VerifyContract { display_name } => (*display_name).to_string(),
            Self::Explain => "spec".to_string(),
        }
    }

    /// Per-command trailing clause. Wording differs intentionally
    /// between subcommands; keep the strings byte-for-byte identical
    /// to the pre-extraction inline copies.
    fn trailing_clause(&self) -> &'static str {
        match self {
            Self::ImpactSide { .. } => {
                "the impact report is computed against the recovered AST and may be incomplete"
            }
            Self::Diff { .. } => {
                "the diff below is computed against the recovered AST and may be incomplete"
            }
            Self::Expand { .. } => {
                "the per-profile annotation below is computed against the recovered AST and may be incomplete"
            }
            Self::Classes { .. } => {
                "equivalence classes below are computed against the recovered AST and may be incomplete"
            }
            Self::VerifyContract { .. } => {
                "contract verdict below is computed against the recovered AST and may be unreliable"
            }
            Self::Explain => {
                "the report below is computed against the recovered AST and may be incomplete"
            }
        }
    }
}

/// Shared parser-diagnostic banner for every `matrix` subcommand that
/// computes a report against a (possibly recovered) AST. Emits one
/// `warning: …` line to stderr when `parsed` carries any parser
/// diagnostics, naming the subject (per `context`) and the
/// per-command trailing clause. No-op when the parse was clean.
///
/// The integration test suite asserts `stderr.contains("parser
/// diagnostic") || stderr.contains("recovered AST")` — keep both
/// phrases in the format string when changing wording.
pub(crate) fn surface_parser_diagnostics(
    context: ParseDiagnosticContext<'_>,
    parsed: &ParseOutcome,
) {
    if parsed.parser_diagnostics.is_empty() {
        return;
    }
    let total = parsed.parser_diagnostics.len();
    let errors = parsed
        .parser_diagnostics
        .iter()
        .filter(|d| matches!(d.severity, ParserSeverity::Error))
        .count();
    let subject = context.subject();
    let trailing = context.trailing_clause();
    eprintln!(
        "warning: {subject} produced {total} parser diagnostic(s) \
         ({errors} error-level) — {trailing}"
    );
}

#[derive(Debug, Args)]
pub struct Cmd {
    /// Explicit path to `.rpmspec.toml`. Without this flag the nearest
    /// `.rpmspec.toml` walking upward from each input file is used.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub action: Action,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Run all active lint rules against every member profile of a
    /// release target set and aggregate findings by affected profiles.
    Check(CheckOpts),
    /// Baseline management — record / inspect the set of currently
    /// known findings so CI can fail only on new ones.
    Baseline(baseline::Cmd),
    /// Report which macros referenced by the spec are defined on
    /// which member profiles. Surfaces platform-specific macros
    /// that often need `%{?guard}` wrappers.
    Portability(portability::PortabilityOpts),
    /// For each `%if` / `%ifarch` branch in the spec, list which
    /// member profiles activate it. Surfaces dead branches and
    /// distro-only branches.
    Coverage(coverage::CoverageOpts),
    /// Explain why a specific spec line is active on some profiles
    /// and inactive on others, or report the per-profile value of a
    /// named macro.
    Explain(explain::ExplainOpts),
    /// Verify the spec against a per-profile contract: declared
    /// must-have / must-not-have BuildRequires for every member
    /// profile of a target set. Fails on any violation.
    VerifyContract(verify_contract::VerifyContractOpts),
    /// Print the spec source per profile with each `%if`/`%ifarch`
    /// directive line tagged `[ACTIVE]`/`[INACTIVE]`/`[INDETERMINATE]`.
    Expand(expand::ExpandOpts),
    /// Structural diff between exactly two profiles: which deps are
    /// common, which are only on A, which are only on B. Branch-aware.
    Diff(diff::DiffOpts),
    /// Group target-set profiles by effective dependency footprint —
    /// surfaces a minimal "representative build set" when many
    /// profiles collapse to the same BuildRequires/Requires.
    Classes(classes::ClassesOpts),
    /// Per-profile dependency delta between two git revisions of a
    /// single spec. PR-review workflow: "this commit touches
    /// foo.spec — which platforms moved and how?".
    Impact(impact::ImpactOpts),
}

impl Cmd {
    pub fn run(self, color: crate::app::ColorChoice) -> Result<ExitCode> {
        match self.action {
            Action::Check(opts) => check::run(opts, self.config.as_deref(), color),
            Action::Baseline(cmd) => cmd.run(self.config.as_deref()),
            Action::Portability(opts) => portability::run(opts, self.config.as_deref(), color),
            Action::Coverage(opts) => coverage::run(opts, self.config.as_deref(), color),
            Action::Explain(opts) => explain::run(opts, self.config.as_deref()),
            Action::VerifyContract(opts) => verify_contract::run(opts, self.config.as_deref()),
            Action::Expand(opts) => expand::run(opts, self.config.as_deref(), color),
            Action::Diff(opts) => diff::run(opts, self.config.as_deref()),
            Action::Classes(opts) => classes::run(opts, self.config.as_deref()),
            Action::Impact(opts) => impact::run(opts, self.config.as_deref()),
        }
    }
}
