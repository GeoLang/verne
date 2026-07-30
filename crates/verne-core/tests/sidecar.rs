//! The sidecar model and the extraction log, with no source and no ptolemy.
//! These run in both feature sets: nothing here needs GDAL.

use std::collections::BTreeMap;

use verne_core::sidecar::{Action, ExtractionLog, LogEntry};
use verne_core::{
    DatasetPlan, Item, ItemKind, Losses, NewDataset, NewDomain, NewRelationship, NewSubtype,
    Sidecar, SourceDescription, Target, Verdict,
};

fn faithful(location: &str) -> Item {
    Item::new(
        location,
        ItemKind::FeatureCollection,
        "Point, 1 feature",
        Verdict::faithful(Target::Ptolemy),
    )
}

fn approximated(location: &str) -> Item {
    Item::new(
        location,
        ItemKind::Relationship,
        "wells -> inspections",
        Verdict::approximated(
            Target::Ptolemy,
            Losses::one("origin_primary_key stays at its default").and("is_composite is not set"),
        ),
    )
}

fn unsupported(location: &str) -> Item {
    Item::new(
        location,
        ItemKind::DataModel,
        "Water_Topology (DETopology)",
        Verdict::unsupported("GDAL reads no definition for this kind of item"),
    )
}

#[test]
fn a_carried_item_takes_its_losses_from_the_verdict() {
    let mut log = ExtractionLog::new("operator@example.test");
    log.carried(&faithful("pads"), "features.gpkg:pads");
    log.carried(&approximated("geodatabase root"), "sidecar relationships");

    assert_eq!(log.entries[0].action, Action::Carried);
    assert_eq!(
        log.entries[0].destination.as_deref(),
        Some("features.gpkg:pads")
    );
    let Action::CarriedWithLoss { losses } = &log.entries[1].action else {
        panic!(
            "an approximated verdict logs its losses: {:?}",
            log.entries[1]
        );
    };
    assert_eq!(losses.len(), 2);
    assert!(losses[0].contains("origin_primary_key"), "{losses:?}");
}

/// The log takes the action from the verdict, so an extraction that thinks it
/// wrote something with no home in GeoLang cannot say so.
#[test]
fn an_unsupported_item_is_logged_as_skipped_even_when_carried() {
    let mut log = ExtractionLog::new("operator");
    log.carried(&unsupported("Water_Topology"), "features.gpkg:topology");

    let Action::Skipped { reason } = &log.entries[0].action else {
        panic!("carrying an unsupported item must log it as skipped");
    };
    assert!(reason.contains("no definition"), "{reason}");
    assert_eq!(log.entries[0].destination, None);
}

/// The report already said why an unsupported thing has no home, so the log
/// repeats that rather than inventing a second account of it.
#[test]
fn a_report_reason_wins_over_the_callers_reason() {
    let mut log = ExtractionLog::new("operator");
    log.skipped(&unsupported("Water_Topology"), "out of scope for v0.3");
    log.skipped(&faithful("wells__ATTACH"), "out of scope for v0.3");

    let Action::Skipped { reason } = &log.entries[0].action else {
        panic!("skipped");
    };
    assert!(reason.contains("no definition"), "{reason}");
    let Action::Skipped { reason } = &log.entries[1].action else {
        panic!("skipped");
    };
    assert_eq!(reason, "out of scope for v0.3");
}

#[test]
fn a_conversion_loss_is_recorded_without_a_verdict() {
    let mut log = ExtractionLog::new("operator");
    log.converted(
        "wells",
        ItemKind::FeatureCollection,
        "1 feature",
        "features.gpkg:wells",
        vec!["the field alias is not written to the GeoPackage".to_string()],
    );
    log.converted(
        "pads",
        ItemKind::FeatureCollection,
        "0 features",
        "features.gpkg:pads",
        Vec::new(),
    );

    assert!(matches!(
        log.entries[0].action,
        Action::CarriedWithLoss { .. }
    ));
    assert_eq!(log.entries[1].action, Action::Carried);
}

#[test]
fn the_counts_split_the_entries_three_ways() {
    let mut log = ExtractionLog::new("operator");
    log.carried(&faithful("pads"), "features.gpkg:pads");
    log.carried(&approximated("geodatabase root"), "sidecar relationships");
    log.skipped(&unsupported("Water_Topology"), "");

    let counts = log.counts();
    assert_eq!(counts.total, 3);
    assert_eq!(counts.carried, 1);
    assert_eq!(counts.approximated, 1);
    assert_eq!(counts.skipped, 1);
    assert!(counts.sentence().contains("1 approximated"), "{counts:?}");
}

#[test]
fn the_log_renders_every_entry_as_a_markdown_row() {
    let mut log = ExtractionLog::new("operator");
    log.carried(&approximated("geodatabase root"), "sidecar relationships");
    let markdown = log.to_markdown();

    assert!(markdown.contains("Extracted by operator at"), "{markdown}");
    assert!(markdown.contains("is_composite is not set"), "{markdown}");
    // a pipe in a detail would end the cell early
    let rows = markdown.lines().filter(|l| l.starts_with("| ")).count();
    assert_eq!(rows, 2, "a header and one entry: {markdown}");
}

#[test]
fn a_coded_domain_serialises_in_the_shape_ptolemy_documents() {
    let domain = NewDomain::coded(
        "status_codes",
        "string",
        [("A", "Active"), ("P", "Plugged")],
    );
    let json = serde_json::to_value(&domain).expect("serialises");

    assert_eq!(json["domain_type"], "coded_value");
    assert_eq!(json["coded_values"][0]["code"], "A");
    assert_eq!(json["coded_values"][1]["name"], "Plugged");
    // a coded domain has no bounds, and ptolemy takes them as optional
    assert!(json.get("range_min").is_none(), "{json}");
}

#[test]
fn a_range_domain_keeps_an_open_end_open() {
    let domain = NewDomain::range("depth_range", "integer", Some(0.0), None);
    let json = serde_json::to_value(&domain).expect("serialises");

    assert_eq!(json["domain_type"], "range");
    assert_eq!(json["range_min"], 0.0);
    assert!(json.get("range_max").is_none(), "{json}");
    assert!(json.get("coded_values").is_none(), "{json}");
}

fn a_sidecar() -> Sidecar {
    let mut assignments = BTreeMap::new();
    assignments.insert("depth".to_string(), "depth_range".to_string());
    let mut defaults = serde_json::Map::new();
    defaults.insert("depth".to_string(), serde_json::json!("100"));

    let mut log = ExtractionLog::new("operator@example.test");
    log.carried(&faithful("wells"), "features.gpkg:wells");
    log.skipped(&unsupported("Water_Topology"), "");

    Sidecar {
        source: SourceDescription::new("Esri file geodatabase", "/data/wells.gdb"),
        geopackage: Some("features.gpkg".into()),
        datasets: vec![DatasetPlan {
            source_table: "wells".into(),
            layer: Some("wells".into()),
            dataset: NewDataset {
                name: "wells".into(),
                srid: 4326,
                geometry_type: "point".into(),
                created_by: "operator@example.test".into(),
            },
            domains: vec![NewDomain::coded(
                "status_codes",
                "string",
                [("A", "Active")],
            )],
            subtypes: vec![NewSubtype {
                subtype_field: "status".into(),
                name: "Active well".into(),
                code: 1,
                default_values: defaults,
                domain_assignments: assignments,
            }],
        }],
        relationships: vec![NewRelationship {
            name: "wells_inspections".into(),
            origin_dataset: "wells".into(),
            destination_dataset: "inspections".into(),
            origin_foreign_key: "well_id".into(),
            cardinality: "one_to_many".into(),
            forward_label: "has inspections".into(),
            backward_label: "inspected well".into(),
        }],
        log,
    }
}

#[test]
fn a_sidecar_round_trips_through_json() {
    let sidecar = a_sidecar();
    let back = Sidecar::from_json(&sidecar.to_json()).expect("parses what it wrote");
    assert_eq!(back, sidecar);
}

/// A subtype's domain assignments name domains and a relationship names
/// datasets, because neither has an id until the load runs. Typing them as
/// names is what stops a loader from posting them straight through.
#[test]
fn the_shapes_ptolemy_wants_as_ids_carry_names_instead() {
    let sidecar = a_sidecar();
    let json = serde_json::to_value(&sidecar).expect("serialises");

    assert_eq!(
        json["datasets"][0]["subtypes"][0]["domain_assignments"]["depth"],
        "depth_range"
    );
    assert_eq!(json["relationships"][0]["origin_dataset"], "wells");
    assert_eq!(
        json["relationships"][0]["destination_dataset"],
        "inspections"
    );
}

#[test]
fn a_dataset_plan_is_found_by_the_name_it_will_have() {
    let sidecar = a_sidecar();
    assert_eq!(
        sidecar.dataset("wells").map(|p| p.source_table.as_str()),
        Some("wells")
    );
    assert!(sidecar.dataset("inspections").is_none());
}

#[test]
fn a_log_entry_flattens_its_action_into_the_row() {
    let entry = LogEntry {
        location: "wells".into(),
        kind: ItemKind::FeatureCollection,
        detail: "Point, 1 feature".into(),
        action: Action::Skipped {
            reason: "nothing to write".into(),
        },
        destination: None,
    };
    let json = serde_json::to_value(&entry).expect("serialises");
    assert_eq!(json["action"], "skipped");
    assert_eq!(json["reason"], "nothing to write");
    assert!(json.get("destination").is_none(), "{json}");
}
