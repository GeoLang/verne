//! The sidecar model and the extraction log, with no source and no ptolemy.
//! These run in both feature sets: nothing here needs GDAL.

use std::collections::BTreeMap;

use verne_core::sidecar::{Action, ExtractionLog, LogEntry};
use verne_core::{
    AttachmentOp, DatasetPlan, DeleteAttachment, DeleteFeature, FeatureOp, Item, ItemKind, Losses,
    NewAttachment, NewDataset, NewDomain, NewFeature, NewField, NewRelationship, NewSchema,
    NewSubtype, Sidecar, SourceDescription, Target, UpdateFeature, Verdict,
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
            Losses::one("origin_primary_key stays at its default").and("no rules are carried"),
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
    assert!(markdown.contains("no rules are carried"), "{markdown}");
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
        incremental: false,
        geopackage: Some("features.gpkg".into()),
        datasets: vec![DatasetPlan {
            source_table: "wells".into(),
            layer: Some("wells".into()),
            features: Some("features/wells.ndjson".into()),
            object_id_field: None,
            drawing_info: Some(serde_json::json!({ "renderer": { "type": "simple" } })),
            dataset: NewDataset {
                name: "wells".into(),
                srid: 4326,
                geometry_type: "point".into(),
                created_by: "operator@example.test".into(),
            },
            schema: NewSchema {
                fields: vec![
                    NewField {
                        name: "well_name".into(),
                        field_type: "string".into(),
                        required: false,
                        alias: Some("Well name".into()),
                    },
                    NewField {
                        name: "depth".into(),
                        field_type: "integer".into(),
                        required: true,
                        alias: None,
                    },
                ],
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
            is_composite: true,
        }],
        attachments: vec![AttachmentOp::Add(NewAttachment {
            dataset: "wells".into(),
            feature_id: "019fb3fc-e521-7d70-80d3-3ee920a0e0d7".into(),
            name: "photo.png".into(),
            content_type: Some("image/png".into()),
            file: "attachments/wells__ATTACH/0-photo.png".into(),
            metadata: serde_json::json!({ "REL_OBJECTID": "1" }),
            created_by: "operator@example.test".into(),
            global_id: None,
        })],
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

#[test]
fn a_field_serialises_as_ptolemys_field_def() {
    let field = NewField {
        name: "constructionmaterial".into(),
        field_type: "string".into(),
        required: false,
        alias: Some("Construction Material".into()),
    };
    let json = serde_json::to_value(&field).expect("serialises");

    assert_eq!(json["name"], "constructionmaterial");
    assert_eq!(json["field_type"], "string");
    assert_eq!(json["required"], false);
    assert_eq!(json["alias"], "Construction Material");
}

/// ptolemy skips the key when there is no alias, and a field with none must not
/// gain an empty one on the way.
#[test]
fn a_field_without_an_alias_carries_no_alias_key() {
    let field = NewField {
        name: "plain".into(),
        field_type: "integer".into(),
        required: true,
        alias: None,
    };
    let json = serde_json::to_value(&field).expect("serialises");

    assert!(json.get("alias").is_none(), "{json}");
    assert_eq!(json["required"], true);
}

/// The schema is posted as it stands, so it has to be the request body and not
/// a bare list of fields.
#[test]
fn a_schema_serialises_as_the_request_body_ptolemy_takes() {
    let schema = NewSchema {
        fields: vec![NewField {
            name: "depth".into(),
            field_type: "integer".into(),
            required: false,
            alias: None,
        }],
    };
    let json = serde_json::to_value(&schema).expect("serialises");

    assert!(json["fields"].is_array(), "{json}");
    assert_eq!(json["fields"][0]["name"], "depth");
    assert!(NewSchema { fields: Vec::new() }.is_empty());
}

/// A sidecar written before the drawing info was carried has no key for it, and
/// absent is how a sidecar says the source said nothing about drawing. It has to
/// load as it did: the extraction is on disk somewhere, and a style verne never
/// read is no reason to refuse the features.
#[test]
fn an_old_shape_sidecar_reads_as_one_with_no_drawing_info() {
    let mut json = serde_json::to_value(a_sidecar()).expect("serialises");
    let plan = json["datasets"][0].as_object_mut().expect("an object");
    assert!(plan.contains_key("drawing_info"), "{plan:#?}");
    plan.remove("drawing_info");

    let read = Sidecar::from_json(&json.to_string()).expect("an old sidecar still loads");
    let wells = read.dataset("wells").expect("the plan");
    assert_eq!(wells.drawing_info, None);
    assert_eq!(wells.schema.fields.len(), 2);

    // and one that carries it round-trips as the document it was handed, which
    // is the whole point of holding it as raw JSON
    let carried = a_sidecar();
    let back = Sidecar::from_json(&carried.to_json()).expect("parses what it wrote");
    assert_eq!(
        back.dataset("wells").expect("the plan").drawing_info,
        Some(serde_json::json!({ "renderer": { "type": "simple" } }))
    );
}

/// A sidecar written before schemas existed would load with an empty one and
/// drop every alias in silence, so it has to fail to parse instead.
#[test]
fn a_sidecar_without_a_schema_is_refused() {
    let mut json = serde_json::to_value(a_sidecar()).expect("serialises");
    json["datasets"][0]
        .as_object_mut()
        .expect("an object")
        .remove("schema");

    let refused = Sidecar::from_json(&json.to_string()).expect_err("a missing schema is an error");
    assert!(refused.to_string().contains("schema"), "{refused}");
}

/// A feature line is a whole `insert` operation of ptolemy's commit route, tag
/// included, so a commit body is these lines in an array and the loader
/// rewrites nothing.
#[test]
fn a_feature_serialises_as_ptolemys_insert_operation() {
    let feature = NewFeature {
        feature_id: "019fb3fc-e521-7d70-80d3-3ee920a0e0d7".into(),
        geometry_wkb_hex: "0101000000000000000000f03f0000000000000040".into(),
        properties: serde_json::json!({ "depth": 120 })
            .as_object()
            .expect("an object")
            .clone(),
        native_geometry_wkb_hex: None,
        native_srid: None,
        native_crs_wkt: None,
    };

    let json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&feature).expect("serialises")).expect("json");

    assert_eq!(json["type"], "insert");
    assert_eq!(json["feature_id"], "019fb3fc-e521-7d70-80d3-3ee920a0e0d7");
    assert_eq!(json["properties"]["depth"], 120);
    // an untransformed feature must not put the keys on the wire at all:
    // ptolemy defaults them, and null and absent should read the same
    let object = json.as_object().expect("an object");
    assert!(!object.contains_key("native_geometry_wkb_hex"));
    assert!(!object.contains_key("native_srid"));
    let read: NewFeature = serde_json::from_value(json).expect("a line reads back");
    assert_eq!(read, feature);
}

/// A transformed feature carries its original beside the working copy, in the
/// field names ptolemy's commit reads.
#[test]
fn a_transformed_feature_carries_its_original_and_code() {
    let feature = NewFeature {
        feature_id: "019fb3fc-e521-7d70-80d3-3ee920a0e0d7".into(),
        geometry_wkb_hex: "0101000000000000000000f03f0000000000000040".into(),
        properties: serde_json::Map::new(),
        native_geometry_wkb_hex: Some("0101000000adfb5e7cc4841f41e92631c9c0ba5141".into()),
        native_srid: Some(26919),
        native_crs_wkt: None,
    };

    let json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&feature).expect("serialises")).expect("json");

    assert_eq!(
        json["native_geometry_wkb_hex"],
        "0101000000adfb5e7cc4841f41e92631c9c0ba5141"
    );
    assert_eq!(json["native_srid"], 26919);
    let read: NewFeature = serde_json::from_value(json).expect("a line reads back");
    assert_eq!(read, feature);
}

/// An update line is a whole `update` operation of ptolemy's commit route,
/// keyed on the id the previous extraction minted.
#[test]
fn an_update_serialises_as_ptolemys_update_operation() {
    let update = UpdateFeature {
        feature_id: "019fb3fc-e521-7d70-80d3-3ee920a0e0d7".into(),
        geometry_wkb_hex: Some("0101000000000000000000f03f0000000000000040".into()),
        properties: serde_json::json!({ "depth": 121 }).as_object().cloned(),
        native_geometry_wkb_hex: None,
        native_srid: None,
        native_crs_wkt: None,
    };
    let json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&update).expect("serialises")).expect("json");

    assert_eq!(json["type"], "update");
    assert_eq!(json["feature_id"], "019fb3fc-e521-7d70-80d3-3ee920a0e0d7");
    assert_eq!(json["properties"]["depth"], 121);
    // ptolemy defaults an absent original to "no original"; the keys must not
    // be on the wire as nulls
    let object = json.as_object().expect("an object");
    assert!(!object.contains_key("native_geometry_wkb_hex"));
    assert!(!object.contains_key("native_srid"));
}

#[test]
fn a_delete_serialises_as_ptolemys_delete_operation() {
    let delete = DeleteFeature {
        feature_id: "019fb3fc-e521-7d70-80d3-3ee920a0e0d7".into(),
    };
    let json: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&delete).expect("serialises")).expect("json");
    assert_eq!(json["type"], "delete");
    assert_eq!(json["feature_id"], "019fb3fc-e521-7d70-80d3-3ee920a0e0d7");
}

/// An attachment operation reads back as what its tag says, the same way a
/// feature line does.
#[test]
fn an_attachment_op_reads_back_by_its_tag() {
    let blob = NewAttachment {
        dataset: "wells".into(),
        feature_id: "019fb3fc-e521-7d70-80d3-3ee920a0e0d7".into(),
        name: "photo.png".into(),
        content_type: Some("image/png".into()),
        file: "attachments/Wells/0-photo.png".into(),
        metadata: serde_json::json!({}),
        created_by: "operator".into(),
        global_id: Some("{6B5A2E31-0F4E-4F79-9E5C-3C1E9B7A0011}".into()),
    };
    let ops = [
        AttachmentOp::Add(blob.clone()),
        AttachmentOp::Update(blob),
        AttachmentOp::Delete(DeleteAttachment {
            dataset: "wells".into(),
            feature_id: "019fb3fc-e521-7d70-80d3-3ee920a0e0d7".into(),
            name: "photo.png".into(),
            global_id: Some("{6B5A2E31-0F4E-4F79-9E5C-3C1E9B7A0011}".into()),
        }),
    ];
    for op in &ops {
        let line = serde_json::to_string(op).expect("serialises");
        let read: AttachmentOp = serde_json::from_str(&line).expect("an operation reads back");
        assert_eq!(&read, op, "{line}");
    }
    // the accessors are how the loader finds the branch and the copy to match,
    // and they have to answer for all three
    for op in &ops {
        assert_eq!(op.dataset(), "wells");
        assert_eq!(op.name(), "photo.png");
        assert_eq!(op.feature_id(), "019fb3fc-e521-7d70-80d3-3ee920a0e0d7");
        assert!(op.global_id().is_some());
    }
}

/// A sidecar written before an attachment could change carries no operation tag
/// and no global id, and every attachment in one is an upload. It has to load as
/// it did, because the extraction that wrote it is on disk somewhere and a
/// pairing verne cannot do is not a reason to refuse the blob.
#[test]
fn an_old_shape_sidecar_reads_its_attachments_as_uploads() {
    let mut json = serde_json::to_value(a_sidecar()).expect("serialises");
    let attachment = json["attachments"][0].as_object_mut().expect("an object");
    attachment.remove("op");
    attachment.remove("global_id");
    assert!(!attachment.contains_key("op"), "{attachment:#?}");

    let read = Sidecar::from_json(&json.to_string()).expect("an old sidecar still loads");
    let [AttachmentOp::Add(held)] = read.attachments.as_slice() else {
        panic!("{:#?}", read.attachments);
    };
    assert_eq!(held.name, "photo.png");
    assert_eq!(held.file, "attachments/wells__ATTACH/0-photo.png");
    // absent means unpairable, which is the truth about it
    assert_eq!(held.global_id, None);
}

/// A feature file line reads back as the operation its tag says, so a delete
/// carrying only a feature id cannot be misread as a bare update.
#[test]
fn a_feature_op_line_reads_back_by_its_tag() {
    let ops = [
        FeatureOp::Insert(NewFeature {
            feature_id: "a".into(),
            geometry_wkb_hex: "010700000000000000".into(),
            properties: serde_json::Map::new(),
            native_geometry_wkb_hex: None,
            native_srid: None,
            native_crs_wkt: None,
        }),
        FeatureOp::Update(UpdateFeature {
            feature_id: "b".into(),
            geometry_wkb_hex: None,
            properties: None,
            native_geometry_wkb_hex: None,
            native_srid: None,
            native_crs_wkt: None,
        }),
        FeatureOp::Delete(DeleteFeature {
            feature_id: "c".into(),
        }),
    ];
    for op in &ops {
        let line = serde_json::to_string(op).expect("serialises");
        let read: FeatureOp = serde_json::from_str(&line).expect("a line reads back");
        assert_eq!(&read, op, "{line}");
    }
}
