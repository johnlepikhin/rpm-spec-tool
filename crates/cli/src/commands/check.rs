//! `check` subcommand — lint + format --check rolled into one CI invocation.

use std::process::ExitCode;

use anyhow::Result;
use clap::Args;
use rpm_spec::printer::print_with;
use rpm_spec_analyzer::{Severity, analyze};

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
        let mut config_cache = cli_config::ConfigCache::new(self.input.config.clone());

        let mut any_failure = false;
        let mut any_io_error = false;
        let mut all_diagnostics = Vec::new();

        for source in sources {
            let Some(analyzer_cfg) =
                config_cache.load_or_report(&source.path, &mut any_io_error)
            else {
                continue;
            };

            let (outcome, diags) = analyze(&source.contents, &analyzer_cfg);

            if diags.iter().any(|d| d.severity == Severity::Deny) {
                any_failure = true;
            }

            let pcfg = analyzer_cfg.format.to_printer_config();
            let formatted = print_with(&outcome.spec, &pcfg);
            if formatted != source.contents {
                eprintln!("would reformat: {}", source.display_name());
                any_failure = true;
            }

            all_diagnostics.push((source, diags));
        }

        output::human::render(&all_diagnostics, color)?;
        Ok(if any_io_error {
            ExitCode::from(2)
        } else if any_failure {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        })
    }
}
