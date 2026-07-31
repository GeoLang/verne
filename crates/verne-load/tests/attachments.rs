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
//! a wrong assertion.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

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

// ─── The scripted ptolemy ────────────────────────────────────────────

/// One request the loader made, as the socket saw it.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    body: String,
}

/// A ptolemy that answers the routes a delta load asks for and remembers what it
/// was asked. The attachments it holds are state: a delete takes one out and an
/// upload puts one in, so a listing after either says what the load did.
struct Ptolemy {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Ptolemy {
    /// A ptolemy holding these attachments on the one feature, as `(id, name)`.
    fn holding(held: &[(&str, &str)]) -> Ptolemy {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let url = format!("http://{}", listener.local_addr().expect("the address"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let attachments: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(
            held.iter()
                .map(|(id, name)| ((*id).to_string(), (*name).to_string()))
                .collect(),
        ));
        let recorded = Arc::clone(&seen);
        // detached: the test ends with the process, and a listener with nothing
        // left to answer costs nothing
        std::thread::spawn(move || {
            let mut uploads = 0usize;
            for stream in listener.incoming() {
                let mut stream = stream.expect("a connection");
                let Some(request) = read_request(&mut stream) else {
                    continue;
                };
                recorded
                    .lock()
                    .expect("the request log")
                    .push(request.clone());
                let answer = answer(&request, &attachments, &mut uploads);
                stream.write_all(answer.as_bytes()).expect("an answer");
                stream.flush().expect("a flushed answer");
            }
        });
        Ptolemy { url, seen }
    }

    /// What it was asked, as `(method, path)`, in order.
    fn calls(&self) -> Vec<(String, String)> {
        self.seen
            .lock()
            .expect("the request log")
            .iter()
            .map(|held| (held.method.clone(), held.path.clone()))
            .collect()
    }

    /// The body of the one request that was `method path`.
    fn body(&self, method: &str, path: &str) -> String {
        self.seen
            .lock()
            .expect("the request log")
            .iter()
            .find(|held| held.method == method && held.path == path)
            .map(|held| held.body.clone())
            .unwrap_or_else(|| panic!("no {method} {path} was made: {:#?}", self.calls()))
    }
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<Seen> {
    let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("a request line");
    let mut words = line.split_whitespace();
    let method = words.next()?.to_string();
    let path = words.next()?.to_string();
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).expect("a header");
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().expect("a content length");
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).expect("the body");
    Some(Seen {
        method,
        path,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// The one place the scripted routes are: what each answers, and what it does to
/// the attachments the ptolemy holds.
fn answer(
    request: &Seen,
    attachments: &Mutex<Vec<(String, String)>>,
    uploads: &mut usize,
) -> String {
    let listing = format!("/api/v1/branches/{BRANCH}/features/{FEATURE}/attachments");
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/v1/datasets") => {
            ok(&serde_json::json!([{ "id": DATASET, "name": "Wells" }]).to_string())
        }
        ("GET", path) if path == format!("/api/v1/datasets/{DATASET}/branches") => {
            ok(&serde_json::json!([{ "id": BRANCH, "name": "main" }]).to_string())
        }
        ("GET", path) if path == listing => {
            let held: Vec<serde_json::Value> = attachments
                .lock()
                .expect("the attachments")
                .iter()
                .map(|(id, name)| serde_json::json!({ "id": id, "name": name }))
                .collect();
            ok(&serde_json::Value::Array(held).to_string())
        }
        ("POST", path) if path == listing => {
            *uploads += 1;
            let id = format!("uploaded-{uploads}");
            let name = serde_json::from_str::<serde_json::Value>(&request.body)
                .expect("an upload body")["name"]
                .as_str()
                .expect("a name")
                .to_string();
            attachments
                .lock()
                .expect("the attachments")
                .push((id.clone(), name));
            ok(&serde_json::json!({ "id": id }).to_string())
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
            "HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
        }
        (method, path) => panic!("no fixture for {method} {path}"),
    }
}

fn ok(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: \
         close\r\n\r\n{body}",
        body.len()
    )
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
    let ptolemy = Ptolemy::holding(&[(LOADED, "photo.png")]);
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
    let body: serde_json::Value =
        serde_json::from_str(&ptolemy.body("POST", &listing)).expect("the upload body");
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
    let ptolemy = Ptolemy::holding(&[(LOADED, "photo.png")]);
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
    let ptolemy = Ptolemy::holding(&[(LOADED, "twin.png"), (TWIN, "twin.png")]);
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
    let ptolemy = Ptolemy::holding(&[]);
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
    let body: serde_json::Value =
        serde_json::from_str(&ptolemy.body("POST", &listing)).expect("the upload body");
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

/// The counts and the reasons are the load's answer about the attachments, which
/// is what the caller prints.
#[test]
fn a_delta_of_several_operations_counts_each_kind() {
    let ptolemy = Ptolemy::holding(&[(LOADED, "photo.png"), (TWIN, "notes.txt")]);
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
