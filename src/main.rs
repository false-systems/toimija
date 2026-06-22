use std::process::ExitCode;

use toimija::{cli, logging};

fn main() -> ExitCode {
    let cli = cli::Cli::parse_args();
    logging::init(cli.verbose, cli.quiet, cli.log_format).unwrap_or_else(|error| {
        eprintln!("failed to initialize logging: {error}");
    });

    match cli::run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
