//! KML and KMZ inventory, pure Rust: no GDAL, no network, no credentials.
//!
//! `open` gets at the document and fails if it cannot. `inventory` reads what
//! is in it and fails rather than returning an empty list.

use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use verne_core::{Item, Source, SourceDescription};

mod scan;
mod verdict;

/// A file carried inside a KMZ alongside the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Kml,
    Kmz,
}

impl Container {
    fn label(self) -> &'static str {
        match self {
            Container::Kml => "KML document",
            Container::Kmz => "KMZ archive",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KmlError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{0} is not UTF-8 text, and verne will not guess an encoding")]
    Encoding(String),
    #[error("cannot open the KMZ archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("the archive holds no .kml entry, so there is no document to inventory")]
    NoDocument,
    #[error("malformed KML: {0}")]
    Xml(String),
    #[error("no <kml> root element, so this is not a KML document")]
    NotKml,
    #[error("the document ends inside {0}, so it is truncated and the inventory would be partial")]
    Truncated(String),
    #[error(
        "the document parsed but verne found nothing in it; an empty inventory must not be mistaken for a clean source"
    )]
    NothingFound,
}

impl From<quick_xml::Error> for KmlError {
    fn from(error: quick_xml::Error) -> Self {
        KmlError::Xml(error.to_string())
    }
}

impl From<quick_xml::events::attributes::AttrError> for KmlError {
    fn from(error: quick_xml::events::attributes::AttrError) -> Self {
        KmlError::Xml(error.to_string())
    }
}

impl From<quick_xml::encoding::EncodingError> for KmlError {
    fn from(error: quick_xml::encoding::EncodingError) -> Self {
        KmlError::Xml(error.to_string())
    }
}

impl From<quick_xml::escape::EscapeError> for KmlError {
    fn from(error: quick_xml::escape::EscapeError) -> Self {
        KmlError::Xml(error.to_string())
    }
}

/// A KML or KMZ file on disk, opened for reading and nothing else.
#[derive(Debug)]
pub struct KmlSource {
    path: PathBuf,
    container: Container,
    xml: String,
    entries: Vec<ArchiveEntry>,
    bytes: u64,
}

impl KmlSource {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, KmlError> {
        let path = path.as_ref().to_path_buf();
        let raw = std::fs::read(&path).map_err(|source| KmlError::Io {
            path: path.display().to_string(),
            source,
        })?;
        let bytes = raw.len() as u64;
        let zipped = raw.starts_with(b"PK\x03\x04");
        let (container, xml, entries) = if zipped {
            let (xml, entries) = read_kmz(&path, raw)?;
            (Container::Kmz, xml, entries)
        } else {
            let xml = String::from_utf8(raw)
                .map_err(|_| KmlError::Encoding(path.display().to_string()))?;
            (Container::Kml, xml, Vec::new())
        };
        Ok(KmlSource {
            path,
            container,
            xml,
            entries,
            bytes,
        })
    }

    /// Files inside the KMZ other than the document itself.
    pub fn entries(&self) -> &[ArchiveEntry] {
        &self.entries
    }
}

fn read_kmz(path: &Path, raw: Vec<u8>) -> Result<(String, Vec<ArchiveEntry>), KmlError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(raw))?;
    let names: Vec<String> = archive.file_names().map(str::to_string).collect();
    let document = names
        .iter()
        .find(|name| name.eq_ignore_ascii_case("doc.kml"))
        .or_else(|| {
            names
                .iter()
                .find(|name| name.to_ascii_lowercase().ends_with(".kml"))
        })
        .cloned()
        .ok_or(KmlError::NoDocument)?;

    let mut xml = String::new();
    archive
        .by_name(&document)?
        .read_to_string(&mut xml)
        .map_err(|_| KmlError::Encoding(format!("{}!{document}", path.display())))?;

    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        if entry.is_dir() || entry.name() == document {
            continue;
        }
        entries.push(ArchiveEntry {
            name: entry.name().to_string(),
            bytes: entry.size(),
        });
    }
    Ok((xml, entries))
}

impl Source for KmlSource {
    type Error = KmlError;

    fn describe(&self) -> SourceDescription {
        let detail = match self.container {
            Container::Kml => format!("{} bytes", self.bytes),
            Container::Kmz => format!(
                "{} bytes, {} file(s) beside the document",
                self.bytes,
                self.entries.len()
            ),
        };
        SourceDescription::new(self.container.label(), self.path.display().to_string())
            .with_detail(detail)
    }

    fn inventory(&self) -> Result<Vec<Item>, Self::Error> {
        let scan = scan::scan(&self.xml)?;
        let items = verdict::items(&scan, &self.entries);
        if items.is_empty() {
            return Err(KmlError::NothingFound);
        }
        Ok(items)
    }
}
