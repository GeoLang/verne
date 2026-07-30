use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use verne_core::{Report, Sidecar};
use verne_kml::KmlSource;
use verne_load::Loader;

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
        /// Directory to write the GeoPackage, the features, the attachment
        /// blobs, the sidecar and the log into
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Who is running this, recorded in the extraction log
        #[arg(long, value_name = "NAME")]
        operator: String,
    },
    /// Create the datasets, domains, subtypes, relationship classes, features
    /// and attachments an extraction produced in a running ptolemy
    Load {
        /// Directory an earlier `verne extract` wrote
        path: PathBuf,
        /// Root ptolemy is served at, such as http://localhost:3000
        #[arg(long, value_name = "URL")]
        ptolemy: String,
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
        Command::Load { path, ptolemy } => load(&path, &ptolemy),
    }
}

/// The token is read from the environment and never taken as an argument: an
/// argument is in the process list of every other user on the machine.
const TOKEN_VAR: &str = "VERNE_PTOLEMY_TOKEN";

fn load(path: &Path, ptolemy: &str) -> Result<(), Box<dyn std::error::Error>> {
    let token = std::env::var(TOKEN_VAR).map_err(|_| {
        format!("set {TOKEN_VAR} to a ptolemy bearer token that may write; the load creates the datasets itself and holds the grant that gives it")
    })?;
    let sidecar_path = if path.is_dir() {
        path.join(verne_core::sidecar::SIDECAR_FILE)
    } else {
        path.to_path_buf()
    };
    // the feature files and the attachment blobs are named relative to the
    // sidecar, so the directory holding it is what the loader reads from
    let directory = sidecar_path.parent().unwrap_or(Path::new("."));
    let sidecar = Sidecar::from_json(&std::fs::read_to_string(&sidecar_path)?)?;
    let loaded = Loader::new(ptolemy, &token)?.load(&sidecar, directory)?;
    println!("loaded into {ptolemy}: {}", loaded.sentence());
    for (name, id) in &loaded.datasets {
        println!("  dataset {name} {id}");
    }
    for (name, id) in &loaded.relationships {
        println!("  relationship class {name} {id}");
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
