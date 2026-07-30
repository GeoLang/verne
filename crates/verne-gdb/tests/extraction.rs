//! Extraction of a geodatabase into a sidecar and a log. No ptolemy: every
//! assertion here is about the file verne writes, not about a load.
#![cfg(feature = "gdal")]

use std::path::{Path, PathBuf};
use std::process::Command;

use verne_core::sidecar::Action;
use verne_core::{Item, Report, Sidecar, Verdict};
use verne_gdb::{GdbSource, SIDECAR_FILE};

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
    assert_eq!(names, ["wells", "pads", "well_labels", "inspections"]);

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

/// The system and attachment tables are not datasets, and the log says so
/// rather than leaving them out.
#[test]
fn the_tables_that_are_not_data_are_left_with_a_reason() {
    let extracted = extract();
    assert!(extracted.sidecar.dataset("wells__ATTACH").is_none());
    assert!(extracted.sidecar.dataset("GDB_Items").is_none());

    let attachments = entry(&extracted, "wells__ATTACH");
    let Action::Skipped { reason } = &attachments.action else {
        panic!("an attachment table is not extracted: {attachments:?}");
    };
    assert!(
        reason.contains("blobs are a slice of work of their own"),
        "{reason}"
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
