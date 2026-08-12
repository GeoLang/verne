//! Loading into a live ptolemy.
//!
//! Gated on `VERNE_PTOLEMY_URL`, so verne's CI does not cover it. A mocked
//! version would only prove the loader agrees with itself: the failure this
//! test exists to catch is a request shape drifting from ptolemy's real API,
//! and a mock built from the same assumptions cannot see that. ptolemy
//! publishes no container image and no OpenAPI spec, so there is nothing CI
//! could stand up or check against, and the test names what it needs and skips
//! when it is not there.
//!
//! ```bash
//! export VERNE_PTOLEMY_URL=http://localhost:3000
//! export VERNE_PTOLEMY_TOKEN=<a bearer token with the editor or admin role>
//! cargo test -p verne-load -- --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use verne_core::SourceDescription;
use verne_core::sidecar::{
    AttachmentOp, DatasetPlan, ExtractionLog, NewAttachment, NewDataset, NewDomain, NewFeature,
    NewField, NewRelationship, NewSchema, NewSubtype, Sidecar,
};
use verne_load::Loader;

/// POINT (1 2) as hex WKB, which is what an extraction writes.
const POINT: &str = "0101000000000000000000f03f0000000000000040";

/// The same well as its source recorded it, a projected point a transform
/// would have moved. It must come back byte for byte, not close.
const NATIVE_POINT: &str = "0101000000adfb5e7cc4841f41e92631c9c0ba5141";

/// A reference no single EPSG code names, abbreviated: ptolemy stores the
/// string as given, so nothing here depends on it resolving.
const COMPOUND_WKT: &str =
    "COMPD_CS[\"NAD83 + NAVD88 height\",GEOGCS[\"NAD83\"],VERT_CS[\"NAVD88 height\"]]";

/// An empty geometry collection: the convention for a row from a table with no
/// geometry, since ptolemy's insert takes a geometry and reads a null one as a
/// deletion.
const EMPTY: &str = "010700000000000000";

/// The bytes the attachment carries, a PNG signature and nothing more.
const BLOB: &[u8] = &[0x89, 0x50, 0x4e, 0x47];

/// Every run makes its own names: ptolemy's dataset name is unique across the
/// instance, so a second run against the same database would collide.
fn suffix() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_nanos();
    format!("{now:x}")
}

/// An extraction directory: the sidecar, the feature files it names and the
/// attachment blob. Written rather than mocked, because the loader reads them
/// off disk the way `verne load` does.
struct Extraction {
    sidecar: Sidecar,
    /// The feature the attachment hangs off, so the test can ask ptolemy for it.
    well: String,
    /// The feature whose original's reference travels as WKT.
    well_wkt: String,
    directory: tempfile::TempDir,
}

fn an_extraction(suffix: &str) -> Extraction {
    let directory = tempfile::tempdir().expect("tempdir");
    let well = uuid::Uuid::now_v7().to_string();
    let well_wkt = uuid::Uuid::now_v7().to_string();
    let inspection = uuid::Uuid::now_v7().to_string();

    write_features(
        directory.path(),
        "wells",
        &[
            NewFeature {
                feature_id: well.clone(),
                geometry_wkb_hex: POINT.into(),
                properties: serde_json::json!({ "well_name": "Alpha", "depth": 120 })
                    .as_object()
                    .expect("an object")
                    .clone(),
                native_geometry_wkb_hex: Some(NATIVE_POINT.into()),
                native_srid: Some(26919),
                native_crs_wkt: None,
            },
            NewFeature {
                feature_id: well_wkt.clone(),
                geometry_wkb_hex: POINT.into(),
                properties: serde_json::json!({ "well_name": "Beta", "depth": 80 })
                    .as_object()
                    .expect("an object")
                    .clone(),
                native_geometry_wkb_hex: Some(NATIVE_POINT.into()),
                native_srid: None,
                native_crs_wkt: Some(COMPOUND_WKT.into()),
            },
        ],
    );
    // a table with no geometry, which is the case the empty geometry
    // collection exists for
    write_features(
        directory.path(),
        "inspections",
        &[NewFeature {
            feature_id: inspection,
            geometry_wkb_hex: EMPTY.into(),
            properties: serde_json::Map::new(),
            native_geometry_wkb_hex: None,
            native_srid: None,
            native_crs_wkt: None,
        }],
    );
    std::fs::create_dir_all(directory.path().join("attachments")).expect("attachments dir");
    std::fs::write(directory.path().join("attachments/photo.png"), BLOB).expect("the blob");

    Extraction {
        sidecar: a_sidecar(suffix, &well),
        well,
        well_wkt,
        directory,
    }
}

fn write_features(directory: &Path, table: &str, features: &[NewFeature]) {
    std::fs::create_dir_all(directory.join("features")).expect("features dir");
    let lines: Vec<String> = features
        .iter()
        .map(|feature| serde_json::to_string(feature).expect("a feature serialises"))
        .collect();
    std::fs::write(
        directory.join(format!("features/{table}.ndjson")),
        lines.join("\n") + "\n",
    )
    .expect("the feature file");
}

fn a_sidecar(suffix: &str, well_feature: &str) -> Sidecar {
    let wells = format!("verne_wells_{suffix}");
    let inspections = format!("verne_inspections_{suffix}");

    let mut assignments = BTreeMap::new();
    assignments.insert("depth".to_string(), "depth_range".to_string());
    let mut defaults = serde_json::Map::new();
    defaults.insert("depth".to_string(), serde_json::json!("100"));

    Sidecar {
        source: SourceDescription::new("Esri file geodatabase", "the live load test"),
        incremental: false,
        geopackage: Some("features.gpkg".into()),
        datasets: vec![
            DatasetPlan {
                source_table: "wells".into(),
                layer: Some("wells".into()),
                features: Some("features/wells.ndjson".into()),
                object_id_field: None,
                drawing_info: None,
                dataset: NewDataset {
                    name: wells.clone(),
                    srid: 4326,
                    geometry_type: "point".into(),
                    created_by: "verne-load test".into(),
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
                            required: false,
                            alias: None,
                        },
                    ],
                },
                domains: vec![
                    NewDomain::coded(
                        "status_codes",
                        "string",
                        [("A", "Active"), ("P", "Plugged")],
                    ),
                    NewDomain::range("depth_range", "integer", Some(0.0), Some(5000.0)),
                ],
                subtypes: vec![
                    NewSubtype {
                        subtype_field: "status".into(),
                        name: "Active well".into(),
                        code: 1,
                        default_values: defaults,
                        domain_assignments: assignments,
                    },
                    NewSubtype {
                        subtype_field: "status".into(),
                        name: "Plugged well".into(),
                        code: 2,
                        default_values: serde_json::Map::new(),
                        domain_assignments: BTreeMap::new(),
                    },
                ],
            },
            DatasetPlan {
                source_table: "inspections".into(),
                layer: Some("inspections".into()),
                features: Some("features/inspections.ndjson".into()),
                object_id_field: None,
                drawing_info: None,
                dataset: NewDataset {
                    name: inspections.clone(),
                    srid: 4326,
                    geometry_type: "geometry".into(),
                    created_by: "verne-load test".into(),
                },
                // no fields, so no schema is sent for it
                schema: NewSchema { fields: Vec::new() },
                domains: Vec::new(),
                subtypes: Vec::new(),
            },
        ],
        relationships: vec![NewRelationship {
            name: format!("wells_inspections_{suffix}"),
            origin_dataset: wells.clone(),
            destination_dataset: inspections,
            origin_foreign_key: "well_id".into(),
            cardinality: "one_to_many".into(),
            forward_label: "has inspections".into(),
            backward_label: "inspected well".into(),
            is_composite: true,
        }],
        attachments: vec![AttachmentOp::Add(NewAttachment {
            dataset: wells,
            feature_id: well_feature.to_string(),
            name: "photo.png".into(),
            content_type: Some("image/png".into()),
            file: "attachments/photo.png".into(),
            metadata: serde_json::json!({ "source_table": "wells__ATTACH" }),
            created_by: "verne-load test".into(),
            global_id: None,
        })],
        log: ExtractionLog::new("verne-load test"),
    }
}

struct Live {
    url: String,
    token: String,
}

/// The live ptolemy, or `None` with a line saying what would have to be set.
fn live() -> Option<Live> {
    let url = std::env::var("VERNE_PTOLEMY_URL").ok()?;
    let token = std::env::var("VERNE_PTOLEMY_TOKEN").unwrap_or_default();
    if token.is_empty() {
        eprintln!(
            "VERNE_PTOLEMY_URL is set and VERNE_PTOLEMY_TOKEN is not; ptolemy gates every write \
             on a bearer token, so the load would only prove that it says so"
        );
        return None;
    }
    Some(Live { url, token })
}

fn skipped() {
    eprintln!(
        "skipping the live load: set VERNE_PTOLEMY_URL to a running ptolemy (for instance \
         http://localhost:3000) and VERNE_PTOLEMY_TOKEN to a bearer token that may write"
    );
}

/// Everything in one sidecar, against the real API. Nothing here is asserted
/// against a fixture of ptolemy's responses: the assertions are that ptolemy
/// accepted each body and gave back an id, which is the only thing a mock
/// could not tell us.
#[test]
fn a_sidecar_loads_into_a_live_ptolemy() {
    let Some(live) = live() else {
        skipped();
        return;
    };
    let suffix = suffix();
    let extraction = an_extraction(&suffix);
    let loader = Loader::new(&live.url, &live.token).expect("the URL is one ptolemy could be at");

    let loaded = loader
        .load(&extraction.sidecar, extraction.directory.path())
        .expect("the sidecar loads");
    eprintln!("loaded {}", loaded.sentence());

    assert_eq!(loaded.datasets.len(), 2);
    // only the dataset with fields got one
    assert_eq!(loaded.schemas.len(), 1);
    assert_eq!(loaded.domains.len(), 2);
    assert_eq!(loaded.subtypes.len(), 2);
    assert_eq!(loaded.relationships.len(), 1);

    let wells = &loaded.datasets[&format!("verne_wells_{suffix}")];
    let client = reqwest::blocking::Client::new();

    // the domains came back on the dataset they were created under
    let domains: serde_json::Value = client
        .get(format!("{}/api/v1/datasets/{wells}/domains", live.url))
        .bearer_auth(&live.token)
        .send()
        .expect("the domains list")
        .json()
        .expect("json");
    let names: Vec<&str> = domains
        .as_array()
        .expect("an array")
        .iter()
        .map(|d| d["name"].as_str().expect("a name"))
        .collect();
    assert!(names.contains(&"status_codes"), "{domains}");
    assert!(names.contains(&"depth_range"), "{domains}");

    // the alias is the reason the schema is carried at all: ptolemy stores it
    // and nothing displays it, so the round trip is the whole contract
    let schema: serde_json::Value = client
        .get(format!("{}/api/v1/datasets/{wells}/schema", live.url))
        .bearer_auth(&live.token)
        .send()
        .expect("the schema")
        .json()
        .expect("json");
    let fields = schema["fields"].as_array().expect("fields");
    assert_eq!(fields[0]["name"], "well_name", "{schema}");
    assert_eq!(fields[0]["alias"], "Well name", "{schema}");
    assert_eq!(fields[0]["field_type"], "string", "{schema}");
    // a field with no alias must not come back with an empty one
    assert!(fields[1]["alias"].is_null(), "{schema}");

    // the subtype's domain assignment holds the id of a domain row, which is
    // the one field the loader had to swap
    let subtypes: serde_json::Value = client
        .get(format!("{}/api/v1/datasets/{wells}/subtypes", live.url))
        .bearer_auth(&live.token)
        .send()
        .expect("the subtypes list")
        .json()
        .expect("json");
    let active = subtypes
        .as_array()
        .expect("an array")
        .iter()
        .find(|s| s["code"] == 1)
        .expect("the active subtype");
    let assigned = active["domain_assignments"]["depth"]
        .as_str()
        .unwrap_or_else(|| panic!("no domain id on the subtype: {active}"));
    let depth_range =
        &loaded.domains[&(format!("verne_wells_{suffix}"), "depth_range".to_string())];
    assert_eq!(assigned, depth_range);

    // the class names both datasets, which is why it had to be created last
    let classes: serde_json::Value = client
        .get(format!(
            "{}/api/v1/datasets/{wells}/relationships",
            live.url
        ))
        .bearer_auth(&live.token)
        .send()
        .expect("the relationship list")
        .json()
        .expect("json");
    let class = &classes.as_array().expect("an array")[0];
    assert_eq!(class["origin_dataset_id"].as_str(), Some(wells.as_str()));
    assert_eq!(class["cardinality"], "one_to_many");
    assert_eq!(class["forward_label"], "has inspections");
    assert_eq!(class["is_composite"], true, "{class}");
}

/// The features and the attachment, read back off the branch the load created.
///
/// This is the pair that had to be done together: an attachment in ptolemy
/// hangs off a feature id, so until verne loaded features there was nothing to
/// hang one off, and a blob on the dataset instead would have thrown away the
/// one thing that makes an attachment worth carrying.
#[test]
fn the_features_and_their_attachment_come_back_off_the_branch() {
    let Some(live) = live() else {
        skipped();
        return;
    };
    let suffix = suffix();
    let extraction = an_extraction(&suffix);
    let expected_feature = extraction.well.clone();
    let loader = Loader::new(&live.url, &live.token).expect("the URL is one ptolemy could be at");

    let loaded = loader
        .load(&extraction.sidecar, extraction.directory.path())
        .expect("the sidecar loads");
    eprintln!("loaded {}", loaded.sentence());

    let wells = format!("verne_wells_{suffix}");
    assert_eq!(loaded.branches.len(), 2, "every dataset gets a branch");
    assert_eq!(loaded.features[&wells].features, 2);
    assert_eq!(loaded.features[&wells].commits, 1);
    // the table with no geometry loaded too, which is the empty geometry
    // collection being accepted rather than refused
    assert_eq!(
        loaded.features[&format!("verne_inspections_{suffix}")].features,
        1
    );
    assert_eq!(loaded.attachments.len(), 1);

    let client = reqwest::blocking::Client::new();
    let branch = &loaded.branches[&wells];

    // the feature is on the branch under the id the extraction minted, which
    // is what let the attachment name it without reading anything back
    let features: serde_json::Value = client
        .get(format!("{}/api/v1/branches/{branch}/features", live.url))
        .bearer_auth(&live.token)
        .send()
        .expect("the feature list")
        .json()
        .expect("json");
    let listed = features["features"].as_array().expect("features");
    assert_eq!(listed.len(), 2, "{features}");
    let alpha = listed
        .iter()
        .find(|f| f["id"].as_str() == Some(expected_feature.as_str()))
        .expect("the attached well is listed");
    assert_eq!(alpha["properties"]["well_name"], "Alpha", "{features}");
    assert_eq!(alpha["properties"]["depth"], 120, "{features}");

    // the original coordinates come back exactly as the extraction wrote them,
    // with the code that says what they are in
    let native = |feature: &str| -> serde_json::Value {
        client
            .get(format!(
                "{}/api/v1/branches/{branch}/features/{feature}/native",
                live.url
            ))
            .bearer_auth(&live.token)
            .send()
            .expect("the native geometry")
            .json()
            .expect("json")
    };
    let coded = native(&expected_feature);
    assert_eq!(
        coded["native_geometry_wkb_hex"].as_str(),
        Some(NATIVE_POINT),
        "{coded}"
    );
    assert_eq!(coded["native_srid"], 26919, "{coded}");
    assert!(coded["native_crs_wkt"].is_null(), "{coded}");

    // and a reference no code names comes back as the WKT it went in as
    let wkt = native(&extraction.well_wkt);
    assert_eq!(
        wkt["native_geometry_wkb_hex"].as_str(),
        Some(NATIVE_POINT),
        "{wkt}"
    );
    assert!(wkt["native_srid"].is_null(), "{wkt}");
    assert_eq!(wkt["native_crs_wkt"], COMPOUND_WKT, "{wkt}");

    // the blob comes back byte for byte, with the content type the source row
    // declared, off the feature it belongs to and not off the dataset
    let id = &loaded.attachments["photo.png"];
    let meta: serde_json::Value = client
        .get(format!("{}/api/v1/attachments/{id}/meta", live.url))
        .bearer_auth(&live.token)
        .send()
        .expect("the attachment metadata")
        .json()
        .expect("json");
    assert_eq!(meta["feature_id"].as_str(), Some(expected_feature.as_str()));
    assert_eq!(meta["branch_id"].as_str(), Some(branch.as_str()));
    assert!(meta["dataset_id"].is_null(), "{meta}");
    assert_eq!(meta["size_bytes"], BLOB.len(), "{meta}");
    assert_eq!(meta["metadata"]["source_table"], "wells__ATTACH", "{meta}");

    let download = client
        .get(format!("{}/api/v1/attachments/{id}", live.url))
        .bearer_auth(&live.token)
        .send()
        .expect("the attachment downloads");
    assert_eq!(
        download
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("image/png")
    );
    assert_eq!(download.bytes().expect("bytes").as_ref(), BLOB);
}

/// ptolemy gates every mutating route on a write ladder, so a loader without a
/// token creates nothing. The failure has to be the server refusing, not the
/// loader deciding not to ask.
#[test]
fn a_load_without_a_token_is_refused_by_ptolemy() {
    let Some(live) = live() else {
        skipped();
        return;
    };
    let extraction = an_extraction(&suffix());
    let loader = Loader::new(&live.url, "").expect("the URL is fine");

    let refused = loader
        .load(&extraction.sidecar, extraction.directory.path())
        .expect_err("an empty token creates nothing");
    let verne_load::LoadError::Refused { status, route, .. } = refused else {
        panic!("expected ptolemy to refuse it: {refused}");
    };
    assert_eq!(route, "/api/v1/datasets");
    assert!(status == 401 || status == 403, "got {status}");
}
