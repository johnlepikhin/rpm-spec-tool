//! `matrix baseline` subcommand — snapshot of matrix findings for
//! CI gating on new regressions.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use rpm_spec_analyzer::{Baseline, run_matrix};

use crate::commands::matrix::check::{self, CheckOpts};
use crate::io;

#[derive(Debug, Args)]
pub struct Cmd {
    #[command(subcommand)]
    pub action: Action,
}

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Run the matrix and emit a baseline JSON document covering the
    /// current set of findings. Pipe into a file (`> baseline.json`)
    /// or pass `--out PATH` to write directly.
    Create(CreateOpts),
}

#[derive(Debug, Args)]
pub struct CreateOpts {
    #[command(flatten)]
    pub check: CheckOpts,
    /// Destination file for the baseline document. When omitted the
    /// document is written to stdout.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

impl Cmd {
    pub fn run(self, config_override: Option<&Path>) -> Result<ExitCode> {
        match self.action {
            Action::Create(opts) => create(opts, config_override),
        }
    }
}

fn create(opts: CreateOpts, config_override: Option<&Path>) -> Result<ExitCode> {
    let ctx = match crate::commands::matrix::prepare_matrix(
        config_override,
        opts.check.target_set.as_deref(),
        &opts.check.profiles,
        &opts.check.defines,
    ) {
        Ok(c) => c,
        Err(e) => return e.into_exit(),
    };
    // Severity overrides mirror the `matrix check` policy so a baseline
    // captures the same finding set the gating run would see.
    let config = check::config_with_severity_overrides(&ctx.config, &opts.check);
    let resolved = ctx.resolved;

    let sources = io::read_sources(&opts.check.input.paths)?;
    // Aggregate findings across every input source — the baseline
    // tracks the union, since CI typically runs the whole spec set
    // against one target. Per-spec partitioning would force a
    // baseline-per-file model that doesn't match how teams gate.
    let mut combined_aggregated: Vec<rpm_spec_analyzer::AggregatedDiagnostic> = Vec::new();
    for source in &sources {
        let source_path = if source.is_stdin {
            None
        } else {
            Some(source.path.as_path())
        };
        let result = run_matrix(&source.contents, source_path, &config, &resolved);
        combined_aggregated.extend(result.aggregated);
    }
    let baseline = Baseline::from_aggregated(&combined_aggregated);

    if let Some(path) = &opts.out {
        // Write to a temp file in the target directory and rename
        // into place — atomic on POSIX, prevents leaving a partial
        // baseline behind if the process dies mid-write (SIGINT,
        // disk full).
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let tmp = tempfile::NamedTempFile::new_in(dir)
            .with_context(|| format!("creating temp file in {}", dir.display()))?;
        baseline
            .write(tmp.as_file())
            .with_context(|| format!("writing baseline to {}", path.display()))?;
        tmp.persist(path)
            .with_context(|| format!("renaming temp file to {}", path.display()))?;
        tracing::info!(
            entries = baseline.len(),
            path = %path.display(),
            "baseline written"
        );
        eprintln!(
            "wrote baseline with {} entries to {}",
            baseline.len(),
            path.display()
        );
    } else {
        let stdout = std::io::stdout();
        baseline
            .write(stdout.lock())
            .context("writing baseline to stdout")?;
    }
    Ok(ExitCode::SUCCESS)
}
