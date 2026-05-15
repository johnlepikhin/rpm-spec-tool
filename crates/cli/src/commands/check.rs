//! `check` subcommand — lint + format --check rolled into one CI invocation.

use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use rpm_spec::printer::{PrinterConfig, print_with};
use rpm_spec_analyzer::{LintSession, Severity, parse};

use crate::app::ColorChoice;
use crate::config as cli_config;
use crate::io;
use crate::output;

#[derive(Debug, Args)]
pub struct Cmd {
    #[command(flatten)]
    pub input: crate::app::CommonInput,
}

impl Cmd {
    pub fn run(self, color: ColorChoice) -> Result<ExitCode> {
        let sources = io::read_sources(&self.input.paths)?;
        let mut any_failure = false;
        let mut all_diagnostics = Vec::new();

        for source in sources {
            let cfg_path = self.input.config.as_deref();
            let analyzer_cfg = cli_config::load(cfg_path, Some(&source.path))?;

            let outcome = parse(&source.contents);
            let mut session = LintSession::from_config(&analyzer_cfg);
            let diags = session.run(&outcome.spec);

            if diags.iter().any(|d| d.severity == Severity::Deny) {
                any_failure = true;
            }

            // Format check.
            let pcfg = PrinterConfig::new()
                .with_indent(analyzer_cfg.format.conditional_indent as usize)
                .with_preamble_value_column(
                    if analyzer_cfg.format.preamble_align_column == 0 {
                        None
                    } else {
                        Some(analyzer_cfg.format.preamble_align_column as usize)
                    },
                );
            let formatted = print_with(&outcome.spec, &pcfg);
            if formatted != source.contents {
                eprintln!("would reformat: {}", source.display_name());
                any_failure = true;
            }

            all_diagnostics.push((source, diags));
        }

        output::human::render(&all_diagnostics, color)?;
        Ok(if any_failure { ExitCode::from(1) } else { ExitCode::SUCCESS })
    }
}
