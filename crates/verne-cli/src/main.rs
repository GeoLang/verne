use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use verne_core::Report;
use verne_kml::KmlSource;

#[derive(Parser)]
#[command(
    name = "verne",
    version,
    about = "Read-only inventory of a data source"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report what a source holds and how much of it GeoLang could keep
    Inspect {
        /// Path to a .kml or .kmz file
        path: PathBuf,
        /// Also write the report as JSON to this path
        #[arg(long, value_name = "PATH")]
        json: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("verne: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let Command::Inspect { path, json } = cli.command;
    let source = KmlSource::open(&path)?;
    let report = Report::build(&source)?;
    println!("{}", report.to_markdown());
    if let Some(json_path) = json {
        std::fs::write(&json_path, report.to_json() + "\n")?;
        eprintln!("wrote {}", json_path.display());
    }
    Ok(())
}
