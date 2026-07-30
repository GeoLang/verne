use std::path::{Path, PathBuf};
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
        /// Path to a .kml or .kmz file, or to a .gdb directory
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
    let report = if is_geodatabase(&path) {
        geodatabase(&path)?
    } else {
        Report::build(&KmlSource::open(&path)?)?
    };
    println!("{}", report.to_markdown());
    if let Some(json_path) = json {
        std::fs::write(&json_path, report.to_json() + "\n")?;
        eprintln!("wrote {}", json_path.display());
    }
    Ok(())
}

/// A file geodatabase is a directory, and the extension is what names it.
fn is_geodatabase(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gdb"))
}

#[cfg(feature = "gdb")]
fn geodatabase(path: &Path) -> Result<Report, Box<dyn std::error::Error>> {
    Ok(Report::build(&verne_gdb::GdbSource::open(path)?)?)
}

#[cfg(not(feature = "gdb"))]
fn geodatabase(path: &Path) -> Result<Report, Box<dyn std::error::Error>> {
    Err(format!(
        "{} is a file geodatabase, and this verne was built without the gdb feature",
        path.display()
    )
    .into())
}
