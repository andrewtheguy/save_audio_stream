mod audio;
mod config;
mod record;
mod schedule;
mod serve;
mod streaming;
mod webm;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Download and convert audio streams to compressed formats"
)]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Record audio from a stream
    Record {
        /// Path to config file (TOML format)
        #[arg(short, long)]
        config: PathBuf,
    },
    /// Serve audio from SQLite database via HTTP
    Serve {
        /// Path to SQLite database file
        sqlite_file: PathBuf,

        /// Port to listen on
        #[arg(short, long, default_value = "3000")]
        port: u16,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let args = Args::parse();

    match args.command {
        Command::Record { config } => record::record(config),
        Command::Serve { sqlite_file, port } => serve::serve(sqlite_file, port),
    }
}
