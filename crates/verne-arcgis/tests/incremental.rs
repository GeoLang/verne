//! What a delta extraction writes: the operations since a previous full
//! extraction of the same service, paired by object id. The fixture's second
//! state changes one feature, leaves one alone, adds one and drops one, so
//! the delta is exactly one update, one insert and one delete.

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use common::{Fake, ROOT, logs_table, service_root, wells_layer};
use serde_json::json;
use verne_arcgis::{ArcgisError, ArcgisSource, Extraction};
use verne_core::{Action, FeatureOp, ItemKind, NewFeature};

const OPERATOR: &str = "verne-arcgis test";

/// A Date attribute as the service sends it, epoch milliseconds.
const DRILLED: i64 = 1743630690000;

fn param<'a>(params: &'a [(&str, String)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(held, _)| *held == name)
        .map(|(_, value)| value.as_str())
}

/// The native second pass, shared by both states: the page's rows again by
/// object id, untransformed.
fn native_page(oids: &str) -> Vec<u8> {
    let natives: Vec<serde_json::Value> = oids
        .split(',')
        .map(|oid| {
            let oid: i64 = oid.parse().expect("an object id");
            json!({
                "attributes": { "objectid": oid },
                "geometry": { "x": 389600.0 + oid as f64, "y": 6540000.0 }
            })
        })
        .collect();
    serde_json::to_vec(&json!({ "objectIdFieldName": "objectid", "features": natives }))
        .expect("the native page serialises")
}

/// The first state of the point layer: features 1, 2 and 3.
fn wells_full(params: &[(&str, String)]) -> Vec<u8> {
    if param(params, "returnCountOnly").is_some() {
        return serde_json::to_vec(&json!({ "count": 3 })).expect("the count serialises");
    }
    if let Some(oids) = param(params, "objectIds") {
        return native_page(oids);
    }
    let page = match param(params, "resultOffset") {
        Some("0") => json!({
            "objectIdFieldName": "objectid",
            "features": [
                {
                    "attributes": { "objectid": 1, "status": 1, "depth": 12.5, "drilled": DRILLED },
                    "geometry": { "x": 3.5, "y": 50.1 }
                },
                {
                    "attributes": { "objectid": 2, "status": 2, "depth": 30.0, "drilled": null },
                    "geometry": { "x": 3.6, "y": 50.2 }
                }
            ],
            "exceededTransferLimit": true
        }),
        Some("2") => json!({
            "objectIdFieldName": "objectid",
            "features": [{
                "attributes": { "objectid": 3, "status": 1, "depth": 40.0, "drilled": DRILLED },
                "geometry": { "x": 3.7, "y": 50.3 }
            }]
        }),
        other => panic!("the extraction asked for offset {other:?}"),
    };
    serde_json::to_vec(&page).expect("the page serialises")
}

/// The second state: 1 got deeper, 2 is as it was, 3 vanished, 4 is new.
fn wells_changed(params: &[(&str, String)]) -> Vec<u8> {
    if param(params, "returnCountOnly").is_some() {
        return serde_json::to_vec(&json!({ "count": 3 })).expect("the count serialises");
    }
    if let Some(oids) = param(params, "objectIds") {
        return native_page(oids);
    }
    let page = match param(params, "resultOffset") {
        Some("0") => json!({
            "objectIdFieldName": "objectid",
            "features": [
                {
                    "attributes": { "objectid": 1, "status": 1, "depth": 13.0, "drilled": DRILLED },
                    "geometry": { "x": 3.5, "y": 50.1 }
                },
                {
                    "attributes": { "objectid": 2, "status": 2, "depth": 30.0, "drilled": null },
                    "geometry": { "x": 3.6, "y": 50.2 }
                }
            ],
            "exceededTransferLimit": true
        }),
        Some("2") => json!({
            "objectIdFieldName": "objectid",
            "features": [{
                "attributes": { "objectid": 4, "status": 1, "depth": 55.0, "drilled": null },
                "geometry": { "x": 3.8, "y": 50.4 }
            }]
        }),
        other => panic!("the extraction asked for offset {other:?}"),
    };
    serde_json::to_vec(&page).expect("the page serialises")
}

/// The table holds the same two rows in both states.
fn logs_pages(params: &[(&str, String)]) -> Vec<u8> {
    if param(params, "returnCountOnly").is_some() {
        return serde_json::to_vec(&json!({ "count": 2 })).expect("the count serialises");
    }
    let page = json!({
        "objectIdFieldName": "objectid",
        "features": [
            { "attributes": { "objectid": 1, "well_id": 1, "note": "first" } },
            { "attributes": { "objectid": 2, "well_id": 1, "note": "second" } }
        ]
    });
    serde_json::to_vec(&page).expect("the page serialises")
}

/// The first state, with nothing attached, so the full extraction carries
/// the features and an empty attachment list.
fn full_fake() -> Fake {
    Fake::new()
        .json("", service_root())
        .json("/0", wells_layer())
        .json("/1", logs_table())
        .answering("/0/query", wells_full)
        .answering("/1/query", logs_pages)
        .json("/0/1/attachments", json!({ "attachmentInfos": [] }))
        .json("/0/2/attachments", json!({ "attachmentInfos": [] }))
        .json("/0/3/attachments", json!({ "attachmentInfos": [] }))
}

/// The second state. No attachment routes on purpose: a delta that asked for
/// one would panic the fake, which is the proof none is asked for.
fn changed_fake() -> Fake {
    Fake::new()
        .json("", service_root())
        .json("/0", wells_layer())
        .json("/1", logs_table())
        .answering("/0/query", wells_changed)
        .answering("/1/query", logs_pages)
}

fn extract_full(directory: &Path) -> Extraction {
    ArcgisSource::open_with(Box::new(full_fake()), ROOT)
        .expect("the fixture opens")
        .extract(directory, OPERATOR)
        .expect("the full extraction runs")
}

fn extract_delta(directory: &Path, previous: &Path) -> Extraction {
    ArcgisSource::open_with(Box::new(changed_fake()), ROOT)
        .expect("the fixture opens")
        .extract_since(directory, OPERATOR, previous)
        .expect("the delta extraction runs")
}

fn ops(directory: &Path, relative: &str) -> Vec<FeatureOp> {
    let text = std::fs::read_to_string(directory.join(relative))
        .unwrap_or_else(|error| panic!("reading {relative}: {error}"));
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}")))
        .collect()
}

/// The feature id the full extraction minted for each object id.
fn minted(directory: &Path) -> BTreeMap<i64, String> {
    let text = std::fs::read_to_string(directory.join("features/Wells.ndjson")).expect("the file");
    text.lines()
        .map(|line| {
            let feature: NewFeature = serde_json::from_str(line).expect("an insert line");
            (
                feature.properties["objectid"].as_i64().expect("an oid"),
                feature.feature_id,
            )
        })
        .collect()
}

#[test]
fn the_delta_is_one_update_one_insert_and_one_delete() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());
    let ids = minted(full.path());
    let extraction = extract_delta(delta.path(), full.path());

    let written = ops(delta.path(), "features/Wells.ndjson");
    assert_eq!(written.len(), 3, "{written:#?}");

    let updates: Vec<_> = written
        .iter()
        .filter_map(|op| match op {
            FeatureOp::Update(update) => Some(update),
            _ => None,
        })
        .collect();
    assert_eq!(updates.len(), 1, "{written:#?}");
    // the update keeps the id the full extraction minted, which is what
    // makes it an edit of that feature rather than a second copy
    assert_eq!(updates[0].feature_id, ids[&1]);
    let properties = updates[0]
        .properties
        .as_ref()
        .expect("properties ride on an update");
    assert_eq!(properties["depth"], 13.0);
    assert!(updates[0].geometry_wkb_hex.is_some());
    // the original rides on the update too: ptolemy reads an omitted one as
    // "no original", never as inherited
    assert!(updates[0].native_geometry_wkb_hex.is_some());
    assert_eq!(updates[0].native_srid, Some(3857));

    let inserts: Vec<_> = written
        .iter()
        .filter_map(|op| match op {
            FeatureOp::Insert(insert) => Some(insert),
            _ => None,
        })
        .collect();
    assert_eq!(inserts.len(), 1, "{written:#?}");
    assert_eq!(inserts[0].properties["objectid"], 4);
    assert!(
        !ids.values().any(|id| *id == inserts[0].feature_id),
        "a new feature must get a fresh id"
    );

    let deletes: Vec<_> = written
        .iter()
        .filter_map(|op| match op {
            FeatureOp::Delete(delete) => Some(delete),
            _ => None,
        })
        .collect();
    assert_eq!(deletes.len(), 1, "{written:#?}");
    assert_eq!(deletes[0].feature_id, ids[&3]);

    assert!(extraction.sidecar.incremental);
    let plan = extraction
        .sidecar
        .dataset("Wells")
        .expect("the point layer");
    assert_eq!(plan.object_id_field.as_deref(), Some("objectid"));
    let counted = extraction.sidecar.log.entries.iter().any(|entry| {
        entry.location == "Wells" && entry.detail == "1 inserted, 1 updated, 1 deleted; 1 unchanged"
    });
    assert!(counted, "{:#?}", extraction.sidecar.log.entries);
}

#[test]
fn an_unchanged_table_is_an_empty_delta_with_its_rows_counted() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());
    let extraction = extract_delta(delta.path(), full.path());

    assert!(ops(delta.path(), "features/Logs.ndjson").is_empty());
    let counted = extraction.sidecar.log.entries.iter().any(|entry| {
        entry.location == "Logs" && entry.detail == "0 inserted, 0 updated, 0 deleted; 2 unchanged"
    });
    assert!(counted, "{:#?}", extraction.sidecar.log.entries);
}

/// The relationship classes were created when the full extraction was loaded,
/// and a local diff learns nothing about attachments: it reads the features
/// again, and the service says nothing about a blob on the way. The log says
/// where both stand.
#[test]
fn a_delta_repeats_no_relationships_and_fetches_no_attachments() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());
    let extraction = extract_delta(delta.path(), full.path());

    assert!(extraction.sidecar.relationships.is_empty());
    assert!(extraction.sidecar.attachments.is_empty());
    let entries = &extraction.sidecar.log.entries;
    let relationship = entries
        .iter()
        .find(|entry| entry.kind == ItemKind::Relationship)
        .unwrap_or_else(|| panic!("no relationship entry: {entries:#?}"));
    assert!(
        matches!(&relationship.action, Action::Skipped { reason } if reason.contains("does not repeat them")),
        "{relationship:#?}"
    );
    let attachments = entries
        .iter()
        .find(|entry| entry.kind == ItemKind::EmbeddedResource)
        .unwrap_or_else(|| panic!("no attachment entry: {entries:#?}"));
    assert!(
        matches!(&attachments.action, Action::Skipped { reason } if reason.contains("says nothing about attachments")),
        "{attachments:#?}"
    );
}

/// A delta's feature files hold only what changed, so diffing against one
/// would read every unchanged feature as vanished.
#[test]
fn a_delta_of_a_delta_is_refused() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    let again = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());
    extract_delta(delta.path(), full.path());

    let refused = ArcgisSource::open_with(Box::new(changed_fake()), ROOT)
        .expect("the fixture opens")
        .extract_since(again.path(), OPERATOR, delta.path())
        .expect_err("a delta is not a basis");
    assert!(
        matches!(refused, ArcgisError::DeltaPrevious { .. }),
        "{refused}"
    );
}

/// An extraction written before deltas existed recorded no object id field,
/// so there is nothing to pair on and the honest answer is no delta at all,
/// not a duplicate of every feature.
#[test]
fn a_previous_without_an_object_id_field_yields_no_delta() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());
    let sidecar_path = full.path().join("sidecar.json");
    let mut sidecar: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar_path).expect("the sidecar"))
            .expect("json");
    for dataset in sidecar["datasets"].as_array_mut().expect("datasets") {
        dataset
            .as_object_mut()
            .expect("an object")
            .remove("object_id_field");
    }
    std::fs::write(&sidecar_path, sidecar.to_string()).expect("rewritten");

    let extraction = extract_delta(delta.path(), full.path());
    let plan = extraction
        .sidecar
        .dataset("Wells")
        .expect("the point layer");
    assert_eq!(plan.features, None);
    assert!(!delta.path().join("features/Wells.ndjson").exists());
    let said = extraction.sidecar.log.entries.iter().any(|entry| {
        matches!(&entry.action, Action::Skipped { reason } if reason.contains("recorded no object id field"))
    });
    assert!(said, "{:#?}", extraction.sidecar.log.entries);
}
