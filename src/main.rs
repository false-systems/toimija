use std::io::Write;
use std::process::ExitCode;

use toimija::{cli, logging};

fn main() -> ExitCode {
    let cli = cli::Cli::parse_args();
    logging::init(cli.verbose, cli.quiet, cli.log_format).unwrap_or_else(|error| {
        eprintln!("failed to initialize logging: {error}");
    });

    match cli::run(cli) {
        Ok(0) => ExitCode::SUCCESS,
        Ok(code) => {
            // `run`/`verify` picked a specific code (wrapped-command status,
            // stale-packet gate). `process::exit` preserves the full i32 range
            // but skips destructors, so flush buffered output first.
            std::io::stdout().flush().ok();
            std::io::stderr().flush().ok();
            std::process::exit(code);
        }
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
