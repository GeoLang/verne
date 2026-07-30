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

use verne_core::SourceDescription;
use verne_core::sidecar::{
    DatasetPlan, ExtractionLog, NewDataset, NewDomain, NewField, NewRelationship, NewSchema,
    NewSubtype, Sidecar,
};
use verne_load::Loader;

/// Every run makes its own names: ptolemy's dataset name is unique across the
/// instance, so a second run against the same database would collide.
fn suffix() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_nanos();
    format!("{now:x}")
}

fn a_sidecar(suffix: &str) -> Sidecar {
    let wells = format!("verne_wells_{suffix}");
    let inspections = format!("verne_inspections_{suffix}");

    let mut assignments = BTreeMap::new();
    assignments.insert("depth".to_string(), "depth_range".to_string());
    let mut defaults = serde_json::Map::new();
    defaults.insert("depth".to_string(), serde_json::json!("100"));

    Sidecar {
        source: SourceDescription::new("Esri file geodatabase", "the live load test"),
        geopackage: Some("features.gpkg".into()),
        datasets: vec![
            DatasetPlan {
                source_table: "wells".into(),
                layer: Some("wells".into()),
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
            origin_dataset: wells,
            destination_dataset: inspections,
            origin_foreign_key: "well_id".into(),
            cardinality: "one_to_many".into(),
            forward_label: "has inspections".into(),
            backward_label: "inspected well".into(),
        }],
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
    let sidecar = a_sidecar(&suffix);
    let loader = Loader::new(&live.url, &live.token).expect("the URL is one ptolemy could be at");

    let loaded = loader.load(&sidecar).expect("the sidecar loads");
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
    let sidecar = a_sidecar(&suffix());
    let loader = Loader::new(&live.url, "").expect("the URL is fine");

    let refused = loader
        .load(&sidecar)
        .expect_err("an empty token creates nothing");
    let verne_load::LoadError::Refused { status, route, .. } = refused else {
        panic!("expected ptolemy to refuse it: {refused}");
    };
    assert_eq!(route, "/api/v1/datasets");
    assert!(status == 401 || status == 403, "got {status}");
}
