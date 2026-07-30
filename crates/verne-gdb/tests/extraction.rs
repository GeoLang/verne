//! Extraction of a geodatabase into a sidecar and a log. No ptolemy: every
//! assertion here is about the file verne writes, not about a load.
#![cfg(feature = "gdal")]

use std::path::{Path, PathBuf};
use std::process::Command;

use verne_core::sidecar::Action;
use verne_core::{Item, ItemKind, Report, SIDECAR_FILE, Sidecar, Verdict};
use verne_gdb::{GdbSource, serialised};

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

struct Extracted {
    sidecar: Sidecar,
    report: Report,
    geopackage: PathBuf,
    /// Where the extraction landed, so the feature files and the blobs it
    /// names can be read back off disk.
    directory: PathBuf,
    _dir: tempfile::TempDir,
}

fn extract() -> Extracted {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = GdbSource::open(fixture(dir.path())).expect("the fixture opens");
    let report = Report::build(&source).expect("the fixture inventories");
    let extraction = source
        .extract(&dir.path().join("out"), "operator@example.test")
        .expect("the fixture extracts");
    // what the caller is handed is what landed on disk
    let written =
        std::fs::read_to_string(&extraction.sidecar_path).expect("the sidecar is written");
    assert_eq!(
        extraction.sidecar_path,
        dir.path().join("out").join(SIDECAR_FILE)
    );
    assert_eq!(
        Sidecar::from_json(&written).expect("the sidecar parses"),
        extraction.sidecar
    );
    Extracted {
        geopackage: extraction.geopackage_path.expect("a GeoPackage is written"),
        directory: extraction.directory,
        sidecar: extraction.sidecar,
        report,
        _dir: dir,
    }
}

/// The point of the log: a row of the report and its entry cannot give
/// different accounts of the same thing. Every item is answered for exactly
/// once, and an approximated item is logged with the report's own losses.
#[test]
fn the_log_answers_for_every_row_of_the_report() {
    let extracted = extract();
    let entries = &extracted.sidecar.log.entries;

    for item in &extracted.report.items {
        let matching: Vec<_> = entries
            .iter()
            .filter(|entry| {
                entry.location == item.location
                    && entry.kind == item.kind
                    && entry.detail == item.detail
            })
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "{} ({}) has {} log entries",
            item.location,
            item.kind,
            matching.len()
        );
        agrees(item, &matching[0].action);
    }
}

fn agrees(item: &Item, action: &Action) {
    match (&item.verdict, action) {
        (Verdict::Faithful { .. }, Action::Carried) => {}
        (Verdict::Faithful { .. }, Action::Skipped { reason }) => {
            assert!(
                !reason.is_empty(),
                "{} skipped without a reason",
                item.location
            )
        }
        (Verdict::Approximated { losses, .. }, Action::CarriedWithLoss { losses: logged }) => {
            let named: Vec<&str> = losses.iter().collect();
            assert_eq!(logged.len(), named.len(), "{}", item.location);
            for (a, b) in named.iter().zip(logged) {
                assert_eq!(a, b, "{}", item.location);
            }
        }
        (Verdict::Approximated { .. }, Action::Skipped { reason }) => {
            assert!(
                !reason.is_empty(),
                "{} skipped without a reason",
                item.location
            )
        }
        (Verdict::Unsupported { reason }, Action::Skipped { reason: logged })
        | (Verdict::NotApplicable { reason }, Action::Skipped { reason: logged }) => {
            assert_eq!(reason, logged, "{}", item.location)
        }
        (verdict, action) => panic!("{} logged {action:?} against {verdict:?}", item.location),
    }
}

/// The three losses that were checked against ptolemy's routes: they are in the
/// report, so they have to be in the log.
#[test]
fn the_log_names_the_losses_ptolemy_cannot_take() {
    let extracted = extract();
    let losses: Vec<&str> = extracted
        .sidecar
        .log
        .entries
        .iter()
        .filter_map(|entry| match &entry.action {
            Action::CarriedWithLoss { losses } => Some(losses),
            _ => None,
        })
        .flatten()
        .map(String::as_str)
        .collect();
    let all = losses.join("\n");

    // a composite class's cascade has a column but no route sets it
    assert!(all.contains("is_composite"), "{all}");
    // a domain's description has a column the create route does not take
    assert!(all.contains("the description"), "{all}");
    // a domain binds to a field only through a subtype
    assert!(all.contains("domain_assignments"), "{all}");
    // the origin key cannot be said through the API
    assert!(all.contains("origin_primary_key"), "{all}");
}

#[test]
fn every_user_table_becomes_a_dataset() {
    let extracted = extract();
    let names: Vec<&str> = extracted
        .sidecar
        .datasets
        .iter()
        .map(|plan| plan.dataset.name.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "wells",
            "pads",
            "well_labels",
            "plots",
            "stray_points",
            "inspections"
        ]
    );

    let wells = extracted.sidecar.dataset("wells").expect("wells");
    assert_eq!(wells.source_table, "wells");
    assert_eq!(wells.dataset.geometry_type, "point");
    assert_eq!(wells.dataset.srid, 4326);
    assert_eq!(wells.dataset.created_by, "operator@example.test");

    // a table with no geometry still needs one of ptolemy's eight names
    let inspections = extracted
        .sidecar
        .dataset("inspections")
        .expect("inspections");
    assert_eq!(inspections.dataset.geometry_type, "geometry");
}

/// A blob table is not a dataset. Its rows go to ptolemy's attachments, on the
/// features they belong to, and the log says which.
#[test]
fn an_attachment_table_is_carried_as_attachments_and_not_as_a_dataset() {
    let extracted = extract();
    assert!(extracted.sidecar.dataset("wells__ATTACH").is_none());
    assert!(extracted.sidecar.dataset("GDB_Items").is_none());

    let attachments = entry(&extracted, "wells__ATTACH");
    let Action::CarriedWithLoss { losses } = &attachments.action else {
        panic!("the fixture's one attachment is carried: {attachments:?}");
    };
    assert_eq!(
        attachments.destination.as_deref(),
        Some("attachments of wells__ATTACH")
    );
    // what ptolemy's attachment cannot hold has to be named
    assert!(
        losses.iter().any(|loss| loss.contains("REL_OBJECTID")),
        "{losses:?}"
    );
}

fn entry<'a>(extracted: &'a Extracted, location: &str) -> &'a verne_core::sidecar::LogEntry {
    extracted
        .sidecar
        .log
        .entries
        .iter()
        .find(|entry| entry.location == location)
        .unwrap_or_else(|| panic!("no log entry for {location}"))
}

#[test]
fn a_coded_domain_carries_its_codes_and_labels() {
    let extracted = extract();
    let wells = extracted.sidecar.dataset("wells").expect("wells");
    let coded = wells
        .domains
        .iter()
        .find(|domain| domain.name == "status_codes")
        .expect("status_codes");

    assert_eq!(coded.domain_type, "coded_value");
    assert_eq!(coded.field_type, "string");
    let values = coded.coded_values.as_ref().expect("coded values");
    assert_eq!(values[0]["code"], "A");
    assert_eq!(values[0]["name"], "Active");
    assert_eq!(values[1]["code"], "P");
    assert!(coded.range_min.is_none());
}

#[test]
fn a_range_domain_carries_both_bounds() {
    let extracted = extract();
    let wells = extracted.sidecar.dataset("wells").expect("wells");
    let range = wells
        .domains
        .iter()
        .find(|domain| domain.name == "depth_range")
        .expect("depth_range");

    assert_eq!(range.domain_type, "range");
    assert_eq!(range.field_type, "integer");
    assert_eq!(range.range_min, Some(0.0));
    assert_eq!(range.range_max, Some(5000.0));
    assert!(range.coded_values.is_none());
}

/// A geodatabase holds one domain for the whole workspace and ptolemy holds one
/// per dataset, so a domain two tables use is written twice and the log says
/// the two copies have come apart.
#[test]
fn a_shared_domain_is_copied_into_every_dataset_that_uses_it() {
    let extracted = extract();
    for dataset in ["wells", "inspections"] {
        let plan = extracted.sidecar.dataset(dataset).expect(dataset);
        assert!(
            plan.domains.iter().any(|d| d.name == "status_codes"),
            "{dataset} has no status_codes: {:?}",
            plan.domains
        );
    }

    let duplication = extracted
        .sidecar
        .log
        .entries
        .iter()
        .find(|entry| entry.detail.contains("copied into"))
        .expect("the duplication is logged");
    let Action::CarriedWithLoss { losses } = &duplication.action else {
        panic!("copying a domain is a loss: {duplication:?}");
    };
    assert!(
        losses[0].contains("no longer changes the others"),
        "{losses:?}"
    );
}

/// A domain only reaches the datasets that name it, so a dataset that binds
/// none gets none rather than a copy of the workspace's whole list.
#[test]
fn a_dataset_that_binds_no_domain_gets_none() {
    let extracted = extract();
    for dataset in ["pads", "well_labels"] {
        let plan = extracted.sidecar.dataset(dataset).expect(dataset);
        assert!(plan.domains.is_empty(), "{dataset}: {:?}", plan.domains);
        assert!(plan.subtypes.is_empty(), "{dataset}: {:?}", plan.subtypes);
    }
    let inspections = extracted
        .sidecar
        .dataset("inspections")
        .expect("inspections");
    let names: Vec<&str> = inspections
        .domains
        .iter()
        .map(|domain| domain.name.as_str())
        .collect();
    assert_eq!(names, ["status_codes"]);
}

#[test]
fn subtypes_carry_their_codes_defaults_and_domain_names() {
    let extracted = extract();
    let wells = extracted.sidecar.dataset("wells").expect("wells");
    assert_eq!(wells.subtypes.len(), 2);

    let active = &wells.subtypes[0];
    assert_eq!(active.subtype_field, "status");
    assert_eq!(active.name, "Active well");
    assert_eq!(active.code, 1);
    assert_eq!(active.default_values["depth"], "100");
    // the domain is named, not identified: no id exists until the load runs
    assert_eq!(active.domain_assignments["depth"], "depth_range");

    let plugged = &wells.subtypes[1];
    assert_eq!(plugged.code, 2);
    assert!(plugged.domain_assignments.is_empty());
}

/// The definition XML declares each default's type in an attribute verne does
/// not read, so a default arrives as text and the log says so.
#[test]
fn a_subtype_default_is_written_as_the_text_it_appears_as() {
    let extracted = extract();
    let note = extracted
        .sidecar
        .log
        .entries
        .iter()
        .find(|entry| entry.detail.contains("subtype default value"))
        .expect("the untyped defaults are logged");
    let Action::CarriedWithLoss { losses } = &note.action else {
        panic!("an untyped default is a loss: {note:?}");
    };
    assert!(losses[0].contains("Active well.depth"), "{losses:?}");
}

#[test]
fn a_relationship_class_names_two_datasets_and_the_key_between_them() {
    let extracted = extract();
    assert_eq!(extracted.sidecar.relationships.len(), 1);
    let class = &extracted.sidecar.relationships[0];

    assert_eq!(class.name, "wells_inspections");
    assert_eq!(class.origin_dataset, "wells");
    assert_eq!(class.destination_dataset, "inspections");
    // the field on the destination side holding the origin's key
    assert_eq!(class.origin_foreign_key, "well_id");
    assert_eq!(class.cardinality, "one_to_many");
    assert_eq!(class.forward_label, "has inspections");
    assert_eq!(class.backward_label, "inspected well");
}

/// An attachment relationship is not a class in ptolemy, and attachments are
/// not extracted at all.
#[test]
fn an_attachment_relationship_is_not_a_relationship_class() {
    let extracted = extract();
    assert!(
        !extracted
            .sidecar
            .relationships
            .iter()
            .any(|class| class.name == "wells_attach"),
        "{:?}",
        extracted.sidecar.relationships
    );
}

#[test]
fn the_log_records_who_ran_it_and_when() {
    let extracted = extract();
    let log = &extracted.sidecar.log;
    assert_eq!(log.operator, "operator@example.test");
    // RFC 3339, UTC
    assert!(log.extracted_at.ends_with('Z'), "{}", log.extracted_at);
    assert!(log.extracted_at.len() >= 20, "{}", log.extracted_at);
    assert!(log.counts().total >= extracted.report.items.len());
    assert!(log.counts().skipped > 0, "{}", log.counts().sentence());
}

// ─── The GeoPackage ─────────────────────────────────────────────────

#[test]
fn every_dataset_names_the_layer_its_features_went_to() {
    let extracted = extract();
    assert_eq!(
        extracted.sidecar.geopackage.as_deref(),
        Some("features.gpkg")
    );
    for plan in &extracted.sidecar.datasets {
        assert_eq!(
            plan.layer.as_deref(),
            Some(plan.source_table.as_str()),
            "{} names no layer",
            plan.source_table
        );
    }
}

#[test]
fn the_geopackage_holds_one_layer_per_dataset() {
    let extracted = extract();
    // reading one back is opening and closing a GeoPackage like any other, so
    // it goes through the same guard: see verne_gdb::geopackage
    let (names, count) = serialised(|| {
        let written = gdal::Dataset::open(&extracted.geopackage).expect("the GeoPackage opens");
        let names: Vec<String> = written
            .layers()
            .map(|layer| gdal::vector::LayerAccess::name(&layer))
            .collect();
        (names, written.layer_count())
    });
    // a GeoPackage lists its layers in its own order, spatial ones first, so
    // this is a set and not a sequence
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        [
            "inspections",
            "pads",
            "plots",
            "stray_points",
            "well_labels",
            "wells"
        ]
    );
    // the system and attachment tables were not asked for
    assert_eq!(count, 6);
}

/// OBJECTID is the geodatabase's feature id rather than a field, and a
/// relationship class is keyed on it, so it has to survive as the GeoPackage's
/// own feature id and not as a number GDAL invented.
#[test]
fn the_features_keep_the_ids_a_relationship_is_keyed_on() {
    use gdal::vector::LayerAccess;

    let extracted = extract();
    let (count, fid, name, has_geometry) = serialised(|| {
        let written = gdal::Dataset::open(&extracted.geopackage).expect("the GeoPackage opens");
        let mut wells = written.layer_by_name("wells").expect("wells");
        let count = wells.feature_count();
        let feature = wells.features().next().expect("one well");
        let index = feature.field_index("well_name").expect("well_name");
        (
            count,
            feature.fid(),
            feature.field_as_string(index).expect("read"),
            feature.geometry().is_some(),
        )
    });

    assert_eq!(count, 1);
    assert_eq!(fid, Some(1));
    assert_eq!(name, Some("Alpha".to_string()));
    assert!(has_geometry, "the point came across");
}

/// The report's losses are losses at ptolemy. The GeoPackage keeps more than
/// that, and the log has to be about the file that was written and not about
/// what will happen to it later.
#[test]
fn the_log_reports_the_geopackage_separately_from_the_dataset() {
    let extracted = extract();
    let rows: Vec<_> = extracted
        .sidecar
        .log
        .entries
        .iter()
        .filter(|entry| {
            entry
                .destination
                .as_deref()
                .is_some_and(|to| to.starts_with("features.gpkg"))
        })
        .collect();
    assert_eq!(rows.len(), 6, "one per dataset: {rows:#?}");

    let wells = rows
        .iter()
        .find(|entry| entry.location == "wells")
        .expect("wells");
    assert_eq!(wells.detail, "1 feature");
    let Action::CarriedWithLoss { losses } = &wells.action else {
        panic!("the fid story is a loss: {wells:?}");
    };
    assert!(losses.iter().any(|l| l.contains("OBJECTID")), "{losses:?}");
    // no field was dropped and no layer renamed, so nothing claims either
    assert!(
        !losses.iter().any(|l| l.contains("not in the layer")),
        "{losses:?}"
    );
    assert!(
        !losses.iter().any(|l| l.contains("could not use")),
        "{losses:?}"
    );
}

/// The load that used to abort the whole binary: several threads extracting at
/// once, each writing its own GeoPackage. On GDAL 3.8 an unguarded close tears
/// down libxml2's global encoding table twice and glibc kills the process, so
/// this passing is the whole of what `geopackage::serialised` is for. It passes
/// on GDAL 3.11 either way, which is why the guard also carries a debug
/// assertion that does not depend on the version.
#[test]
fn many_threads_can_extract_at_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let geodatabase = fixture(dir.path());
    let out = dir.path().join("concurrent");

    let extracted: Vec<usize> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|n| {
                let geodatabase = geodatabase.clone();
                let out = out.join(n.to_string());
                scope.spawn(move || {
                    let source = GdbSource::open(&geodatabase).expect("the fixture opens");
                    let extraction = source
                        .extract(&out, "operator@example.test")
                        .expect("the fixture extracts");
                    extraction.sidecar.datasets.len()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("no thread panicked"))
            .collect()
    });

    assert_eq!(extracted, vec![6; 8]);
}

// ─── The dataset schema ─────────────────────────────────────────────

#[test]
fn every_column_reaches_the_schema_with_its_type() {
    let extracted = extract();
    let wells = extracted.sidecar.dataset("wells").expect("wells");
    let named: Vec<(&str, &str)> = wells
        .schema
        .fields
        .iter()
        .map(|field| (field.name.as_str(), field.field_type.as_str()))
        .collect();

    assert_eq!(
        named,
        [
            ("well_name", "string"),
            ("depth", "integer"),
            ("status", "string"),
            // ptolemy has no field type for bytes
            ("logo", "string"),
        ]
    );
    // the fixture's columns all take a null, so ptolemy is asked for nothing
    // the geodatabase did not ask for either
    assert!(wells.schema.fields.iter().all(|field| !field.required));
}

/// The whole point: the label a reader knows the column by now has somewhere to
/// go, and a column that never had one does not gain an empty label.
#[test]
fn a_field_alias_is_carried_onto_the_schema() {
    let extracted = extract();
    let wells = extracted.sidecar.dataset("wells").expect("wells");

    let name = &wells.schema.fields[0];
    assert_eq!(name.name, "well_name");
    assert_eq!(name.alias.as_deref(), Some("Well name"));
    assert!(
        wells.schema.fields[1..]
            .iter()
            .all(|field| field.alias.is_none()),
        "{:?}",
        wells.schema.fields
    );
}

#[test]
fn a_table_with_no_columns_gets_an_empty_schema() {
    let extracted = extract();
    let pads = extracted.sidecar.dataset("pads").expect("pads");
    assert!(pads.schema.is_empty(), "{:?}", pads.schema);
}

/// A type ptolemy cannot name exactly is an approximation, and both the report
/// and the log have to say which column and which type.
#[test]
fn a_column_type_ptolemy_cannot_name_is_a_loss_in_both() {
    let extracted = extract();

    let wells = only(
        &extracted.report.items,
        ItemKind::FeatureCollection,
        "wells",
    );
    let shortfall = wells.verdict.shortfall();
    assert!(shortfall.contains("logo (Binary)"), "{shortfall}");
    assert!(
        shortfall.contains("string, integer, float, boolean, array and object"),
        "{shortfall}"
    );

    let note = extracted
        .sidecar
        .log
        .entries
        .iter()
        .find(|entry| entry.detail.contains("nearest type ptolemy has"))
        .expect("the log records the approximation");
    assert_eq!(note.destination.as_deref(), Some("schema of wells"));
    let Action::CarriedWithLoss { losses } = &note.action else {
        panic!("an approximated type is a loss: {note:?}");
    };
    assert!(losses[0].contains("Binary column"), "{losses:?}");
}

/// The alias reaches ptolemy now, so the report must stop saying it merely
/// could be, and must not overclaim either: nothing displays it.
#[test]
fn the_report_says_an_alias_is_stored_but_never_shown() {
    let extracted = extract();
    let wells = only(
        &extracted.report.items,
        ItemKind::FeatureCollection,
        "wells",
    );
    let shortfall = wells.verdict.shortfall();

    assert!(shortfall.contains("well_name \"Well name\""), "{shortfall}");
    assert!(shortfall.contains("reach ptolemy"), "{shortfall}");
    assert!(shortfall.contains("never shown"), "{shortfall}");
    // the old claim, which was wrong: an extra JSON key was silently dropped
    assert!(!shortfall.contains("free-form JSON"), "{shortfall}");
}

fn only<'a>(items: &'a [Item], kind: ItemKind, location: &str) -> &'a Item {
    items
        .iter()
        .find(|item| item.kind == kind && item.location == location)
        .unwrap_or_else(|| panic!("no {kind} row for {location}"))
}

// ─── The features and the attachments ───────────────────────────────

fn features(extracted: &Extracted, dataset: &str) -> Vec<serde_json::Value> {
    let plan = extracted
        .sidecar
        .dataset(dataset)
        .unwrap_or_else(|| panic!("no plan for {dataset}"));
    let named = plan
        .features
        .as_deref()
        .unwrap_or_else(|| panic!("{dataset} names no feature file"));
    let text = std::fs::read_to_string(extracted.directory.join(named))
        .unwrap_or_else(|_| panic!("{named} is written"));
    text.lines()
        .map(|line| serde_json::from_str(line).expect("every line is one operation"))
        .collect()
}

/// The whole reason the features are written a second time: the loader builds
/// without GDAL, so what it gets is a file of ptolemy's own insert operations.
#[test]
fn every_dataset_names_a_file_of_insert_operations() {
    let extracted = extract();
    for plan in &extracted.sidecar.datasets {
        // stray_points has no spatial reference, so nothing of it can be sent
        let expected = (plan.source_table != "stray_points")
            .then(|| format!("features/{}.ndjson", plan.source_table));
        assert_eq!(
            plan.features, expected,
            "{} names the wrong feature file",
            plan.source_table
        );
    }

    let wells = features(&extracted, "wells");
    assert_eq!(wells.len(), 1);
    assert_eq!(wells[0]["type"], "insert");
    assert_eq!(
        wells[0]["geometry_wkb_hex"],
        "0101000000000000000000f03f0000000000000040"
    );
    assert_eq!(wells[0]["properties"]["well_name"], "Alpha");
    assert_eq!(wells[0]["properties"]["depth"], 120);
    // ptolemy's properties are JSON and the schema declares the blob column a
    // string, so a value for it would arrive as something it is not
    assert!(wells[0]["properties"].get("logo").is_none(), "{wells:?}");
}

/// A row from a table with no geometry: ptolemy's insert takes a geometry and
/// reads a null one as a deletion, so it gets an empty geometry collection.
#[test]
fn a_row_with_no_geometry_carries_an_empty_geometry_collection() {
    let extracted = extract();
    let empty = extracted
        .sidecar
        .dataset("inspections")
        .expect("inspections");
    assert!(empty.features.is_some());

    // the fixture's table is empty, so the convention is stated in the report
    // rather than shown in a row
    let inspections = only(
        &extracted.report.items,
        ItemKind::FeatureCollection,
        "inspections",
    );
    assert!(
        inspections
            .verdict
            .shortfall()
            .contains("empty geometry collection"),
        "{}",
        inspections.verdict.shortfall()
    );
}

/// An attachment names the feature it belongs to by the id the extraction
/// minted for it, which is the whole point of minting them here.
#[test]
fn an_attachment_names_a_feature_the_extraction_wrote() {
    let extracted = extract();
    assert_eq!(extracted.sidecar.attachments.len(), 1);
    let attachment = &extracted.sidecar.attachments[0];

    assert_eq!(attachment.dataset, "wells");
    assert_eq!(attachment.name, "photo.png");
    assert_eq!(attachment.content_type.as_deref(), Some("image/png"));
    assert_eq!(attachment.created_by, "operator@example.test");
    // the columns ptolemy has no field for are kept in the metadata
    assert_eq!(attachment.metadata["REL_OBJECTID"], "1");
    assert_eq!(attachment.metadata["source_table"], "wells__ATTACH");

    let wells = features(&extracted, "wells");
    assert_eq!(attachment.feature_id, wells[0]["feature_id"]);

    // the bytes are a file beside the sidecar, never inside it
    let bytes = std::fs::read(extracted.directory.join(&attachment.file)).expect("the blob");
    assert_eq!(bytes, [0x89, 0x50, 0x4e, 0x47]);
    let sidecar = std::fs::read_to_string(extracted.directory.join(SIDECAR_FILE)).expect("read");
    assert!(!sidecar.contains("iVBOR"), "the blob is not in the sidecar");
}

/// A blob table nothing points at is skipped and said to be skipped. Attaching
/// it to whatever the name suggests would put files on the wrong features,
/// which is worse than not carrying them.
#[test]
fn an_orphan_attachment_table_is_skipped_with_the_reason() {
    let extracted = extract();
    assert!(
        extracted
            .sidecar
            .attachments
            .iter()
            .all(|held| held.file.contains("wells__ATTACH")),
        "{:?}",
        extracted.sidecar.attachments
    );

    let orphan = entry(&extracted, "pads__ATTACH");
    let Action::Skipped { reason } = &orphan.action else {
        panic!("nothing says which class these belong to: {orphan:?}");
    };
    assert!(reason.contains("will not guess"), "{reason}");
    assert!(orphan.destination.is_none(), "{orphan:?}");
}

// ─── Getting to the one reference ptolemy stores ────────────────────

/// The longitude and latitude out of a point's hex WKB, little endian.
fn point(hex: &str) -> (f64, f64) {
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
        .collect();
    assert_eq!(bytes[0], 1, "little endian");
    assert_eq!(bytes[1], 1, "a point");
    let at =
        |start: usize| f64::from_le_bytes(bytes[start..start + 8].try_into().expect("8 bytes"));
    (at(5), at(13))
}

/// The case that was catastrophic and had no cover: a projected class.
///
/// ptolemy reads every geometry it is committed as EPSG:4326, so a class in
/// metres sent unchanged has its eastings read as degrees of longitude, which
/// is not an error of a metre or two but a coordinate with no meaning. The
/// fixture's `plots` is NAD83 / UTM zone 19N and its one point sits on the
/// zone's central meridian, 500000 easting, which is 69 degrees west.
#[test]
fn a_projected_class_reaches_ptolemy_in_degrees_and_not_in_metres() {
    let extracted = extract();
    let plots = features(&extracted, "plots");
    assert_eq!(plots.len(), 1);

    let (longitude, latitude) = point(
        plots[0]["geometry_wkb_hex"]
            .as_str()
            .expect("a hex geometry"),
    );

    // the easting and northing as they stand would be these, and they are what
    // used to be sent
    assert!(
        !(400_000.0..600_000.0).contains(&longitude),
        "the easting was committed as a longitude: {longitude}"
    );
    assert!((-69.01..-68.99).contains(&longitude), "{longitude}");
    assert!((46.4..46.6).contains(&latitude), "{latitude}");

    // and the GeoPackage keeps the class as it was, so the two outputs differ
    // on purpose
    let (easting, northing) = serialised(|| {
        use gdal::vector::LayerAccess;
        let written = gdal::Dataset::open(&extracted.geopackage).expect("the GeoPackage opens");
        let mut layer = written.layer_by_name("plots").expect("plots");
        let feature = layer.features().next().expect("one plot");
        let geometry = feature.geometry().expect("a point").get_point(0);
        (geometry.0, geometry.1)
    });
    assert_eq!(easting, 500_000.0);
    assert_eq!(northing, 5_150_000.0);
}

/// The report and the log have to say the two outputs differ, because a reader
/// who takes the GeoPackage and the ptolemy dataset for copies of each other
/// is wrong about both.
#[test]
fn the_log_says_which_output_holds_which_coordinates() {
    let extracted = extract();

    let plots = only(
        &extracted.report.items,
        ItemKind::FeatureCollection,
        "plots",
    );
    let shortfall = plots.verdict.shortfall();
    assert!(shortfall.contains("EPSG:26919"), "{shortfall}");
    assert!(shortfall.contains("transformed out of"), "{shortfall}");

    let written = extracted
        .sidecar
        .log
        .entries
        .iter()
        .find(|entry| entry.destination.as_deref() == Some("features/plots.ndjson"))
        .expect("the feature file is logged");
    let Action::CarriedWithLoss { losses } = &written.action else {
        panic!("a transformation is a loss: {written:?}");
    };
    assert!(
        losses
            .iter()
            .any(|loss| loss.contains("features.gpkg beside it keeps the class in")),
        "{losses:?}"
    );

    // a class already in 4326 is transformed out of nothing and says nothing
    let wells = only(
        &extracted.report.items,
        ItemKind::FeatureCollection,
        "wells",
    );
    assert!(
        !wells.verdict.shortfall().contains("transformed out of"),
        "{}",
        wells.verdict.shortfall()
    );
}

/// A class with no spatial reference cannot be transformed, and unknown
/// coordinates committed as degrees are worse than none: it is skipped, and
/// both the report and the log say so.
#[test]
fn a_class_with_no_spatial_reference_is_not_sent_at_all() {
    let extracted = extract();
    let stray = extracted
        .sidecar
        .dataset("stray_points")
        .expect("stray_points is still a dataset");
    assert!(stray.features.is_none(), "{:?}", stray.features);
    // the dataset, its schema and its layer are all still there: only the
    // features are refused
    assert_eq!(stray.layer.as_deref(), Some("stray_points"));
    assert_eq!(stray.schema.fields.len(), 1);

    let item = only(
        &extracted.report.items,
        ItemKind::FeatureCollection,
        "stray_points",
    );
    assert!(
        item.verdict
            .shortfall()
            .contains("names no spatial reference"),
        "{}",
        item.verdict.shortfall()
    );

    let skipped = extracted
        .sidecar
        .log
        .entries
        .iter()
        .find(|entry| entry.location == "stray_points" && entry.detail == "0 features")
        .expect("the refusal is logged");
    let Action::Skipped { reason } = &skipped.action else {
        panic!("nothing was written: {skipped:?}");
    };
    assert!(reason.contains("nothing to transform out of"), "{reason}");
    assert!(skipped.destination.is_none(), "{skipped:?}");
}

/// Every dataset says 4326, because that is what its geometry is once it gets
/// there. Saying the source's code instead would have the dataset describe
/// coordinates it does not hold.
#[test]
fn every_dataset_declares_the_reference_its_geometry_arrives_in() {
    let extracted = extract();
    for plan in &extracted.sidecar.datasets {
        assert_eq!(plan.dataset.srid, 4326, "{}", plan.source_table);
    }
}
