use std::path::{Path, PathBuf};

use gdal::{Dataset, DatasetOptions, GdalOpenFlags};
use verne_core::{Item, Source, SourceDescription};

use crate::{scan, verdict};

/// The open driver, named so that Esri's FileGDB SDK driver is never picked up
/// even where someone has built GDAL with it.
const DRIVER: &str = "OpenFileGDB";

#[derive(Debug, thiserror::Error)]
pub enum GdbError {
    #[error("{0} is not a directory, and a file geodatabase is a directory of tables")]
    NotADirectory(String),
    #[error("cannot open {path} with {DRIVER}: {source}")]
    Open {
        path: String,
        #[source]
        source: gdal::errors::GdalError,
    },
    #[error(
        "the geodatabase holds no tables at all; an empty inventory must not be mistaken for a clean source"
    )]
    NothingFound,
    #[error("cannot write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

/// A file geodatabase on disk, opened for reading and nothing else.
pub struct GdbSource {
    path: PathBuf,
    pub(crate) dataset: Dataset,
}

impl GdbSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, GdbError> {
        let path = path.as_ref().to_path_buf();
        if !path.is_dir() {
            return Err(GdbError::NotADirectory(path.display().to_string()));
        }
        let options = DatasetOptions {
            open_flags: GdalOpenFlags::GDAL_OF_VECTOR | GdalOpenFlags::GDAL_OF_READONLY,
            allowed_drivers: Some(&[DRIVER]),
            // system and __ATTACH tables are part of what the report has to name
            open_options: Some(&["LIST_ALL_TABLES=YES"]),
            sibling_files: None,
        };
        let dataset = Dataset::open_ex(&path, options).map_err(|source| GdbError::Open {
            path: path.display().to_string(),
            source,
        })?;
        Ok(GdbSource { path, dataset })
    }
}

impl Source for GdbSource {
    type Error = GdbError;

    fn describe(&self) -> SourceDescription {
        let tables = self.dataset.layer_count();
        SourceDescription::new("Esri file geodatabase", self.path.display().to_string())
            .with_detail(format!(
                "{tables} tables, read with {DRIVER} on {}",
                gdal::version::VersionInfo::version_summary()
                    .split(',')
                    .next()
                    .unwrap_or("GDAL")
                    .trim()
            ))
    }

    fn inventory(&self) -> Result<Vec<Item>, Self::Error> {
        let scan = scan::scan(&self.dataset);
        let items = verdict::items(&scan);
        if items.is_empty() {
            return Err(GdbError::NothingFound);
        }
        Ok(items)
    }
}
