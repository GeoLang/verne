//! A map service, which is the same layer and query contract with a tree over
//! it: group layers, raster layers, and versioning stated per layer rather than
//! once at the root. Every response here is the shape sampleserver6 answers
//! with, explicit nulls included, because those nulls are what a MapServer
//! sends where a FeatureServer leaves the key out.

mod common;

use common::Fake;
use serde_json::json;
use verne_arcgis::ArcgisSource;
use verne_core::{Action, Item, ItemKind, NewFeature, Outcome, Source, Verdict};

const MAP_ROOT: &str = "https://example.invalid/arcgis/rest/services/Fixture/MapServer";

const OPERATOR: &str = "verne-arcgis test";

fn param<'a>(params: &'a [(&str, String)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(held, _)| *held == name)
        .map(|(_, value)| value.as_str())
}

/// The root: three layers and no tables, and no `hasVersionedData` at all,
/// which is why a map service has to be asked layer by layer.
fn map_root() -> serde_json::Value {
    json!({
        "mapName": "Fixture",
        "layers": [
            { "id": 0, "name": "Grouped" },
            { "id": 1, "name": "Roads" },
            { "id": 2, "name": "Hillshade" }
        ],
        "tables": []
    })
}

/// The group layer. The nulls are the fixture: `fields` and `geometryType` come
/// back as explicit nulls, which serde's `default` alone does not cover.
fn group_layer() -> serde_json::Value {
    json!({
        "id": 0,
        "name": "Grouped",
        "type": "Group Layer",
        "geometryType": null,
        "fields": null,
        "subLayers": [{ "id": 1, "name": "Roads" }],
        "parentLayer": null,
        "subtypeField": null,
        "defaultSubtypeCode": null
    })
}

/// The one layer with features. `objectIdField` is null, so the object id has
/// to come from the field declared as one, and the data behind it is versioned.
fn roads_layer() -> serde_json::Value {
    json!({
        "id": 1,
        "name": "Roads",
        "type": "Feature Layer",
        "geometryType": "esriGeometryPolyline",
        "parentLayer": { "id": 0, "name": "Grouped" },
        "isDataVersioned": true,
        "objectIdField": null,
        "fields": [
            { "name": "OBJECTID", "type": "esriFieldTypeOID" },
            { "name": "name", "type": "esriFieldTypeString" }
        ],
        "extent": { "spatialReference": { "wkid": 4269, "latestWkid": 4269 } },
        "maxRecordCount": 1000,
        "advancedQueryCapabilities": { "supportsPagination": true }
    })
}

fn raster_layer() -> serde_json::Value {
    json!({
        "id": 2,
        "name": "Hillshade",
        "type": "Raster Layer",
        "geometryType": null,
        "fields": null
    })
}

/// What a map service answers when something asks a group or raster layer for
/// rows: an error object under HTTP 200. The count is allowed to fail, so this
/// is a layer with no count rather than a failed open.
fn no_rows(_params: &[(&str, String)]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "error": { "code": 400, "message": "Invalid or missing input parameters." }
    }))
    .expect("the error serialises")
}

/// The feature layer's `/query`: the count, the one page, and the second
/// untransformed pass over the page's object ids.
fn roads_pages(params: &[(&str, String)]) -> Vec<u8> {
    if param(params, "returnCountOnly").is_some() {
        return serde_json::to_vec(&json!({ "count": 2 })).expect("the count serialises");
    }
    // the second pass: the same rows by object id, in the layer's own
    // reference, so no outSR, and the pairing key is the fallback object id
    if let Some(oids) = param(params, "objectIds") {
        assert_eq!(param(params, "outSR"), None, "{params:?}");
        assert_eq!(param(params, "outFields"), Some("OBJECTID"), "{params:?}");
        assert_eq!(param(params, "returnGeometry"), Some("true"), "{params:?}");
        let natives: Vec<serde_json::Value> = oids
            .split(',')
            .map(|oid| {
                let oid: i64 = oid.parse().expect("an object id");
                json!({
                    "attributes": { "OBJECTID": oid },
                    "geometry": { "paths": [[[-71.0, 42.0], [-71.1, 42.1]]] }
                })
            })
            .collect();
        return serde_json::to_vec(&json!({
            "objectIdFieldName": "OBJECTID",
            "features": natives
        }))
        .expect("the native page serialises");
    }
    assert_eq!(param(params, "where"), Some("1=1"), "{params:?}");
    assert_eq!(param(params, "outFields"), Some("*"), "{params:?}");
    assert_eq!(param(params, "outSR"), Some("4326"), "{params:?}");
    // the layer names no objectIdField, so the ordering key is the field
    // declared as the object id
    assert_eq!(
        param(params, "orderByFields"),
        Some("OBJECTID"),
        "{params:?}"
    );
    assert_eq!(param(params, "resultOffset"), Some("0"), "{params:?}");
    assert_eq!(
        param(params, "resultRecordCount"),
        Some("1000"),
        "{params:?}"
    );
    let page = json!({
        "objectIdFieldName": "OBJECTID",
        "features": [
            {
                "attributes": { "OBJECTID": 1, "name": "Main" },
                "geometry": { "paths": [[[-71.0, 42.0], [-71.1, 42.1]]] }
            },
            {
                "attributes": { "OBJECTID": 2, "name": "Elm" },
                "geometry": { "paths": [[[-71.2, 42.2], [-71.3, 42.3]]] }
            }
        ]
    });
    serde_json::to_vec(&page).expect("the page serialises")
}

/// The count runs for every layer the service listed, group and raster
/// included, so both of those routes answer the refusal a map service gives
/// rather than being left unrouted.
fn fake() -> Fake {
    Fake::at(MAP_ROOT)
        .json("", map_root())
        .json("/0", group_layer())
        .json("/1", roads_layer())
        .json("/2", raster_layer())
        .answering("/0/query", no_rows)
        .answering("/1/query", roads_pages)
        .answering("/2/query", no_rows)
}

fn open() -> ArcgisSource {
    ArcgisSource::open_with(Box::new(fake()), MAP_ROOT).expect("the map service opens")
}

/// Every row goes through here, so the invariants hold for all of them.
fn inventory() -> Vec<Item> {
    let items = open().inventory().expect("the map service inventories");
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
            Verdict::Unsupported { reason } | Verdict::NotApplicable { reason } => {
                assert!(!reason.is_empty(), "{} gives no reason", item.location);
                assert_eq!(item.verdict.target(), None);
            }
            Verdict::Faithful { .. } => {}
        }
    }
    items
}

fn only(items: &[Item], kind: ItemKind) -> &Item {
    let mut matching = items.iter().filter(|item| item.kind == kind);
    let found = matching
        .next()
        .unwrap_or_else(|| panic!("no {kind} row in {items:#?}"));
    assert!(matching.next().is_none(), "more than one {kind} row");
    found
}

/// The tempdir comes back with the extraction: dropping it would take the files
/// the sidecar names with it.
fn extract() -> (verne_arcgis::Extraction, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("tempdir");
    let extraction = open()
        .extract(directory.path(), OPERATOR)
        .expect("the map service extracts");
    (extraction, directory)
}

#[test]
fn a_group_layer_is_a_hierarchy_row_naming_its_members_and_no_dataset() {
    let items = inventory();
    let group = only(&items, ItemKind::Hierarchy);
    assert_eq!(group.location, "Grouped");
    assert_eq!(group.detail, "group layer holding 1 member: Roads");
    assert_eq!(group.verdict.outcome(), Outcome::Approximated);
    assert!(
        group
            .verdict
            .shortfall()
            .contains("no container above a dataset"),
        "{}",
        group.verdict.shortfall()
    );

    // the grouping is structure, and ptolemy has nothing to create for it
    let (extraction, _directory) = extract();
    assert!(extraction.sidecar.dataset("Grouped").is_none());
    let entry = extraction
        .sidecar
        .log
        .entries
        .iter()
        .find(|entry| entry.kind == ItemKind::Hierarchy)
        .unwrap_or_else(|| panic!("no hierarchy entry: {:#?}", extraction.sidecar.log.entries));
    let Action::Skipped { reason } = &entry.action else {
        panic!("the grouping was carried: {entry:#?}");
    };
    assert!(
        reason.contains("each member that holds features became its own dataset"),
        "{reason}"
    );
}

#[test]
fn a_raster_layer_is_a_raster_overlay_row_to_terrano_and_no_dataset() {
    let items = inventory();
    let raster = only(&items, ItemKind::RasterOverlay);
    assert_eq!(raster.location, "Hillshade");
    assert_eq!(raster.detail, "raster layer");
    assert_eq!(raster.verdict.outcome(), Outcome::Approximated);
    assert_eq!(
        raster.verdict.target().map(|target| target.component()),
        Some("terrano")
    );
    assert!(
        raster.verdict.shortfall().contains("unverified"),
        "{}",
        raster.verdict.shortfall()
    );

    let (extraction, _directory) = extract();
    assert!(extraction.sidecar.dataset("Hillshade").is_none());
    let entry = extraction
        .sidecar
        .log
        .entries
        .iter()
        .find(|entry| entry.kind == ItemKind::RasterOverlay)
        .unwrap_or_else(|| panic!("no raster entry: {:#?}", extraction.sidecar.log.entries));
    let Action::Skipped { reason } = &entry.action else {
        panic!("the raster was carried: {entry:#?}");
    };
    assert!(
        reason.contains("does not fetch or open the raster"),
        "{reason}"
    );
}

#[test]
fn versioned_data_behind_one_layer_makes_the_versioning_row_unsupported() {
    let items = inventory();
    let versioning = only(&items, ItemKind::Temporal);
    assert_eq!(versioning.location, "service root");
    // the root said nothing about versioning, so this can only have come from
    // the layer's own isDataVersioned
    assert_eq!(
        versioning.detail,
        "versioning and archiving, versioned data behind Roads"
    );
    assert_eq!(versioning.verdict.outcome(), Outcome::Unsupported);
    assert!(
        versioning.verdict.shortfall().contains("geogit"),
        "{}",
        versioning.verdict.shortfall()
    );
}

#[test]
fn the_object_id_field_falls_back_to_the_field_declared_as_one() {
    // the page and native handlers assert orderByFields and the pairing field,
    // both of which are the fallback: a layer whose objectIdField is null
    let (_extraction, directory) = extract();
    let written = features(directory.path().join("features/Roads.ndjson"));
    assert_eq!(written.len(), 2);
    // the object id was found, so the untransformed original could be paired
    // with its row and rides along
    assert_eq!(written[0].native_srid, Some(4269));
    assert!(
        written[0]
            .native_geometry_wkb_hex
            .as_deref()
            .is_some_and(|hex| hex.starts_with("0105000000")),
        "{:?}",
        written[0].native_geometry_wkb_hex
    );
}

#[test]
fn only_the_feature_layer_becomes_a_dataset() {
    let (extraction, directory) = extract();
    assert_eq!(extraction.sidecar.datasets.len(), 1);
    let roads = extraction.sidecar.dataset("Roads").expect("the road layer");
    assert_eq!(roads.dataset.srid, 4326);
    // a polyline layer is declared multilinestring, which is how every feature
    // is encoded
    assert_eq!(roads.dataset.geometry_type, "multilinestring");
    assert_eq!(roads.features.as_deref(), Some("features/Roads.ndjson"));

    let written = features(directory.path().join("features/Roads.ndjson"));
    assert_eq!(written.len(), 2);
    assert!(
        written[0].geometry_wkb_hex.starts_with("0105000000"),
        "{}",
        written[0].geometry_wkb_hex
    );
    assert_eq!(written[0].properties["name"], "Main");
    assert_eq!(written[1].properties["name"], "Elm");
}

#[test]
fn no_features_are_fetched_from_a_group_or_raster_layer() {
    let fake = fake();
    let calls = fake.calls();
    let directory = tempfile::tempdir().expect("tempdir");
    let source = ArcgisSource::open_with(Box::new(fake), MAP_ROOT).expect("the map service opens");
    source
        .extract(directory.path(), OPERATOR)
        .expect("the map service extracts");

    // the count runs over every layer the service listed, and nothing else may
    // reach one that holds no rows
    for call in calls.borrow().iter() {
        if call.route == "/0/query" || call.route == "/2/query" {
            assert_eq!(
                call.param("returnCountOnly"),
                Some("true"),
                "{} was asked for rows: {:?}",
                call.route,
                call.params
            );
        }
    }
    assert!(
        !directory.path().join("features/Grouped.ndjson").exists(),
        "the group layer got a feature file"
    );
    assert!(
        !directory.path().join("features/Hillshade.ndjson").exists(),
        "the raster layer got a feature file"
    );
}

#[test]
fn describe_counts_the_group_and_the_raster_apart_from_the_layers() {
    let description = open().describe();
    assert_eq!(description.format, "ArcGIS Map Service");
    assert_eq!(description.location, MAP_ROOT);
    assert_eq!(
        description.detail.as_deref(),
        Some("1 layer, 0 tables, 1 group layer, 1 raster layer")
    );
}

fn features(path: std::path::PathBuf) -> Vec<NewFeature> {
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{line}: {error}")))
        .collect()
}
