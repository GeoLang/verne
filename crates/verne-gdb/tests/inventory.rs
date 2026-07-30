//! Verdicts on a geodatabase built by GDAL itself. These were checked against
//! the real GeoLang components, so a change here is a claim about what a
//! component can hold.
#![cfg(feature = "gdal")]

use std::path::{Path, PathBuf};
use std::process::Command;

use verne_core::{Item, ItemKind, Outcome, Source, Verdict};
use verne_gdb::GdbSource;

/// Build the fixture with the GDAL python bindings. The tests read what GDAL
/// wrote, so nothing here can claim a capability the driver does not have.
fn fixture(dir: &Path) -> PathBuf {
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixture.py");
    let path = dir.join("fixture.gdb");
    let out = Command::new("python3")
        .arg(&script)
        .arg(&path)
        .output()
        .expect("python3 runs; the gdal feature's tests need the GDAL python bindings");
    assert!(
        out.status.success(),
        "fixture.py failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    path
}

/// Every fixture goes through here, so the invariants hold for all of them.
fn inventory(path: &Path) -> Vec<Item> {
    let source = GdbSource::open(path).expect("the fixture opens");
    let items = source.inventory().expect("the fixture inventories");
    for item in &items {
        match &item.verdict {
            Verdict::Approximated { losses, .. } => {
                assert!(losses.count() >= 1, "{} names no loss", item.location);
                assert!(
                    !item.verdict.shortfall().is_empty(),
                    "{} has an empty shortfall",
                    item.location
                );
            }
            Verdict::Unsupported { reason } => {
                assert!(!reason.is_empty(), "{} gives no reason", item.location);
                assert_eq!(item.verdict.target(), None);
            }
            Verdict::Faithful { .. } => {}
        }
    }
    items
}

fn only_matching<'a>(items: &'a [Item], kind: ItemKind, needle: &str) -> &'a Item {
    let mut matching = items.iter().filter(|item| {
        item.kind == kind && (item.detail.contains(needle) || item.location.contains(needle))
    });
    let found = matching
        .next()
        .unwrap_or_else(|| panic!("no {kind} row mentioning {needle} in {items:#?}"));
    assert!(
        matching.next().is_none(),
        "more than one {kind} row mentioning {needle}"
    );
    found
}

#[test]
fn a_feature_class_is_inventoried_with_its_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let wells = only_matching(&items, ItemKind::FeatureCollection, "wells");
    assert_eq!(wells.location, "wells");
    assert!(wells.detail.contains("Point"), "{}", wells.detail);
    assert!(wells.detail.contains("1 feature"), "{}", wells.detail);
    assert!(wells.detail.contains("well_name"), "{}", wells.detail);
    assert_eq!(
        wells.verdict.target().map(|t| t.component()),
        Some("ptolemy")
    );
}

#[test]
fn a_field_bound_to_a_domain_is_named_with_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let wells = only_matching(&items, ItemKind::FeatureCollection, "wells");
    assert!(
        wells.detail.contains("status -> status_codes"),
        "{}",
        wells.detail
    );
    assert!(
        wells.detail.contains("depth -> depth_range"),
        "{}",
        wells.detail
    );
}

#[test]
fn a_field_alias_and_a_blob_column_are_losses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let wells = only_matching(&items, ItemKind::FeatureCollection, "wells");
    assert_eq!(wells.verdict.outcome(), Outcome::Approximated);
    let shortfall = wells.verdict.shortfall();
    assert!(shortfall.contains("Well name"), "{shortfall}");
    assert!(shortfall.contains("logo"), "{shortfall}");
}

#[test]
fn a_table_without_geometry_needs_a_convention() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let inspections = only_matching(&items, ItemKind::FeatureCollection, "inspections");
    assert_eq!(inspections.verdict.outcome(), Outcome::Approximated);
    assert!(
        inspections.verdict.shortfall().contains("deletion"),
        "{}",
        inspections.verdict.shortfall()
    );
    // a plain feature class with no alias and no blob loses nothing
    let pads = only_matching(&items, ItemKind::FeatureCollection, "pads");
    assert_eq!(pads.verdict.outcome(), Outcome::Faithful);
}

#[test]
fn the_system_tables_are_listed_and_unsupported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let system = only_matching(&items, ItemKind::Metadata, "GDB_Items");
    assert_eq!(system.verdict.outcome(), Outcome::Unsupported);
    assert!(system.detail.contains("GDB_SystemCatalog"), "{system:#?}");
}

#[test]
fn a_path_that_is_not_a_geodatabase_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("plain.txt");
    std::fs::write(&file, "not a geodatabase").expect("write");
    assert!(GdbSource::open(&file).is_err());
    assert!(GdbSource::open(dir.path()).is_err());
}
