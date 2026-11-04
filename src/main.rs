// Nightly microbenchmarks are opt-in through `make bench`.
#![cfg_attr(rep_bench, feature(test))]
#[cfg(rep_bench)]
extern crate test;

#[cfg(rep_bench)]
mod benches;
mod cli;
mod config;
mod diff;
mod engine;
mod expressions;
mod hyperlink;
mod interactive;
mod options;
mod output;
mod runtime;
mod scan;
mod theme;
mod ui;
mod write;

use clap::CommandFactory as _;
use std::io::IsTerminal as _;

fn main() -> std::process::ExitCode {
    match run() {
        Ok(code) => std::process::ExitCode::from(code),
        Err(error) => {
            runtime::print_error(&error);
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> anyhow::Result<u8> {
    let environment = config::Environment::capture();
    let (mut cli, matches, origin) =
        match cli::parse_unresolved(std::env::args_os().collect(), &environment) {
            Ok(parsed) => parsed,
            Err(error) => {
                error.print()?;
                return Ok(u8::try_from(error.exit_code()).unwrap_or(1));
            }
        };
    let styles = ui::Styles::resolve(
        cli.color,
        std::io::stdout().is_terminal(),
        environment.no_color(),
    );
    if let Some(shell) = cli.completions {
        clap_complete::generate(
            shell,
            &mut cli::Cli::command(),
            "rep",
            &mut std::io::stdout(),
        );
        return Ok(0);
    }
    if cli.help_long {
        cli::print_help_long(&cli, styles);
        return Ok(0);
    }
    if cli.help {
        cli::print_help(&cli, styles);
        return Ok(0);
    }
    cli::resolve_mutex_groups(&mut cli, &matches, &origin)?;
    if !cli.uses_expressions() && cli.args.is_empty() && !cli.delete && !cli.list_files {
        cli::print_help(&cli, styles);
        return Ok(1);
    }
    if cli.positional_skip() > cli.args.len() {
        let missing = if cli.is_find_only() || cli.args.is_empty() {
            "<find>"
        } else {
            "<replace>"
        };
        runtime::print_error(&anyhow::anyhow!("missing required argument: {missing}"));
        cli::print_help(&cli, styles);
        return Ok(1);
    }
    runtime::run(&cli.into_options()?).map(|code| u8::try_from(code).unwrap_or(1))
}
