use clap::ValueEnum;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LogFormat {
    Text,
    Json,
}

pub fn init(verbose: u8, quiet: bool, format: LogFormat) -> Result<(), String> {
    let level = if quiet {
        LevelFilter::OFF
    } else {
        match verbose {
            0 => LevelFilter::WARN,
            1 => LevelFilter::INFO,
            _ => LevelFilter::DEBUG,
        }
    };

    let filter = EnvFilter::builder()
        .with_default_directive(level.into())
        .from_env_lossy();

    match format {
        LogFormat::Text => {
            let layer = fmt::layer().with_writer(std::io::stderr);
            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init()
                .map_err(|error| error.to_string())
        }
        LogFormat::Json => {
            let layer = fmt::layer().json().with_writer(std::io::stderr);
            tracing_subscriber::registry()
                .with(filter)
                .with(layer)
                .try_init()
                .map_err(|error| error.to_string())
        }
    }
}
