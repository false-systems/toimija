use std::io::Write;
use std::process::ExitCode;

use toimija::{cli, logging};

fn main() -> ExitCode {
    let cli = cli::Cli::parse_args();
    logging::init(cli.verbose, cli.quiet, cli.log_format, cli.no_color).unwrap_or_else(|error| {
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
            // A refusal is a formatted conversation with the human: print it
            // as-is (no context-chain flattening) and exit 2, the usage-error
            // convention, so scripts can tell "you asked wrong" from "it broke".
            if error.downcast_ref::<cli::Refusal>().is_some() {
                eprintln!("{error}");
                std::io::stdout().flush().ok();
                std::io::stderr().flush().ok();
                std::process::exit(2);
            }
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}
