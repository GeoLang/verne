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
    /// Write a source out as a GeoPackage and a sidecar ptolemy can load
    Extract {
        /// Path to a .gdb directory
        path: PathBuf,
        /// Directory to write the GeoPackage, the sidecar and the log into
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Who is running this, recorded in the extraction log
        #[arg(long, value_name = "NAME")]
        operator: String,
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
    match cli.command {
        Command::Inspect { path, json } => {
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
        Command::Extract {
            path,
            out,
            operator,
        } => extract(&path, &out, &operator),
    }
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

#[cfg(feature = "gdb")]
fn extract(path: &Path, out: &Path, operator: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !is_geodatabase(path) {
        return Err(format!(
            "{} is not a file geodatabase, and only geodatabases can be extracted so far",
            path.display()
        )
        .into());
    }
    let source = verne_gdb::GdbSource::open(path)?;
    let extraction = source.extract(out, operator)?;
    println!("{}", extraction.sidecar.log.to_markdown());
    eprintln!("wrote {}", extraction.sidecar_path.display());
    if let Some(geopackage) = &extraction.geopackage_path {
        eprintln!("wrote {}", geopackage.display());
    }
    Ok(())
}

#[cfg(not(feature = "gdb"))]
fn extract(path: &Path, _out: &Path, _operator: &str) -> Result<(), Box<dyn std::error::Error>> {
    Err(format!(
        "{} can only be extracted by a verne built with the gdb feature",
        path.display()
    )
    .into())
}
