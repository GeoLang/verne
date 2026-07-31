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
        /// Path to a .kml or .kmz file or a .gdb directory, or the URL of an
        /// ArcGIS FeatureServer or MapServer, whole or scoped to one layer id
        source: String,
        /// Also write the report as JSON to this path
        #[arg(long, value_name = "PATH")]
        json: Option<PathBuf>,
        /// Named geodatabase version to read instead of the default, such as
        /// SDE.DEFAULT; only a versioned enterprise service has any
        #[arg(long, value_name = "NAME")]
        gdb_version: Option<String>,
    },
    /// Write a source out as a sidecar ptolemy can load: a geodatabase also
    /// gets a GeoPackage, a feature service is fetched over REST
    Extract {
        /// Path to a .gdb directory, or the URL of an ArcGIS FeatureServer or
        /// MapServer, whole or scoped to one layer id
        source: String,
        /// Directory to write the features, the attachment blobs, the sidecar
        /// and the log into (and the GeoPackage, from a geodatabase)
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
        /// Who is running this, recorded in the extraction log
        #[arg(long, value_name = "NAME")]
        operator: String,
        /// Named geodatabase version to read instead of the default
        #[arg(long, value_name = "NAME")]
        gdb_version: Option<String>,
    },
    /// List the feature services a portal holds, one URL per line
    Services {
        /// Root of the portal, such as https://www.arcgis.com
        portal: String,
        /// Only the services this account owns
        #[arg(long, value_name = "NAME")]
        owner: Option<String>,
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
        Command::Inspect {
            source,
            json,
            gdb_version,
        } => {
            let report = if is_url(&source) {
                Report::build(&open_service(&source, gdb_version)?)?
            } else {
                let path = PathBuf::from(&source);
                if is_geodatabase(&path) {
                    geodatabase(&path)?
                } else {
                    Report::build(&KmlSource::open(&path)?)?
                }
            };
            println!("{}", report.to_markdown());
            if let Some(json_path) = json {
                std::fs::write(&json_path, report.to_json() + "\n")?;
                eprintln!("wrote {}", json_path.display());
            }
            Ok(())
        }
        Command::Extract {
            source,
            out,
            operator,
            gdb_version,
        } => {
            if is_url(&source) {
                extract_service(&source, &out, &operator, gdb_version)
            } else {
                extract(&PathBuf::from(&source), &out, &operator)
            }
        }
        Command::Services { portal, owner } => services(&portal, owner.as_deref()),
        Command::Load { path, ptolemy } => load(&path, &ptolemy),
    }
}

/// The tokens are read from the environment and never taken as arguments: an
/// argument is in the process list of every other user on the machine.
const TOKEN_VAR: &str = "VERNE_PTOLEMY_TOKEN";

fn is_url(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

/// The portal a token is minted at when none is named.
const DEFAULT_PORTAL: &str = "https://www.arcgis.com";

fn open_service(
    url: &str,
    gdb_version: Option<String>,
) -> Result<verne_arcgis::ArcgisSource, Box<dyn std::error::Error>> {
    Ok(verne_arcgis::ArcgisSource::open(
        url,
        arcgis_credentials()?,
        gdb_version,
    )?)
}

/// A token the operator holds wins over an app id and secret: whoever set one
/// meant it to be used. Failing both, the service is read as the public.
fn arcgis_credentials() -> Result<verne_arcgis::Credentials, Box<dyn std::error::Error>> {
    if let Some(token) = env(verne_arcgis::TOKEN_VAR) {
        return Ok(verne_arcgis::Credentials::Token(token));
    }
    match (
        env(verne_arcgis::CLIENT_ID_VAR),
        env(verne_arcgis::CLIENT_SECRET_VAR),
    ) {
        (Some(client_id), Some(client_secret)) => {
            let portal =
                env(verne_arcgis::PORTAL_VAR).unwrap_or_else(|| DEFAULT_PORTAL.to_string());
            Ok(verne_arcgis::Credentials::ClientCredentials {
                token_url: format!("{}/sharing/rest/oauth2/token", portal.trim_end_matches('/')),
                client_id,
                client_secret,
            })
        }
        (Some(_), None) => Err(format!(
            "{} is set without {}, and half a client credential mints nothing",
            verne_arcgis::CLIENT_ID_VAR,
            verne_arcgis::CLIENT_SECRET_VAR
        )
        .into()),
        (None, Some(_)) => Err(format!(
            "{} is set without {}, and half a client credential mints nothing",
            verne_arcgis::CLIENT_SECRET_VAR,
            verne_arcgis::CLIENT_ID_VAR
        )
        .into()),
        (None, None) => Ok(verne_arcgis::Credentials::Anonymous),
    }
}

/// A variable set to nothing is a variable unset: an empty token would be sent
/// as a credential and refused.
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|held| !held.is_empty())
}

fn extract_service(
    url: &str,
    out: &Path,
    operator: &str,
    gdb_version: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let extraction = open_service(url, gdb_version)?.extract(out, operator)?;
    println!("{}", extraction.sidecar.log.to_markdown());
    eprintln!("wrote {}", extraction.sidecar_path.display());
    Ok(())
}

/// The URL comes first on the line so a listing pipes straight into an inspect,
/// and a portal holding nothing that matched is not a failure.
fn services(portal: &str, owner: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
    let fetch = verne_arcgis::HttpFetch::new(arcgis_credentials()?)?;
    let services = verne_arcgis::feature_services(&fetch, portal, owner)?;
    if services.is_empty() {
        eprintln!("no feature services matched");
        return Ok(());
    }
    for service in &services {
        println!("{}  {} ({})", service.url, service.title, service.owner);
    }
    Ok(())
}

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
