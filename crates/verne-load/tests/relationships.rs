//! Relationship classes reaching ptolemy, against a scripted ptolemy.
//!
//! What is under test is the body verne posts: both sides swapped for the ids
//! their datasets came back with, and the composite flag as the source stated
//! it. The live test is where the body is held against the real API. The socket
//! itself is in `mock`.

mod mock;

use mock::{Ptolemy, Seen, created};
use verne_core::SourceDescription;
use verne_core::sidecar::{
    DatasetPlan, ExtractionLog, NewDataset, NewRelationship, NewSchema, Sidecar,
};
use verne_load::Loader;

const WELLS: &str = "11111111-1111-1111-1111-111111111111";
const INSPECTIONS: &str = "22222222-2222-2222-2222-222222222222";
const PADS: &str = "33333333-3333-3333-3333-333333333333";
const BRANCH: &str = "44444444-4444-4444-4444-444444444444";
const CLASS: &str = "55555555-5555-5555-5555-555555555555";

fn answer(request: &Seen) -> String {
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/api/v1/datasets") => {
            let name = request.json()["name"].as_str().expect("a name").to_string();
            let id = match name.as_str() {
                "wells" => WELLS,
                "inspections" => INSPECTIONS,
                _ => PADS,
            };
            created(&serde_json::json!({ "id": id, "name": name }))
        }
        ("POST", path) if path.ends_with("/branches") => {
            created(&serde_json::json!({ "id": BRANCH }))
        }
        ("POST", path) if path.ends_with("/relationships") => {
            created(&serde_json::json!({ "id": CLASS }))
        }
        (method, path) => panic!("no fixture for {method} {path}"),
    }
}

fn dataset(name: &str) -> DatasetPlan {
    DatasetPlan {
        source_table: name.to_string(),
        layer: None,
        features: None,
        object_id_field: None,
        drawing_info: None,
        dataset: NewDataset {
            name: name.to_string(),
            srid: 4326,
            geometry_type: "point".into(),
            created_by: "verne-load test".into(),
        },
        schema: NewSchema { fields: Vec::new() },
        domains: Vec::new(),
        subtypes: Vec::new(),
    }
}

/// Two classes off one origin: the composite one, where deleting a well deletes
/// its inspections, and a plain one where it does not.
fn a_sidecar() -> Sidecar {
    let class = |name: &str, destination: &str, is_composite: bool| NewRelationship {
        name: name.to_string(),
        origin_dataset: "wells".into(),
        destination_dataset: destination.to_string(),
        origin_foreign_key: "well_id".into(),
        cardinality: "one_to_many".into(),
        forward_label: "has inspections".into(),
        backward_label: "inspected well".into(),
        is_composite,
    };
    Sidecar {
        source: SourceDescription::new("File Geodatabase", "the relationship load test"),
        incremental: false,
        geopackage: None,
        datasets: vec![dataset("wells"), dataset("inspections"), dataset("pads")],
        relationships: vec![
            class("wells_inspections", "inspections", true),
            class("wells_pads", "pads", false),
        ],
        attachments: Vec::new(),
        log: ExtractionLog::new("verne-load test"),
    }
}

fn posted(ptolemy: &Ptolemy, name: &str) -> serde_json::Value {
    ptolemy
        .matching("POST", "/relationships")
        .iter()
        .map(Seen::json)
        .find(|body| body["name"] == name)
        .unwrap_or_else(|| panic!("no {name} was created: {:#?}", ptolemy.calls()))
}

/// The source says a class cascades its deletes and ptolemy's create route
/// takes that, so it goes in the body rather than being dropped.
#[test]
fn a_composite_class_is_created_composite() {
    let ptolemy = Ptolemy::answering(answer);
    let directory = tempfile::tempdir().expect("tempdir");

    let loaded = Loader::new(&ptolemy.url, "a-token")
        .expect("the URL is one ptolemy could be at")
        .load(&a_sidecar(), directory.path())
        .expect("the sidecar loads");

    let composite = posted(&ptolemy, "wells_inspections");
    assert_eq!(composite["is_composite"], true, "{composite}");
    assert_eq!(composite["origin_dataset_id"], WELLS, "{composite}");
    assert_eq!(
        composite["destination_dataset_id"], INSPECTIONS,
        "{composite}"
    );
    assert_eq!(loaded.relationships["wells_inspections"], CLASS);
}

/// And a class the source did not call composite says so, rather than leaving
/// ptolemy to read the default off an absent key.
#[test]
fn a_plain_class_says_it_is_not_composite() {
    let ptolemy = Ptolemy::answering(answer);
    let directory = tempfile::tempdir().expect("tempdir");

    Loader::new(&ptolemy.url, "a-token")
        .expect("the URL is one ptolemy could be at")
        .load(&a_sidecar(), directory.path())
        .expect("the sidecar loads");

    let plain = posted(&ptolemy, "wells_pads");
    assert_eq!(plain["is_composite"], false, "{plain}");
}
