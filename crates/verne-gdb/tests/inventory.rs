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
            Verdict::NotApplicable { reason } => {
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
        inspections
            .verdict
            .shortfall()
            .contains("empty geometry collection"),
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

#[test]
fn a_coded_domain_carries_its_values_and_names_its_binding() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let coded = only_matching(&items, ItemKind::AttributeSchema, "status_codes");
    assert!(coded.detail.contains("A=Active"), "{}", coded.detail);
    assert!(coded.detail.contains("P=Plugged"), "{}", coded.detail);
    assert!(
        coded.detail.contains("used by wells.status"),
        "{}",
        coded.detail
    );
    assert_eq!(coded.verdict.outcome(), Outcome::Approximated);
    assert_eq!(
        coded.verdict.target().map(|t| t.component()),
        Some("ptolemy")
    );
    let shortfall = coded.verdict.shortfall();
    // the values themselves go across; the binding and the description do not
    assert!(shortfall.contains("domain_assignments"), "{shortfall}");
    assert!(shortfall.contains("well status"), "{shortfall}");
}

#[test]
fn a_range_domain_carries_both_bounds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let range = only_matching(&items, ItemKind::AttributeSchema, "depth_range");
    assert!(
        range.detail.contains("range: 0 to 5000"),
        "{}",
        range.detail
    );
    // both ends are included, so nothing is said about inclusivity
    assert!(
        !range.verdict.shortfall().contains("part of the range"),
        "{}",
        range.verdict.shortfall()
    );
}

#[test]
fn a_composite_relationship_names_the_cascade_it_loses() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let related = only_matching(&items, ItemKind::Relationship, "wells_inspections");
    assert!(
        related
            .detail
            .contains("wells.OBJECTID -> inspections.well_id"),
        "{}",
        related.detail
    );
    assert!(related.detail.contains("one to many"), "{}", related.detail);
    assert!(
        related.detail.contains("has inspections"),
        "{}",
        related.detail
    );
    assert_eq!(related.verdict.outcome(), Outcome::Approximated);
    let shortfall = related.verdict.shortfall();
    assert!(shortfall.contains("is_composite"), "{shortfall}");
    assert!(shortfall.contains("origin_primary_key"), "{shortfall}");
}

#[test]
fn subtypes_come_through_the_definition_xml() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let subtypes = only_matching(&items, ItemKind::AttributeSchema, "subtypes on");
    assert!(
        subtypes.detail.contains("2 subtypes on wells.status"),
        "{}",
        subtypes.detail
    );
    assert!(
        subtypes.detail.contains("1 Active well"),
        "{}",
        subtypes.detail
    );
    assert!(subtypes.detail.contains("default 1"), "{}", subtypes.detail);
    assert_eq!(subtypes.verdict.outcome(), Outcome::Approximated);
    let shortfall = subtypes.verdict.shortfall();
    assert!(
        shortfall.contains("which code is the default"),
        "{shortfall}"
    );
    assert!(shortfall.contains("depth_range"), "{shortfall}");
}

#[test]
fn an_annotation_class_keeps_its_data_and_loses_its_graphics() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    // the geometry and fields are an ordinary layer, and say where the rest went
    let layer = only_matching(&items, ItemKind::FeatureCollection, "well_labels");
    assert!(
        layer.detail.contains("graphics reported below"),
        "{}",
        layer.detail
    );

    let graphics = only_matching(&items, ItemKind::Styling, "esriFTAnnotation");
    assert_eq!(graphics.location, "well_labels");
    assert_eq!(graphics.verdict.outcome(), Outcome::Unsupported);
    assert!(graphics.verdict.shortfall().contains("placement"));
}

#[test]
fn an_attachment_table_goes_to_the_attachments_table() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let attachments = only_matching(&items, ItemKind::EmbeddedResource, "wells__ATTACH");
    assert!(
        attachments.detail.contains("attachments on wells"),
        "{}",
        attachments.detail
    );
    assert!(
        attachments.detail.contains("1 row"),
        "{}",
        attachments.detail
    );
    assert_eq!(attachments.verdict.outcome(), Outcome::Approximated);
    assert!(attachments.verdict.shortfall().contains("branch"));

    // the media relationship is reported here, not a second time as a class
    assert!(
        !items
            .iter()
            .any(|item| item.kind == ItemKind::Relationship && item.detail.contains("wells_attach")),
        "{items:#?}"
    );
}

#[test]
fn a_blob_table_with_no_relationship_is_still_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let orphan = only_matching(&items, ItemKind::EmbeddedResource, "pads__ATTACH");
    assert!(
        orphan.detail.contains("no relationship pointing at it"),
        "{}",
        orphan.detail
    );
    assert_eq!(orphan.verdict.outcome(), Outcome::Approximated);
}

#[test]
fn a_feature_dataset_is_a_grouping_ptolemy_has_no_container_for() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let group = only_matching(&items, ItemKind::Hierarchy, "Water");
    assert!(group.detail.contains("pads"), "{}", group.detail);
    assert_eq!(group.verdict.outcome(), Outcome::Approximated);
    assert!(
        group
            .verdict
            .shortfall()
            .contains("no container above a dataset")
    );
}

#[test]
fn a_metadata_record_is_carried_as_catalogue_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let metadata = only_matching(&items, ItemKind::Metadata, "ISO or FGDC");
    assert_eq!(metadata.location, "wells");
    assert_eq!(metadata.verdict.outcome(), Outcome::Approximated);
    assert!(metadata.verdict.shortfall().contains("lineage"));
}

#[test]
fn an_item_gdal_cannot_read_is_named_and_nothing_more() {
    let dir = tempfile::tempdir().expect("tempdir");
    let items = inventory(&fixture(dir.path()));

    let topology = only_matching(&items, ItemKind::DataModel, "DETopology");
    assert_eq!(topology.location, "Water_Topology");
    assert_eq!(topology.verdict.outcome(), Outcome::Unsupported);

    // a domain is read through the C API, so it is not an item verne failed on
    assert!(
        !items
            .iter()
            .any(|item| item.kind == ItemKind::DataModel && item.detail.contains("status_codes")),
        "{items:#?}"
    );
}

#[test]
fn versioning_does_not_arise_for_a_file_geodatabase() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = fixture(dir.path());
    let items = inventory(&path);

    let versioning = only_matching(&items, ItemKind::Temporal, "versioning");
    assert_eq!(versioning.verdict.outcome(), Outcome::NotApplicable);
    assert_eq!(versioning.verdict.target(), None);
    assert!(versioning.verdict.shortfall().contains("enterprise"));

    let source = GdbSource::open(&path).expect("opens");
    let report = verne_core::Report::build(&source).expect("builds");
    assert_eq!(report.summary.not_applicable, 1);
    assert!(
        report.summary.sentence().contains("1 not applicable"),
        "{}",
        report.summary.sentence()
    );
}
