//! Opening a KML or a KMZ, and failing loudly when it is neither.

use std::io::{Cursor, Write};
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use verne_core::{ItemKind, Outcome, Source};
use verne_kml::{KmlError, KmlSource};

const DOC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<kml xmlns="http://www.opengis.net/kml/2.2">
  <Document>
    <name>Fixture</name>
    <Placemark><name>a</name><Point><coordinates>1,2</coordinates></Point></Placemark>
  </Document>
</kml>
"#;

fn named_doc(name: &str) -> String {
    DOC.replace("Fixture", name)
}

fn write(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).expect("write fixture");
    path
}

fn kmz(dir: &TempDir, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (entry, bytes) in files {
        writer.start_file(*entry, options).expect("start entry");
        writer.write_all(bytes).expect("write entry");
    }
    let archive = writer.finish().expect("finish archive").into_inner();
    write(dir, name, &archive)
}

/// open only reads the bytes, so the parse errors surface from inventory.
fn error(path: impl AsRef<Path>) -> KmlError {
    match KmlSource::open(path) {
        Err(error) => error,
        Ok(source) => source.inventory().expect_err("the fixture should fail"),
    }
}

#[test]
fn a_kml_file_carries_no_entries() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write(&dir, "doc.kml", DOC.as_bytes());
    let source = KmlSource::open(&path).expect("the fixture opens");
    assert_eq!(source.describe().format, "KML document");
    assert!(source.entries().is_empty());
}

#[test]
fn a_kmz_reports_the_files_beside_the_document() {
    let dir = tempfile::tempdir().expect("tempdir");
    let pin: &[u8] = b"pretend png";
    let overlay: &[u8] = b"pretend jpeg bytes";
    let path = kmz(
        &dir,
        "bundle.kmz",
        &[
            ("doc.kml", DOC.as_bytes()),
            ("icons/pin.png", pin),
            ("overlay.jpg", overlay),
        ],
    );
    let source = KmlSource::open(&path).expect("the archive opens");
    assert_eq!(source.describe().format, "KMZ archive");

    let entries = source.entries();
    assert_eq!(entries.len(), 2, "{entries:#?}");
    assert!(entries.iter().all(|entry| entry.name != "doc.kml"));
    let pin_entry = entries
        .iter()
        .find(|entry| entry.name == "icons/pin.png")
        .expect("the icon is listed");
    assert_eq!(pin_entry.bytes, pin.len() as u64);
    let overlay_entry = entries
        .iter()
        .find(|entry| entry.name == "overlay.jpg")
        .expect("the overlay is listed");
    assert_eq!(overlay_entry.bytes, overlay.len() as u64);

    let items = source.inventory().expect("the archive inventories");
    let embedded = items
        .iter()
        .find(|item| item.kind == ItemKind::EmbeddedResource)
        .expect("an embedded resource row");
    assert_eq!(embedded.verdict.outcome(), Outcome::Approximated);
    assert_eq!(
        embedded.verdict.target().map(|target| target.component()),
        Some("ptolemy")
    );
    assert!(embedded.verdict.shortfall().contains("attachment"));
}

#[test]
fn doc_kml_wins_over_another_kml_in_the_archive() {
    let dir = tempfile::tempdir().expect("tempdir");
    let other = named_doc("Other");
    let chosen = named_doc("Chosen");
    let path = kmz(
        &dir,
        "bundle.kmz",
        &[
            ("other.kml", other.as_bytes()),
            ("doc.kml", chosen.as_bytes()),
        ],
    );
    let source = KmlSource::open(&path).expect("the archive opens");
    let items = source.inventory().expect("the archive inventories");
    let metadata = items
        .iter()
        .find(|item| item.kind == ItemKind::Metadata)
        .expect("a metadata row");
    assert!(metadata.detail.contains("Chosen"), "{}", metadata.detail);
    assert!(!metadata.detail.contains("Other"));
}

#[test]
fn a_kmz_without_a_kml_entry_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = kmz(&dir, "bundle.kmz", &[("icons/pin.png", b"pretend png")]);
    assert!(matches!(error(&path), KmlError::NoDocument));
}

#[test]
fn xml_that_is_not_kml_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write(&dir, "root.kml", b"<root/>");
    assert!(matches!(error(&path), KmlError::NotKml));
}

#[test]
fn a_missing_path_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let error = error(dir.path().join("absent.kml"));
    match error {
        KmlError::Io { path, .. } => assert!(path.ends_with("absent.kml")),
        other => panic!("expected an io error, got {other:?}"),
    }
}

#[test]
fn truncated_document_fails() {
    // a partial inventory of a cut-off file must not pass for a clean source
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write(
        &dir,
        "cut.kml",
        b"<kml><Document><name>a</name><Placemark><Point>",
    );
    match error(&path) {
        KmlError::Truncated(open) => assert!(open.contains("Placemark")),
        other => panic!("expected a truncation error, got {other:?}"),
    }
}

#[test]
fn malformed_xml_fails() {
    // a mismatched end tag, not a truncated one: quick_xml reaches Eof without
    // complaining about elements left open
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write(
        &dir,
        "broken.kml",
        b"<kml><Document><name>a</name></Documnt></kml>",
    );
    match error(&path) {
        KmlError::Xml(message) => assert!(!message.is_empty()),
        other => panic!("expected an xml error, got {other:?}"),
    }
}

#[test]
fn non_utf8_bytes_fail() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = write(
        &dir,
        "latin.kml",
        b"<kml><Document><name>\xff\xfe</name></Document></kml>",
    );
    match error(&path) {
        KmlError::Encoding(named) => assert!(named.ends_with("latin.kml")),
        other => panic!("expected an encoding error, got {other:?}"),
    }
}
