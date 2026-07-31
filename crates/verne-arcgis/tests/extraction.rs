//! What an extraction of the canned service writes to disk: the feature files,
//! the attachment blobs and the sidecar ptolemy is loaded from.

mod common;

use std::path::Path;

use common::{Fake, ROOT, logs_table, service_root, wells_layer};
use serde_json::json;
use verne_arcgis::{ArcgisSource, Extraction};
use verne_core::{Action, ItemKind, NewFeature};

const OPERATOR: &str = "verne-arcgis test";

/// The bytes the one attachment carries, a PNG signature and nothing more.
const BLOB: &[u8] = &[0x89, 0x50, 0x4e, 0x47];

/// A Date attribute as the service sends it, epoch milliseconds.
const DRILLED: i64 = 1743630690000;

fn param<'a>(params: &'a [(&str, String)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(held, _)| *held == name)
        .map(|(_, value)| value.as_str())
}

/// The point layer's `/query`: the count, then two pages, the second one
/// without the flag. Every page asserts what it was asked for, so a query the
/// adapter builds wrongly fails here rather than further down.
fn wells_pages(params: &[(&str, String)]) -> Vec<u8> {
    if param(params, "returnCountOnly").is_some() {
        return serde_json::to_vec(&json!({ "count": 3 })).expect("the count serialises");
    }
    // the second pass: the page's rows again by object id, untransformed, so
    // no outSR, and only the pairing key comes back as an attribute
    if let Some(oids) = param(params, "objectIds") {
        assert_eq!(param(params, "outSR"), None, "{params:?}");
        assert_eq!(param(params, "outFields"), Some("objectid"), "{params:?}");
        assert_eq!(param(params, "returnGeometry"), Some("true"), "{params:?}");
        let natives: Vec<serde_json::Value> = oids
            .split(',')
            .map(|oid| {
                let oid: i64 = oid.parse().expect("an object id");
                // web mercator metres, unmistakably not degrees
                json!({
                    "attributes": { "objectid": oid },
                    "geometry": { "x": 389600.0 + oid as f64, "y": 6540000.0 }
                })
            })
            .collect();
        return serde_json::to_vec(&json!({
            "objectIdFieldName": "objectid",
            "features": natives
        }))
        .expect("the native page serialises");
    }
    assert_eq!(param(params, "where"), Some("1=1"), "{params:?}");
    assert_eq!(param(params, "outFields"), Some("*"), "{params:?}");
    assert_eq!(param(params, "returnGeometry"), Some("true"), "{params:?}");
    assert_eq!(param(params, "outSR"), Some("4326"), "{params:?}");
    // the layer declares neither, so neither may be asked for
    assert_eq!(param(params, "returnZ"), None, "{params:?}");
    assert_eq!(param(params, "returnM"), None, "{params:?}");
    assert_eq!(
        param(params, "orderByFields"),
        Some("objectid"),
        "{params:?}"
    );
    assert_eq!(param(params, "resultRecordCount"), Some("2"), "{params:?}");

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

/// The table's `/query`: one page, and no geometry asked for.
fn logs_pages(params: &[(&str, String)]) -> Vec<u8> {
    if param(params, "returnCountOnly").is_some() {
        return serde_json::to_vec(&json!({ "count": 2 })).expect("the count serialises");
    }
    assert_eq!(param(params, "returnGeometry"), Some("false"), "{params:?}");
    let page = json!({
        "objectIdFieldName": "objectid",
        "features": [
            { "attributes": { "objectid": 1, "well_id": 1, "note": "first" } },
            { "attributes": { "objectid": 2, "well_id": 1, "note": "second" } }
        ]
    });
    serde_json::to_vec(&page).expect("the page serialises")
}

/// The blob route, which takes no parameters at all: `f=json` on it would ask
/// the service to describe the attachment instead of handing it over.
fn blob(params: &[(&str, String)]) -> Vec<u8> {
    assert!(params.is_empty(), "the blob was asked for with {params:?}");
    BLOB.to_vec()
}

fn attachment_infos() -> serde_json::Value {
    json!({
        "attachmentInfos": [
            { "id": 1, "name": "pic.png", "contentType": "image/png", "size": 4 }
        ]
    })
}

/// The layer lists attachments one feature at a time.
fn fake() -> Fake {
    Fake::new()
        .json("", service_root())
        .json("/0", wells_layer())
        .json("/1", logs_table())
        .answering("/0/query", wells_pages)
        .answering("/1/query", logs_pages)
        .json("/0/1/attachments", attachment_infos())
        .json("/0/2/attachments", json!({ "attachmentInfos": [] }))
        .json("/0/3/attachments", json!({ "attachmentInfos": [] }))
        .answering("/0/1/attachments/1", blob)
}

/// The same service with `supportsQueryAttachments`, so the whole layer is
/// listed in one request. One of the groups names an object id no feature was
/// written for, which is the case that has nothing to hang an attachment off.
fn fake_with_query_attachments() -> Fake {
    let mut layer = wells_layer();
    layer["advancedQueryCapabilities"]["supportsQueryAttachments"] = json!(true);
    Fake::new()
        .json("", service_root())
        .json("/0", layer)
        .json("/1", logs_table())
        .answering("/0/query", wells_pages)
        .answering("/1/query", logs_pages)
        .json(
            "/0/queryAttachments",
            json!({
                "attachmentGroups": [
                    {
                        "parentObjectId": 1,
                        "attachmentInfos": [
                            { "id": 1, "name": "pic.png", "contentType": "image/png", "size": 4 }
                        ]
                    },
                    {
                        "parentObjectId": 99,
                        "attachmentInfos": [
                            { "id": 7, "name": "stray.png", "contentType": "image/png" }
                        ]
                    }
                ]
            }),
        )
        .answering("/0/1/attachments/1", blob)
}

/// The tempdir comes back with the extraction: dropping it would take the files
/// the sidecar names with it.
fn run(fake: Fake) -> (Extraction, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("tempdir");
    let source = ArcgisSource::open_with(Box::new(fake), ROOT).expect("the fixture opens");
    let extraction = source
        .extract(directory.path(), OPERATOR)
        .expect("the extraction runs");
    (extraction, directory)
}

fn features(directory: &Path, relative: &str) -> Vec<NewFeature> {
    let text = std::fs::read_to_string(directory.join(relative))
        .unwrap_or_else(|error| panic!("reading {relative}: {error}"));
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}")))
        .collect()
}

#[test]
fn three_features_arrive_across_two_pages() {
    let fake = fake();
    let calls = fake.calls();
    let (extraction, directory) = run(fake);

    let plan = extraction
        .sidecar
        .dataset("Wells")
        .expect("a plan for the point layer");
    assert_eq!(plan.features.as_deref(), Some("features/Wells.ndjson"));
    assert_eq!(features(directory.path(), "features/Wells.ndjson").len(), 3);

    // the pages were asked for in order and only the second was needed
    let offsets: Vec<String> = calls
        .borrow()
        .iter()
        .filter(|call| {
            call.route == "/0/query"
                && call.param("returnCountOnly").is_none()
                && call.param("objectIds").is_none()
        })
        .map(|call| call.param("resultOffset").unwrap_or("none").to_string())
        .collect();
    assert_eq!(offsets, ["0", "2"]);

    // each page was followed by its native pass, POSTed because the object id
    // list of a full page does not fit in a URL
    let native_ids: Vec<String> = calls
        .borrow()
        .iter()
        .filter(|call| call.route == "/0/query" && call.param("objectIds").is_some())
        .map(|call| {
            assert_eq!(call.method, "POST");
            call.param("objectIds").unwrap_or_default().to_string()
        })
        .collect();
    assert_eq!(native_ids, ["1,2", "3"]);
}

#[test]
fn every_line_is_an_insert_ptolemy_could_take() {
    let (_extraction, directory) = run(fake());
    let written = features(directory.path(), "features/Wells.ndjson");
    for feature in &written {
        uuid::Uuid::parse_str(&feature.feature_id).expect("the feature id is a uuid");
        // 01 little endian, 01000000 point, and no Z or M in the type code
        assert!(
            feature.geometry_wkb_hex.starts_with("0101000000"),
            "{}",
            feature.geometry_wkb_hex
        );
        // the untransformed original came back from the second pass and rides
        // on the insert as the layer's EPSG code
        let native = feature
            .native_geometry_wkb_hex
            .as_deref()
            .expect("a native original on a transformed layer");
        assert!(native.starts_with("0101000000"), "{native}");
        assert_ne!(native, feature.geometry_wkb_hex);
        assert_eq!(feature.native_srid, Some(3857));
        assert_eq!(feature.native_crs_wkt, None);
    }
    // a table row carries the empty geometry collection instead
    let rows = features(directory.path(), "features/Logs.ndjson");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].geometry_wkb_hex, "010700000000000000");
    assert_eq!(rows[0].properties["note"], "first");
}

#[test]
fn a_date_attribute_is_rewritten_as_rfc3339() {
    let (_extraction, directory) = run(fake());
    let written = features(directory.path(), "features/Wells.ndjson");
    assert_eq!(written[0].properties["drilled"], "2025-04-02T21:51:30Z");
    assert_eq!(written[0].properties["depth"], 12.5);
    assert_eq!(written[0].properties["status"], 1);
    // a null attribute is left out rather than sent as a null
    assert!(
        !written[1].properties.contains_key("drilled"),
        "{:?}",
        written[1].properties
    );
}

#[test]
fn the_datasets_declare_the_geometry_ptolemy_will_hold() {
    let (extraction, _directory) = run(fake());
    let wells = extraction
        .sidecar
        .dataset("Wells")
        .expect("the point layer");
    assert_eq!(wells.dataset.srid, 4326);
    assert_eq!(wells.dataset.geometry_type, "point");
    assert_eq!(wells.dataset.created_by, OPERATOR);
    assert_eq!(wells.source_table, "Wells");
    // no GeoPackage on this path, so no layer in one
    assert_eq!(wells.layer, None);
    assert_eq!(extraction.sidecar.geopackage, None);

    let logs = extraction.sidecar.dataset("Logs").expect("the table");
    assert_eq!(logs.dataset.geometry_type, "geometry");
}

#[test]
fn the_schema_declares_the_nearest_type_ptolemy_has() {
    let (extraction, _directory) = run(fake());
    let wells = extraction
        .sidecar
        .dataset("Wells")
        .expect("the point layer");
    let typed: Vec<(&str, &str, bool)> = wells
        .schema
        .fields
        .iter()
        .map(|field| {
            (
                field.name.as_str(),
                field.field_type.as_str(),
                field.required,
            )
        })
        .collect();
    assert_eq!(
        typed,
        [
            ("objectid", "integer", false),
            ("status", "integer", false),
            ("depth", "float", false),
            // ptolemy has nothing temporal, so a Date column is a string
            ("drilled", "string", false),
        ]
    );
    // the alias is stored on the schema, which is the whole of what happens to it
    assert_eq!(wells.schema.fields[1].alias.as_deref(), Some("Status"));
    assert_eq!(wells.schema.fields[2].alias, None);
}

#[test]
fn the_domains_hold_the_coded_values_and_the_range() {
    let (extraction, _directory) = run(fake());
    let wells = extraction
        .sidecar
        .dataset("Wells")
        .expect("the point layer");
    assert_eq!(wells.domains.len(), 2);

    let coded = &wells.domains[0];
    assert_eq!(coded.name, "StatusCodes");
    assert_eq!(coded.domain_type, "coded_value");
    // the domain rides on the status field, and that field is an integer
    assert_eq!(coded.field_type, "integer");
    assert_eq!(
        coded.coded_values,
        Some(json!([
            { "code": "1", "name": "Active" },
            { "code": "2", "name": "Plugged" }
        ]))
    );

    let range = &wells.domains[1];
    assert_eq!(range.name, "DepthRange");
    assert_eq!(range.domain_type, "range");
    assert_eq!(range.field_type, "float");
    assert_eq!(range.range_min, Some(0.0));
    assert_eq!(range.range_max, Some(5000.0));
}

#[test]
fn the_subtype_names_its_domain_and_leaves_the_id_to_the_loader() {
    let (extraction, _directory) = run(fake());
    let wells = extraction
        .sidecar
        .dataset("Wells")
        .expect("the point layer");
    assert_eq!(wells.subtypes.len(), 1);
    let active = &wells.subtypes[0];
    assert_eq!(active.code, 1);
    assert_eq!(active.name, "Active");
    assert_eq!(active.subtype_field, "status");
    assert_eq!(active.default_values["depth"], 100);
    assert_eq!(
        active.domain_assignments.get("depth").map(String::as_str),
        Some("DepthRange")
    );
}

#[test]
fn the_relationship_is_keyed_on_the_column_the_table_side_names() {
    let (extraction, _directory) = run(fake());
    assert_eq!(extraction.sidecar.relationships.len(), 1);
    let class = &extraction.sidecar.relationships[0];
    assert_eq!(class.name, "wells_logs");
    assert_eq!(class.origin_dataset, "Wells");
    assert_eq!(class.destination_dataset, "Logs");
    assert_eq!(class.cardinality, "one_to_many");
    // the destination end's keyField, not the origin's
    assert_eq!(class.origin_foreign_key, "well_id");
    // the REST layer description carries no labels
    assert!(class.forward_label.is_empty());
    assert!(class.backward_label.is_empty());
}

#[test]
fn an_attachment_is_listed_per_feature_and_written_beside_the_sidecar() {
    let fake = fake();
    let calls = fake.calls();
    let (extraction, directory) = run(fake);

    let routes: Vec<String> = calls
        .borrow()
        .iter()
        .map(|call| call.route.clone())
        .collect();
    assert!(
        routes.contains(&"/0/1/attachments".to_string()),
        "{routes:?}"
    );
    assert!(
        !routes.contains(&"/0/queryAttachments".to_string()),
        "the layer does not support queryAttachments: {routes:?}"
    );

    assert_eq!(extraction.sidecar.attachments.len(), 1);
    let attachment = &extraction.sidecar.attachments[0];
    assert_eq!(attachment.dataset, "Wells");
    assert_eq!(attachment.name, "pic.png");
    assert_eq!(attachment.content_type.as_deref(), Some("image/png"));
    assert_eq!(attachment.created_by, OPERATOR);
    assert_eq!(attachment.metadata["attachment_id"], 1);
    assert_eq!(attachment.metadata["object_id"], "1");
    assert_eq!(attachment.metadata["source_layer"], "Wells");

    // the file the loader will read, and the feature it hangs off
    let blob = directory.path().join(&attachment.file);
    assert!(
        attachment.file.starts_with("attachments/"),
        "{}",
        attachment.file
    );
    assert_eq!(std::fs::read(&blob).expect("the blob"), BLOB);
    let written = features(directory.path(), "features/Wells.ndjson");
    assert!(
        written
            .iter()
            .any(|feature| feature.feature_id == attachment.feature_id),
        "the attachment names a feature the extraction did not write"
    );
}

#[test]
fn a_layer_that_supports_it_is_listed_in_one_query_attachments_call() {
    let fake = fake_with_query_attachments();
    let calls = fake.calls();
    let (extraction, directory) = run(fake);

    let listed: Vec<String> = calls
        .borrow()
        .iter()
        .filter(|call| call.route == "/0/queryAttachments")
        .map(|call| call.param("objectIds").unwrap_or("none").to_string())
        .collect();
    assert_eq!(listed, ["1,2,3"]);

    // the stray group named object id 99, which no feature was written for, so
    // there is nothing to attach it to and it is not in the sidecar
    assert_eq!(extraction.sidecar.attachments.len(), 1);
    let attachment = &extraction.sidecar.attachments[0];
    assert_eq!(attachment.name, "pic.png");
    assert_eq!(
        std::fs::read(directory.path().join(&attachment.file)).expect("the blob"),
        BLOB
    );

    let orphan = extraction.sidecar.log.entries.iter().any(|entry| {
        entry.kind == ItemKind::EmbeddedResource
            && matches!(&entry.action, Action::CarriedWithLoss { losses } if losses
                .iter()
                .any(|loss| loss.contains("object id 99")))
    });
    assert!(
        orphan,
        "the log says nothing about object id 99: {:#?}",
        extraction.sidecar.log.entries
    );
}

#[test]
fn the_log_names_the_reprojection_and_leaves_the_renderer_behind() {
    let (extraction, _directory) = run(fake());
    let entries = &extraction.sidecar.log.entries;
    assert_eq!(extraction.sidecar.log.operator, OPERATOR);

    let reprojected = entries.iter().any(|entry| match &entry.action {
        Action::CarriedWithLoss { losses } => losses
            .iter()
            .any(|loss| loss.contains("EPSG:3857") && loss.contains("second pass")),
        _ => false,
    });
    assert!(reprojected, "no entry names the transform: {entries:#?}");

    let renderer = entries
        .iter()
        .find(|entry| entry.kind == ItemKind::Styling)
        .unwrap_or_else(|| panic!("no styling entry: {entries:#?}"));
    let Action::Skipped { reason } = &renderer.action else {
        panic!("the renderer was not left behind: {renderer:#?}");
    };
    assert!(reason.contains("jung is not among its outputs"), "{reason}");
    assert_eq!(renderer.destination, None);
}
