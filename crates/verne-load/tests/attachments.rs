//! A delta's attachment operations, against a scripted ptolemy.
//!
//! The live test is where a request shape is held against the real API, and a
//! mock cannot do that job. What is under test here is verne's own: ptolemy has
//! no route that changes an attachment, so a replacement is a delete and an
//! upload, and the order of those two is the whole contract. A socket is the
//! only place that order can be seen, so this one answers on one.
//!
//! Every route the loader touches is scripted, and a route it asks for that is
//! not here is a panic rather than a 404: a missing fixture must be louder than
//! a wrong assertion. The socket itself is in `mock`.

mod mock;

use std::collections::BTreeMap;
use std::sync::Mutex;

use mock::{Ptolemy, Seen, created, no_content, ok};
use verne_core::SourceDescription;
use verne_core::sidecar::{
    AttachmentOp, DatasetPlan, DeleteAttachment, ExtractionLog, NewAttachment, NewDataset,
    NewSchema, Sidecar,
};
use verne_load::Loader;

/// The ids the scripted ptolemy hands out, so an assertion can name them.
const DATASET: &str = "11111111-1111-1111-1111-111111111111";
const BRANCH: &str = "22222222-2222-2222-2222-222222222222";
const FEATURE: &str = "33333333-3333-3333-3333-333333333333";
const LOADED: &str = "44444444-4444-4444-4444-444444444444";
const TWIN: &str = "55555555-5555-5555-5555-555555555555";

/// The bytes the delta carries for the replacement.
const FRESH: &[u8] = b"the second copy";

// ─── The scripted routes ─────────────────────────────────────────────

/// A ptolemy holding these attachments on the one feature, as `(id, name)`. What
/// it holds is state: a delete takes one out and an upload puts one in, so a
/// listing after either says what the load did.
fn holding(held: &[(&str, &str)]) -> Ptolemy {
    let attachments = Mutex::new(
        held.iter()
            .map(|(id, name)| ((*id).to_string(), (*name).to_string()))
            .collect::<Vec<(String, String)>>(),
    );
    let uploads = Mutex::new(0usize);
    Ptolemy::answering(move |request| answer(request, &attachments, &uploads))
}

/// What each route answers, and what it does to the attachments held.
fn answer(
    request: &Seen,
    attachments: &Mutex<Vec<(String, String)>>,
    uploads: &Mutex<usize>,
) -> String {
    let listing = format!("/api/v1/branches/{BRANCH}/features/{FEATURE}/attachments");
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/v1/datasets") => ok(&serde_json::json!([{ "id": DATASET, "name": "Wells" }])),
        ("GET", path) if path == format!("/api/v1/datasets/{DATASET}/branches") => {
            ok(&serde_json::json!([{ "id": BRANCH, "name": "main" }]))
        }
        ("GET", path) if path == listing => {
            let held: Vec<serde_json::Value> = attachments
                .lock()
                .expect("the attachments")
                .iter()
                .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
                .collect();
            ok(&serde_json::Value::Array(held))
        }
        ("POST", path) if path == format!("/api/v1/branches/{BRANCH}/commit") => {
            created(&serde_json::json!({ "id": "committed" }))
        }
        ("POST", path) if path == listing => {
            let mut count = uploads.lock().expect("the upload count");
            *count += 1;
            let id = format!("uploaded-{count}");
            let name = request.json()["name"].as_str().expect("a name").to_string();
            attachments
                .lock()
                .expect("the attachments")
                .push((id.clone(), name));
            created(&serde_json::json!({ "id": id }))
        }
        ("DELETE", path) => {
            let id = path
                .strip_prefix("/api/v1/attachments/")
                .unwrap_or_else(|| panic!("nothing at {path}"))
                .to_string();
            attachments
                .lock()
                .expect("the attachments")
                .retain(|(held, _)| *held != id);
            no_content()
        }
        (method, path) => panic!("no fixture for {method} {path}"),
    }
}

// ─── The delta ───────────────────────────────────────────────────────

/// An extraction directory holding the blob the delta carries, and a sidecar of
/// the attachment operations given.
fn a_delta(ops: Vec<AttachmentOp>) -> (Sidecar, tempfile::TempDir) {
    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(directory.path().join("attachments/Wells")).expect("the blob dir");
    std::fs::write(
        directory.path().join("attachments/Wells/0-photo.png"),
        FRESH,
    )
    .expect("the blob");
    let sidecar = Sidecar {
        source: SourceDescription::new("ArcGIS Feature Service", "the attachment load test"),
        incremental: true,
        geopackage: None,
        datasets: vec![DatasetPlan {
            source_table: "Wells".into(),
            layer: None,
            // no feature file: what is under test is what happens after the
            // features, and an empty delta commits nothing
            features: None,
            object_id_field: Some("objectid".into()),
            drawing_info: None,
            dataset: NewDataset {
                name: "Wells".into(),
                srid: 4326,
                geometry_type: "point".into(),
                created_by: "verne-load test".into(),
            },
            schema: NewSchema { fields: Vec::new() },
            domains: Vec::new(),
            subtypes: Vec::new(),
        }],
        relationships: Vec::new(),
        attachments: ops,
        log: ExtractionLog::new("verne-load test"),
    };
    (sidecar, directory)
}

fn replacement(name: &str) -> AttachmentOp {
    AttachmentOp::Update(NewAttachment {
        dataset: "Wells".into(),
        feature_id: FEATURE.into(),
        name: name.into(),
        content_type: Some("image/png".into()),
        file: "attachments/Wells/0-photo.png".into(),
        metadata: serde_json::json!({}),
        created_by: "verne-load test".into(),
        global_id: Some("{A1B2C3D4-0000-0000-0000-000000000001}".into()),
    })
}

fn base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn load(ptolemy: &Ptolemy, sidecar: &Sidecar, directory: &tempfile::TempDir) -> verne_load::Loaded {
    Loader::new(&ptolemy.url, "a-token")
        .expect("the URL is one ptolemy could be at")
        .load(sidecar, directory.path())
        .expect("the delta loads")
}

// ─── The tests ───────────────────────────────────────────────────────

/// A replacement is the loaded copy deleted and the new bytes uploaded, in that
/// order. The other order would leave two attachments of one name on the feature
/// with nothing to tell them apart, and the next delta pairs on that name.
#[test]
fn a_replacement_deletes_the_loaded_copy_before_uploading_the_new_bytes() {
    let ptolemy = holding(&[(LOADED, "photo.png")]);
    let (sidecar, directory) = a_delta(vec![replacement("photo.png")]);

    let loaded = load(&ptolemy, &sidecar, &directory);

    let listing = format!("/api/v1/branches/{BRANCH}/features/{FEATURE}/attachments");
    let attachment_calls: Vec<(String, String)> = ptolemy
        .calls()
        .into_iter()
        .filter(|(_, path)| path.contains("attachments"))
        .collect();
    assert_eq!(
        attachment_calls,
        vec![
            // the loaded copy is found by name, because ptolemy minted its id
            // and no extraction ever saw one
            ("GET".to_string(), listing.clone()),
            (
                "DELETE".to_string(),
                format!("/api/v1/attachments/{LOADED}")
            ),
            ("POST".to_string(), listing.clone()),
        ],
        "{:#?}",
        ptolemy.calls()
    );

    // the bytes that went up are the delta's, base64ed into the body ptolemy
    // takes
    let body = ptolemy.call("POST", &listing).json();
    assert_eq!(body["name"], "photo.png");
    assert_eq!(body["data"], base64(FRESH));
    assert_eq!(body["content_type"], "image/png");

    assert_eq!(loaded.attachment_ops.replaced, 1);
    assert_eq!(loaded.attachment_ops.added, 0);
    assert!(loaded.attachment_ops.unmatched.is_empty());
    assert_eq!(loaded.attachments["photo.png"], "uploaded-1");
}

/// A delete finds the loaded copy the same way and uploads nothing.
#[test]
fn a_delete_removes_the_loaded_copy_and_uploads_nothing() {
    let ptolemy = holding(&[(LOADED, "photo.png")]);
    let (sidecar, directory) = a_delta(vec![AttachmentOp::Delete(DeleteAttachment {
        dataset: "Wells".into(),
        feature_id: FEATURE.into(),
        name: "photo.png".into(),
        global_id: Some("{A1B2C3D4-0000-0000-0000-000000000001}".into()),
    })]);

    let loaded = load(&ptolemy, &sidecar, &directory);

    assert!(
        ptolemy.calls().contains(&(
            "DELETE".to_string(),
            format!("/api/v1/attachments/{LOADED}")
        )),
        "{:#?}",
        ptolemy.calls()
    );
    assert!(
        !ptolemy
            .calls()
            .iter()
            .any(|(method, path)| method == "POST" && path.ends_with("attachments")),
        "a delete uploaded something: {:#?}",
        ptolemy.calls()
    );
    assert_eq!(loaded.attachment_ops.deleted, 1);
    assert!(loaded.attachment_ops.unmatched.is_empty());
}

/// Two attachments of one name on one feature is a pairing the loader will not
/// pick between: it refuses that operation, says so, and leaves both alone. The
/// rest of the delta still loads.
#[test]
fn two_attachments_of_one_name_refuse_the_operation() {
    let ptolemy = holding(&[(LOADED, "twin.png"), (TWIN, "twin.png")]);
    let (sidecar, directory) = a_delta(vec![replacement("twin.png")]);

    let loaded = load(&ptolemy, &sidecar, &directory);

    assert!(
        !ptolemy
            .calls()
            .iter()
            .any(|(method, _)| method == "DELETE" || method == "POST"),
        "an ambiguous name was acted on anyway: {:#?}",
        ptolemy.calls()
    );
    assert_eq!(loaded.attachment_ops.replaced, 0);
    assert_eq!(loaded.attachment_ops.unmatched.len(), 1);
    assert!(
        loaded.attachment_ops.unmatched[0].contains("2 attachments")
            && loaded.attachment_ops.unmatched[0].contains("twin.png"),
        "{:#?}",
        loaded.attachment_ops.unmatched
    );
}

/// An addition is an upload and nothing else: there is no loaded copy to find.
#[test]
fn an_addition_uploads_without_listing_anything() {
    let ptolemy = holding(&[]);
    let (sidecar, directory) = a_delta(vec![AttachmentOp::Add(NewAttachment {
        dataset: "Wells".into(),
        feature_id: FEATURE.into(),
        name: "photo.png".into(),
        content_type: None,
        file: "attachments/Wells/0-photo.png".into(),
        metadata: serde_json::json!({ "source_layer": "Wells" }),
        created_by: "verne-load test".into(),
        global_id: Some("{A1B2C3D4-0000-0000-0000-000000000002}".into()),
    })]);

    let loaded = load(&ptolemy, &sidecar, &directory);

    let methods: Vec<String> = ptolemy
        .calls()
        .into_iter()
        .filter(|(_, path)| path.contains("attachments"))
        .map(|(method, _)| method)
        .collect();
    assert_eq!(methods, vec!["POST".to_string()], "{:#?}", ptolemy.calls());
    assert_eq!(loaded.attachment_ops.added, 1);
    // the metadata rides along, which is where everything ptolemy has no field
    // for is kept
    let listing = format!("/api/v1/branches/{BRANCH}/features/{FEATURE}/attachments");
    let body = ptolemy.call("POST", &listing).json();
    assert_eq!(body["metadata"]["source_layer"], "Wells");
    // no content type is no key on the wire, so ptolemy's own default stands
    assert!(
        body.as_object()
            .expect("an object")
            .get("content_type")
            .is_none(),
        "{body}"
    );
}

/// An attachment on a feature the same delta deletes is deleted by an operation
/// of its own, and it works because the feature's attachments outlive it: a
/// delete in ptolemy writes a new version of the feature rather than removing a
/// row, its attachments keep hanging off the feature id, and the listing this
/// finds them through still answers with them. So the operation comes after the
/// commit that deleted the feature and still has something to delete.
#[test]
fn an_attachment_on_a_deleted_feature_is_deleted_after_the_commit() {
    let ptolemy = holding(&[(LOADED, "site.jpg")]);
    let (mut sidecar, directory) = a_delta(vec![AttachmentOp::Delete(DeleteAttachment {
        dataset: "Wells".into(),
        feature_id: FEATURE.into(),
        name: "site.jpg".into(),
        global_id: None,
    })]);
    std::fs::create_dir_all(directory.path().join("features")).expect("the feature dir");
    std::fs::write(
        directory.path().join("features/Wells.ndjson"),
        serde_json::json!({ "type": "delete", "feature_id": FEATURE }).to_string() + "\n",
    )
    .expect("the feature file");
    sidecar.datasets[0].features = Some("features/Wells.ndjson".into());

    let loaded = load(&ptolemy, &sidecar, &directory);

    let calls = ptolemy.calls();
    let commit = calls
        .iter()
        .position(|(method, path)| {
            method == "POST" && *path == format!("/api/v1/branches/{BRANCH}/commit")
        })
        .unwrap_or_else(|| panic!("the feature delete was never committed: {calls:#?}"));
    let deleted = calls
        .iter()
        .position(|(method, path)| method == "DELETE" && path.ends_with(LOADED))
        .unwrap_or_else(|| panic!("the attachment was never deleted: {calls:#?}"));
    assert!(commit < deleted, "{calls:#?}");
    assert_eq!(loaded.attachment_ops.deleted, 1);
    assert!(loaded.attachment_ops.unmatched.is_empty());
}

/// The counts and the reasons are the load's answer about the attachments, which
/// is what the caller prints.
#[test]
fn a_delta_of_several_operations_counts_each_kind() {
    let ptolemy = holding(&[(LOADED, "photo.png"), (TWIN, "notes.txt")]);
    let (sidecar, directory) = a_delta(vec![
        replacement("photo.png"),
        AttachmentOp::Delete(DeleteAttachment {
            dataset: "Wells".into(),
            feature_id: FEATURE.into(),
            name: "notes.txt".into(),
            global_id: None,
        }),
        AttachmentOp::Delete(DeleteAttachment {
            dataset: "Wells".into(),
            feature_id: FEATURE.into(),
            name: "never-loaded.txt".into(),
            global_id: None,
        }),
    ]);

    let loaded = load(&ptolemy, &sidecar, &directory);

    assert_eq!(loaded.attachment_ops.replaced, 1);
    assert_eq!(loaded.attachment_ops.deleted, 1);
    assert_eq!(loaded.attachment_ops.unmatched.len(), 1);
    assert!(
        loaded.attachment_ops.unmatched[0].contains("nothing to delete"),
        "{:#?}",
        loaded.attachment_ops.unmatched
    );
    // the dataset and its branch were found by name, which is all an incremental
    // load creates: nothing
    assert_eq!(
        loaded.datasets,
        BTreeMap::from([("Wells".to_string(), DATASET.to_string())])
    );
    assert_eq!(
        loaded.branches,
        BTreeMap::from([("Wells".to_string(), BRANCH.to_string())])
    );
}
