//! The source's drawing rules reaching ptolemy, against a scripted ptolemy.
//!
//! What is under test is verne's own: which route the document goes to, that it
//! goes up untouched inside the tag naming its format, and that a delta does not
//! send it again. The live test is where the body is held against the real API.
//! The socket itself is in `mock`.

mod mock;

use mock::{Ptolemy, Seen, created, ok};
use verne_core::SourceDescription;
use verne_core::sidecar::{DatasetPlan, ExtractionLog, NewDataset, NewField, NewSchema, Sidecar};
use verne_load::Loader;

/// The ids the scripted ptolemy hands out. One per dataset, so a route names
/// which dataset a rule was created under.
const WELLS: &str = "11111111-1111-1111-1111-111111111111";
const LOGS: &str = "22222222-2222-2222-2222-222222222222";
const BRANCH: &str = "33333333-3333-3333-3333-333333333333";
const RULE: &str = "44444444-4444-4444-4444-444444444444";

/// The route the one rule goes to.
fn symbology_route() -> String {
    format!("/api/v1/datasets/{WELLS}/symbology")
}

/// An Esri `drawingInfo` with more in it than any model of verne's holds, which
/// is the reason it is carried as a document.
fn drawing_info() -> serde_json::Value {
    serde_json::json!({
        "renderer": {
            "type": "uniqueValue",
            "field1": "status",
            "uniqueValueInfos": [{
                "value": "1",
                "label": "Active",
                "symbol": {
                    "type": "esriSMS",
                    "style": "esriSMSCircle",
                    "color": [255, 0, 0, 255],
                    "outline": { "color": [0, 0, 0, 255], "width": 0.5 }
                }
            }]
        },
        "labelingInfo": [{ "labelExpression": "[objectid]", "minScale": 5000 }],
        "transparency": 25
    })
}

fn answer(request: &Seen) -> String {
    match (request.method.as_str(), request.path.as_str()) {
        // the id a dataset comes back with is the one its rule's route names
        ("POST", "/api/v1/datasets") => {
            let name = request.json()["name"].as_str().expect("a name").to_string();
            let id = if name == "Wells" { WELLS } else { LOGS };
            created(&serde_json::json!({ "id": id, "name": name }))
        }
        ("POST", path) if path.ends_with("/symbology") => {
            created(&serde_json::json!({ "id": RULE }))
        }
        ("POST", path) if path.ends_with("/branches") => {
            created(&serde_json::json!({ "id": BRANCH }))
        }
        // the incremental path reads what is already there instead
        ("GET", "/api/v1/datasets") => ok(&serde_json::json!([
            { "id": WELLS, "name": "Wells" },
            { "id": LOGS, "name": "Logs" }
        ])),
        ("GET", path) if path.ends_with("/branches") => {
            ok(&serde_json::json!([{ "id": BRANCH, "name": "main" }]))
        }
        (method, path) => panic!("no fixture for {method} {path}"),
    }
}

/// A sidecar of two datasets, the first with drawing info and the second with
/// none, and no features or attachments: what is under test is the style.
fn a_sidecar(incremental: bool) -> Sidecar {
    let plan = |name: &str, drawing: Option<serde_json::Value>| DatasetPlan {
        source_table: name.to_string(),
        layer: None,
        features: None,
        object_id_field: Some("objectid".into()),
        drawing_info: drawing,
        dataset: NewDataset {
            name: name.to_string(),
            srid: 4326,
            geometry_type: "point".into(),
            created_by: "verne-load test".into(),
        },
        schema: NewSchema { fields: Vec::new() },
        domains: Vec::new(),
        subtypes: Vec::new(),
    };
    Sidecar {
        source: SourceDescription::new("ArcGIS Feature Service", "the symbology load test"),
        incremental,
        geopackage: None,
        datasets: vec![plan("Wells", Some(drawing_info())), plan("Logs", None)],
        relationships: Vec::new(),
        attachments: Vec::new(),
        log: ExtractionLog::new("verne-load test"),
    }
}

fn load(ptolemy: &Ptolemy, sidecar: &Sidecar) -> verne_load::Loaded {
    let directory = tempfile::tempdir().expect("tempdir");
    Loader::new(&ptolemy.url, "a-token")
        .expect("the URL is one ptolemy could be at")
        .load(sidecar, directory.path())
        .expect("the sidecar loads")
}

/// The whole document goes up as one rule on the dataset it was read off, inside
/// the tag that says which format it is in. Nothing in the body is verne's
/// reading of an Esri symbol, because verne does not read one.
#[test]
fn the_drawing_info_goes_up_as_one_symbology_rule() {
    let ptolemy = Ptolemy::answering(answer);

    let loaded = load(&ptolemy, &a_sidecar(false));

    let posted = ptolemy.call("POST", &symbology_route()).json();
    assert_eq!(posted["name"], "esri-drawing-info");
    assert_eq!(posted["symbol"]["format"], "esri-drawing-info");
    assert_eq!(posted["symbol"]["drawingInfo"], drawing_info());
    assert_eq!(posted["priority"], 0);
    // the optional bounds are left off, which ptolemy reads as every scale and
    // every feature, and that is what a layer's drawing info says
    let body = posted.as_object().expect("an object");
    assert!(!body.contains_key("min_scale"), "{posted}");
    assert!(!body.contains_key("max_scale"), "{posted}");
    assert!(!body.contains_key("filter_expression"), "{posted}");

    assert_eq!(loaded.symbology["Wells"], RULE);
    // and the dataset whose source said nothing about drawing has no rule
    assert_eq!(loaded.symbology.len(), 1, "{:#?}", loaded.symbology);
    assert_eq!(ptolemy.matching("POST", "/symbology").len(), 1);
    assert!(
        loaded.sentence().contains("1 symbology rules"),
        "{}",
        loaded.sentence()
    );
}

/// The rule is created on the dataset the document came off, so the dataset has
/// to exist first: its id is in the route.
#[test]
fn the_rule_is_created_after_the_dataset_it_hangs_off() {
    let ptolemy = Ptolemy::answering(answer);

    load(&ptolemy, &a_sidecar(false));

    let calls = ptolemy.calls();
    let dataset = calls
        .iter()
        .position(|(method, path)| method == "POST" && path == "/api/v1/datasets")
        .expect("no dataset was created");
    let rule = calls
        .iter()
        .position(|(method, path)| method == "POST" && *path == symbology_route())
        .expect("no rule was created");
    assert!(dataset < rule, "{calls:#?}");
}

/// A schema is set before the style, because both are about the dataset itself
/// and the schema is what ptolemy validates a commit against.
#[test]
fn a_schema_still_goes_first() {
    let ptolemy = Ptolemy::answering(|request| match request.method.as_str() {
        "PUT" => mock::no_content(),
        _ => answer(request),
    });
    let mut sidecar = a_sidecar(false);
    sidecar.datasets[0].schema = NewSchema {
        fields: vec![NewField {
            name: "depth".into(),
            field_type: "integer".into(),
            required: false,
            alias: None,
        }],
    };

    load(&ptolemy, &sidecar);

    let calls = ptolemy.calls();
    let schema = calls
        .iter()
        .position(|(method, _)| method == "PUT")
        .expect("no schema was set");
    let rule = calls
        .iter()
        .position(|(method, path)| method == "POST" && *path == symbology_route())
        .expect("no rule was created");
    assert!(schema < rule, "{calls:#?}");
}

/// A delta commits feature operations. The style the full load created stands, so
/// nothing is posted about it even when the sidecar carries a document: a second
/// rule of the same name would be a second style on the dataset.
#[test]
fn an_incremental_load_sends_no_symbology() {
    let ptolemy = Ptolemy::answering(answer);

    let loaded = load(&ptolemy, &a_sidecar(true));

    assert!(
        ptolemy.matching("POST", "/symbology").is_empty(),
        "{:#?}",
        ptolemy.calls()
    );
    assert!(loaded.symbology.is_empty());
    // it did find the datasets the first load created, so the delta ran
    assert_eq!(loaded.datasets.len(), 2);
}
