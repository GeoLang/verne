//! Esri file geodatabase inventory, read-only, through GDAL's OpenFileGDB driver.
//!
//! GDAL sits behind the `gdal` feature, so the rest of the workspace builds and
//! tests without it. The driver is the open one that ships with GDAL: verne
//! never loads Esri's FileGDB SDK, and the open call names the driver it allows.

/// The Esri layer definition XML. GDAL hands it over without reading it, so
/// this is plain XML work and needs no GDAL.
pub mod definition;

#[cfg(feature = "gdal")]
mod extract;
#[cfg(feature = "gdal")]
mod glue;
#[cfg(feature = "gdal")]
mod scan;
#[cfg(feature = "gdal")]
mod source;
#[cfg(feature = "gdal")]
mod verdict;

#[cfg(feature = "gdal")]
pub use extract::{Extraction, SIDECAR_FILE};
#[cfg(feature = "gdal")]
pub use source::{GdbError, GdbSource};
