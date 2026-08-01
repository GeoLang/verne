//! A delta the service itself worked out: `extractChanges`.
//!
//! The fixture's second state is the one `incremental.rs` diffs locally, so the
//! operations a change file leads to can be held against the ones the local
//! diff writes: 1 got deeper, 2 is as it was, 3 vanished, 4 is new, and 5 never
//! moves in any window, which is what makes it the parent an attachment can be
//! added to without its own row being fetched. What differs is how they were
//! found, and how many rows had to be fetched to find them.
//!
//! The window also carries attachment edits, which is the other half of what a
//! change file says: one added, one replaced, and one deleted that nothing was
//! ever loaded under.
//!
//! Every route here is a fixture. The one part that is tested against a socket
//! is the client's own: a result URL redirects to a signed file on a host that
//! is not the service, and what must not go with it is the token.

mod common;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread::JoinHandle;

use common::{
    Fake, ROOT, added, logs_table, service_root, tracked_wells_layer, tracking_root, well_guid,
};
use serde_json::json;
use verne_arcgis::{
    ATTACHMENT_IDS_DIR, ArcgisError, ArcgisSource, Credentials, Extraction, Fetch, HttpFetch,
    OBJECT_IDS_DIR, SERVER_GENS_FILE,
};
use verne_core::{Action, AttachmentOp, FeatureOp, NewFeature};

const OPERATOR: &str = "verne-arcgis test";

/// A Date attribute as the service sends it, epoch milliseconds.
const DRILLED: i64 = 1743630690000;

/// The attachment the full extraction carries, on well 1: the id the service
/// knows it by, and the bytes at that point.
const PHOTO: &str = "{A1B2C3D4-0000-0000-0000-000000000001}";
const PHOTO_BYTES: &[u8] = b"\x89PNG the first";

/// The bytes the same attachment holds after the window, which is what a
/// replacement has to end up with.
const PHOTO_AGAIN: &[u8] = b"\x89PNG the second";

/// The attachment the first window adds, to a well that did not itself change.
const LOGS: &str = "{A1B2C3D4-0000-0000-0000-000000000002}";
const LOGS_BYTES: &[u8] = b"%PDF logs";

/// An attachment the service says is gone that nothing was ever loaded under.
const NEVER_LOADED: &str = "{A1B2C3D4-0000-0000-0000-00000000ffff}";

/// The generation the full extraction reads at, and the one the change file's
/// window ends at.
const FIRST_GEN: u64 = 4277428;
const NEXT_GEN: u64 = 4277901;
/// And the one the window after that ends at, for a delta of a delta.
const LATER_GEN: u64 = 4278115;

/// Where the fixture's async job lives, as URLs under the fixture's root so
/// every request stays on the fake.
const STATUS_URL: &str = "/jobs/j1";
const RESULT_URL: &str = "/jobs/j1/result";

fn param<'a>(params: &'a [(&str, String)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(held, _)| *held == name)
        .map(|(_, value)| value.as_str())
}

/// A row of the point layer, in the shape the query route answers with. Every
/// row carries its global id, because the layer declares one.
fn row(oid: i64, depth: f64, drilled: Option<i64>, x: f64, y: f64) -> serde_json::Value {
    json!({
        "attributes": {
            "objectid": oid,
            "globalid": well_guid(oid),
            "status": if oid == 2 { 2 } else { 1 },
            "depth": depth,
            "drilled": drilled
        },
        "geometry": { "x": x, "y": y }
    })
}

/// Well 5 never changes in any window, which makes it the parent an attachment
/// can be added to without the feature itself being fetched.
fn quiet_row() -> serde_json::Value {
    row(5, 60.0, None, 3.9, 50.5)
}

/// The rows of the point layer's second state, by object id.
fn changed_row(oid: i64) -> serde_json::Value {
    match oid {
        1 => row(1, 13.0, Some(DRILLED), 3.5, 50.1),
        2 => row(2, 30.0, None, 3.6, 50.2),
        4 => row(4, 55.0, None, 3.8, 50.4),
        5 => quiet_row(),
        other => panic!(
            "the extraction asked for object id {other}, which the second state has no row for"
        ),
    }
}

/// The layer answering `where globalid IN (...)`, which is how the parent of an
/// added attachment is found when that feature did not itself change. Only the
/// two columns the pairing needs come back, as the extraction asks.
fn by_global_id(clause: &str) -> Vec<u8> {
    let rows: Vec<serde_json::Value> = (1..=5)
        .filter(|oid| clause.contains(&well_guid(*oid)))
        .map(|oid| json!({ "attributes": { "objectid": oid, "globalid": well_guid(oid) } }))
        .collect();
    serde_json::to_vec(&json!({ "objectIdFieldName": "objectid", "features": rows }))
        .expect("the page serialises")
}

/// Whether a query is the global id pairing rather than a page or a count.
fn pairing(params: &[(&str, String)]) -> Option<String> {
    param(params, "where")
        .filter(|clause| clause.starts_with("globalid IN"))
        .map(str::to_string)
}

/// The native second pass: the rows again by object id, untransformed.
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

/// The first state of the point layer: features 1, 2, 3 and 5, a page at a time.
fn wells_full(params: &[(&str, String)]) -> Vec<u8> {
    if param(params, "returnCountOnly").is_some() {
        return serde_json::to_vec(&json!({ "count": 4 })).expect("the count serialises");
    }
    if let Some(oids) = param(params, "objectIds") {
        return native_page(oids);
    }
    let page = match param(params, "resultOffset") {
        Some("0") => json!({
            "objectIdFieldName": "objectid",
            "features": [row(1, 12.5, Some(DRILLED), 3.5, 50.1), row(2, 30.0, None, 3.6, 50.2)],
            "exceededTransferLimit": true
        }),
        Some("2") => json!({
            "objectIdFieldName": "objectid",
            "features": [row(3, 40.0, Some(DRILLED), 3.7, 50.3), quiet_row()]
        }),
        other => panic!("the extraction asked for offset {other:?}"),
    };
    serde_json::to_vec(&page).expect("the page serialises")
}

/// The second state, answering the two ways it can be asked: by the object ids
/// a change file named, which is a POST carrying every column, or page by page
/// for a local diff. The native pass asks by object id too, and is told apart
/// by asking for the object id column alone.
fn wells_changed(params: &[(&str, String)]) -> Vec<u8> {
    if param(params, "returnCountOnly").is_some() {
        return serde_json::to_vec(&json!({ "count": 4 })).expect("the count serialises");
    }
    if let Some(clause) = pairing(params) {
        return by_global_id(&clause);
    }
    if let Some(oids) = param(params, "objectIds") {
        if param(params, "outFields") != Some("*") {
            return native_page(oids);
        }
        let rows: Vec<serde_json::Value> = oids
            .split(',')
            .map(|oid| changed_row(oid.parse().expect("an object id")))
            .collect();
        return serde_json::to_vec(&json!({ "objectIdFieldName": "objectid", "features": rows }))
            .expect("the page serialises");
    }
    let page = match param(params, "resultOffset") {
        Some("0") => json!({
            "objectIdFieldName": "objectid",
            "features": [changed_row(1), changed_row(2)],
            "exceededTransferLimit": true
        }),
        Some("2") => json!({
            "objectIdFieldName": "objectid",
            "features": [changed_row(4), changed_row(5)]
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

/// The change file the fixture's job writes: 4 added, 1 and 2 updated, 3
/// deleted, and three attachment edits.
///
/// The add carries a depth the query route does not answer with, which is what
/// proves the features themselves are fetched rather than read out of here.
/// The attachment records are the shape a live service writes: the bytes are
/// behind a URL on the service's own host, a delete names a global id and not
/// the numeric id an add carries, and the feature an attachment hangs off is
/// named by global id.
fn change_file() -> serde_json::Value {
    json!({
        "edits": [
            {
                "id": 0,
                "features": {
                    "adds": [{
                        "attributes": { "objectid": 4, "globalid": well_guid(4), "status": 1, "depth": 999.0, "drilled": null },
                        "geometry": { "x": 3.8, "y": 50.4 }
                    }],
                    "updates": [
                        {
                            "attributes": { "objectid": 1, "globalid": well_guid(1), "status": 1, "depth": 999.0, "drilled": DRILLED },
                            "geometry": { "x": 3.5, "y": 50.1 }
                        },
                        {
                            "attributes": { "objectid": 2, "globalid": well_guid(2), "status": 2, "depth": 999.0, "drilled": null },
                            "geometry": { "x": 3.6, "y": 50.2 }
                        }
                    ],
                    "deleteIds": [3]
                },
                "attachments": {
                    "adds": [logs_add()],
                    "updates": [{
                        "attachmentId": 1,
                        "globalId": PHOTO,
                        "parentGlobalId": well_guid(1),
                        "contentType": "image/png",
                        "name": "photo.png",
                        "size": PHOTO_AGAIN.len(),
                        "url": format!("{ROOT}/0/1/attachments/1")
                    }],
                    "deleteIds": [NEVER_LOADED]
                }
            },
            { "id": 1, "features": { "adds": [], "updates": [], "deleteIds": [] } }
        ],
        "layerServerGens": [
            { "id": 0, "serverGen": NEXT_GEN },
            { "id": 1, "serverGen": NEXT_GEN }
        ]
    })
}

/// The add the window names: a log on well 5, which nothing was ever loaded for.
fn logs_add() -> serde_json::Value {
    json!({
        "attachmentId": 7,
        "globalId": LOGS,
        "parentGlobalId": well_guid(5),
        "contentType": "application/pdf",
        "name": "logs.pdf",
        "size": LOGS_BYTES.len(),
        "url": format!("{ROOT}/0/5/attachments/7")
    })
}

/// The photo on well 1 as an add rather than an update, which is how a window
/// that ends where the last one began reports it, with the size the service says
/// it is now.
fn photo_add(size: usize) -> serde_json::Value {
    json!({
        "attachmentId": 1,
        "globalId": PHOTO,
        "parentGlobalId": well_guid(1),
        "contentType": "image/png",
        "name": "photo.png",
        "size": size,
        "url": format!("{ROOT}/0/1/attachments/1")
    })
}

/// The standard window with its attachment section replaced.
fn window_with(adds: Vec<serde_json::Value>) -> serde_json::Value {
    let mut file = change_file();
    file["edits"][0]["attachments"] = json!({
        "adds": adds,
        "updates": [],
        "deleteIds": []
    });
    file
}

/// The first state, tracking changes and publishing its generations. Well 1
/// carries the one attachment, and its global id is what a later window's edit
/// to it pairs on.
fn full_fake() -> Fake {
    Fake::new()
        .json("", tracking_root(FIRST_GEN))
        .json("/0", tracked_wells_layer())
        .json("/1", logs_table())
        .answering("/0/query", wells_full)
        .answering("/1/query", logs_pages)
        .json(
            "/0/1/attachments",
            json!({
                "attachmentInfos": [{
                    "id": 1,
                    "globalId": PHOTO,
                    "parentGlobalId": well_guid(1),
                    "name": "photo.png",
                    "contentType": "image/png",
                    "size": PHOTO_BYTES.len()
                }]
            }),
        )
        .answering("/0/1/attachments/1", |_| PHOTO_BYTES.to_vec())
        .json("/0/2/attachments", json!({ "attachmentInfos": [] }))
        .json("/0/3/attachments", json!({ "attachmentInfos": [] }))
        .json("/0/5/attachments", json!({ "attachmentInfos": [] }))
}

/// The second state, with the job routes wired: the POST answers a status URL,
/// the status answers Completed at once, and the result URL answers the change
/// file. A test that wants the job to take a while scripts its own status.
fn changed_fake() -> Fake {
    job_fake(json!({ "status": "Completed", "resultUrl": format!("{ROOT}{RESULT_URL}") }))
}

/// The second state with a scripted job status.
fn job_fake(status: serde_json::Value) -> Fake {
    changed_fake_with(status, change_file())
}

/// The second state with a scripted job status and change file.
fn changed_fake_with(status: serde_json::Value, file: serde_json::Value) -> Fake {
    changed_attachments(
        Fake::new()
            .json("", tracking_root(NEXT_GEN))
            .json("/0", tracked_wells_layer())
            .json("/1", logs_table())
            .answering("/0/query", wells_changed)
            .answering("/1/query", logs_pages)
            .json(
                "/extractChanges",
                json!({ "statusUrl": format!("{ROOT}{STATUS_URL}") }),
            )
            .json(STATUS_URL, status)
            .json(RESULT_URL, file),
    )
}

/// The second state's attachments, both ways they can be read: the listing per
/// feature a local diff walks, and the blob routes the change file's records
/// point at, which are the same routes a listing leads to. The photo on well 1
/// holds new bytes and the log on well 5 is there now, which is what the change
/// file names as edits and what a local diff has to find for itself.
fn changed_attachments(fake: Fake) -> Fake {
    fake.json(
        "/0/1/attachments",
        json!({
            "attachmentInfos": [{
                "id": 1,
                "globalId": PHOTO,
                "parentGlobalId": well_guid(1),
                "name": "photo.png",
                "contentType": "image/png",
                "size": PHOTO_AGAIN.len()
            }]
        }),
    )
    .json("/0/2/attachments", json!({ "attachmentInfos": [] }))
    .json("/0/4/attachments", json!({ "attachmentInfos": [] }))
    .json(
        "/0/5/attachments",
        json!({
            "attachmentInfos": [{
                "id": 7,
                "globalId": LOGS,
                "parentGlobalId": well_guid(5),
                "name": "logs.pdf",
                "contentType": "application/pdf",
                "size": LOGS_BYTES.len()
            }]
        }),
    )
    .answering("/0/1/attachments/1", |_| PHOTO_AGAIN.to_vec())
    .answering("/0/5/attachments/7", |_| LOGS_BYTES.to_vec())
}

/// The same second state answering a change file a test built itself.
fn changed_file_fake(file: serde_json::Value) -> Fake {
    changed_fake_with(
        json!({ "status": "Completed", "resultUrl": format!("{ROOT}{RESULT_URL}") }),
        file,
    )
}

fn extract_full(directory: &Path) -> Extraction {
    ArcgisSource::open_with(Box::new(full_fake()), ROOT)
        .expect("the fixture opens")
        .extract(directory, OPERATOR)
        .expect("the full extraction runs")
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

/// The generations an extraction recorded, as `(dataset, layer, gen)`.
fn recorded(directory: &Path) -> Vec<(String, i64, u64)> {
    let text = std::fs::read_to_string(directory.join(SERVER_GENS_FILE))
        .unwrap_or_else(|error| panic!("reading {SERVER_GENS_FILE}: {error}"));
    let held: serde_json::Value = serde_json::from_str(&text).expect("json");
    held["layers"]
        .as_array()
        .expect("the layers array")
        .iter()
        .map(|layer| {
            (
                layer["dataset"].as_str().expect("a dataset").to_string(),
                layer["layer"].as_i64().expect("a layer id"),
                layer["server_gen"].as_u64().expect("a generation"),
            )
        })
        .collect()
}

/// The one attachment the full extraction carried, by the feature id it landed
/// on.
fn attached(extraction: &Extraction) -> (String, String) {
    let held = added(&extraction.sidecar.attachments[0]);
    (held.feature_id.clone(), held.name.clone())
}

/// The attachment index a delta left behind, as `(global id, feature id, name)`.
fn attachment_index(directory: &Path) -> Vec<(String, String, String)> {
    let path = directory.join(ATTACHMENT_IDS_DIR).join("Wells.ndjson");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let held: serde_json::Value = serde_json::from_str(line).expect("an index line");
            (
                held["global_id"].as_str().expect("a global id").to_string(),
                held["feature_id"]
                    .as_str()
                    .expect("a feature id")
                    .to_string(),
                held["name"].as_str().expect("a name").to_string(),
            )
        })
        .collect()
}

/// The report rows for the point layer's attachments: the row the inventory
/// judged first, and the counts of what was carried after it, which is written
/// only where something was.
fn attachment_entries(extraction: &Extraction) -> Vec<&verne_core::LogEntry> {
    extraction
        .sidecar
        .log
        .entries
        .iter()
        .filter(|entry| {
            entry.kind == verne_core::ItemKind::EmbeddedResource && entry.location == "Wells"
        })
        .collect()
}

/// The row saying how many attachment edits were carried.
fn attachment_counts(extraction: &Extraction) -> &verne_core::LogEntry {
    let entries = attachment_entries(extraction);
    entries
        .get(1)
        .copied()
        .unwrap_or_else(|| panic!("no counts row: {entries:#?}"))
}

/// The row the inventory judged, which says whether anything was carried at all.
fn attachment_verdict(extraction: &Extraction) -> &verne_core::LogEntry {
    let entries = attachment_entries(extraction);
    entries
        .first()
        .copied()
        .unwrap_or_else(|| panic!("{:#?}", extraction.sidecar.log.entries))
}

/// The reason the report gives for the delta path that ran, off the one entry
/// the service root's change-tracking conversion writes.
fn path_entry(extraction: &Extraction) -> &verne_core::LogEntry {
    extraction
        .sidecar
        .log
        .entries
        .iter()
        .find(|entry| entry.destination.as_deref() == Some("the delta's feature files"))
        .unwrap_or_else(|| panic!("no delta path entry: {:#?}", extraction.sidecar.log.entries))
}

#[test]
fn a_service_that_publishes_generations_has_them_recorded() {
    let full = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());

    assert_eq!(
        recorded(full.path()),
        vec![
            ("Wells".to_string(), 0, FIRST_GEN),
            ("Logs".to_string(), 1, FIRST_GEN)
        ]
    );
}

/// A service that publishes no generation window leaves no cursor behind, and
/// an empty file would read as one.
#[test]
fn a_service_that_publishes_no_generations_records_no_file() {
    let full = tempfile::tempdir().expect("tempdir");
    let fake = Fake::new()
        .json("", service_root())
        .json("/0", tracked_wells_layer())
        .json("/1", logs_table())
        .answering("/0/query", wells_full)
        .answering("/1/query", logs_pages)
        .json("/0/1/attachments", json!({ "attachmentInfos": [] }))
        .json("/0/2/attachments", json!({ "attachmentInfos": [] }))
        .json("/0/3/attachments", json!({ "attachmentInfos": [] }))
        .json("/0/5/attachments", json!({ "attachmentInfos": [] }));
    ArcgisSource::open_with(Box::new(fake), ROOT)
        .expect("the fixture opens")
        .extract(full.path(), OPERATOR)
        .expect("the full extraction runs");

    assert!(!full.path().join(SERVER_GENS_FILE).exists());
}

/// The whole path in one test: the job is asked, the change file's ids are
/// fetched and nothing else is, and what lands is the same delta the local
/// diff writes.
#[test]
fn a_delta_with_recorded_generations_rides_extract_changes() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());
    let ids = minted(full.path());

    let fake = changed_fake();
    let calls = fake.calls();
    let extraction = ArcgisSource::open_with(Box::new(fake), ROOT)
        .expect("the fixture opens")
        .extract_since(delta.path(), OPERATOR, full.path())
        .expect("the delta extraction runs");

    // the job was asked about both layers, with the generations the full
    // extraction recorded and the layer list they belong to
    let calls = calls.borrow();
    let submitted = calls
        .iter()
        .find(|call| call.route == "/extractChanges")
        .expect("extractChanges was not asked");
    assert_eq!(submitted.method, "POST");
    assert_eq!(submitted.param("layers"), Some("0,1"));
    assert_eq!(
        submitted.param("layerServerGens"),
        Some(
            format!(r#"[{{"id":0,"serverGen":{FIRST_GEN}}},{{"id":1,"serverGen":{FIRST_GEN}}}]"#)
                .as_str()
        )
    );

    // only the ids the change file named were fetched: no page of the layer
    // was asked for at all
    let queried: Vec<&str> = calls
        .iter()
        .filter(|call| call.route == "/0/query" && call.param("outFields") == Some("*"))
        .map(|call| call.param("objectIds").expect("an id list"))
        .collect();
    assert_eq!(queried, vec!["1,2", "4"], "{:#?}", *calls);
    // a page walk is what the local diff does, and it did not happen: the only
    // `where` on the layer is the count the open asks for
    assert!(
        !calls
            .iter()
            .any(|call| call.route == "/0/query" && call.param("resultOffset").is_some()),
        "the whole layer was read anyway: {:#?}",
        *calls
    );

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
    assert_eq!(updates[0].feature_id, ids[&1]);
    let properties = updates[0]
        .properties
        .as_ref()
        .expect("properties ride on an update");
    // the query route's value, not the change file's: the change file is read
    // for its ids alone
    assert_eq!(properties["depth"], 13.0);
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
    assert_eq!(inserts[0].properties["depth"], 55.0);

    let deletes: Vec<_> = written
        .iter()
        .filter_map(|op| match op {
            FeatureOp::Delete(delete) => Some(delete),
            _ => None,
        })
        .collect();
    assert_eq!(deletes.len(), 1, "{written:#?}");
    assert_eq!(deletes[0].feature_id, ids[&3]);

    // 2 was in the change file and hashes the same, so it is counted rather
    // than written
    let counted = extraction.sidecar.log.entries.iter().any(|entry| {
        entry.location == "Wells" && entry.detail == "1 inserted, 1 updated, 1 deleted; 1 unchanged"
    });
    assert!(counted, "{:#?}", extraction.sidecar.log.entries);

    // the generations the window ended at, so the next delta carries on from
    // here
    assert_eq!(
        recorded(delta.path()),
        vec![
            ("Wells".to_string(), 0, NEXT_GEN),
            ("Logs".to_string(), 1, NEXT_GEN)
        ]
    );

    let entry = path_entry(&extraction);
    assert_eq!(entry.detail, "delta read from extractChanges");
    assert_eq!(entry.action, Action::Carried);

    // the report says what became of the attachment edits, counted, and names
    // the one that paired with nothing
    let attachments = attachment_counts(&extraction);
    assert_eq!(
        attachments.detail,
        "1 attachment added, 1 replaced, 0 deleted; 0 unchanged"
    );
    let Action::CarriedWithLoss { losses } = &attachments.action else {
        panic!("{attachments:#?}");
    };
    assert!(
        losses
            .iter()
            .any(|loss| loss.contains(NEVER_LOADED)
                && loss.contains("nothing was written to delete it")),
        "{losses:#?}"
    );
}

/// A running job is asked again until it finishes: the statuses a live service
/// walks through are ExportChanges, ExportAttachments and then Completed with
/// the result URL filled in.
#[test]
fn a_running_job_is_polled_until_it_answers_completed() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());

    let polls = std::cell::Cell::new(0usize);
    let fake = Fake::new()
        .json("", tracking_root(NEXT_GEN))
        .json("/0", tracked_wells_layer())
        .json("/1", logs_table())
        .answering("/0/query", wells_changed)
        .answering("/1/query", logs_pages)
        .json(
            "/extractChanges",
            json!({ "statusUrl": format!("{ROOT}{STATUS_URL}") }),
        )
        .answering(STATUS_URL, move |_| {
            polls.set(polls.get() + 1);
            let status = match polls.get() {
                1 => json!({ "status": "ExportChanges", "resultUrl": "" }),
                _ => json!({ "status": "Completed", "resultUrl": format!("{ROOT}{RESULT_URL}") }),
            };
            serde_json::to_vec(&status).expect("the status serialises")
        })
        .json(RESULT_URL, change_file())
        .answering("/0/1/attachments/1", |_| PHOTO_AGAIN.to_vec())
        .answering("/0/5/attachments/7", |_| LOGS_BYTES.to_vec());
    let calls = fake.calls();
    ArcgisSource::open_with(Box::new(fake), ROOT)
        .expect("the fixture opens")
        .extract_since(delta.path(), OPERATOR, full.path())
        .expect("the delta extraction runs");

    let asked = calls
        .borrow()
        .iter()
        .filter(|call| call.route == STATUS_URL)
        .count();
    assert_eq!(asked, 2, "the job was not polled again");
    assert_eq!(ops(delta.path(), "features/Wells.ndjson").len(), 3);
}

/// A job that fails is an error: past the submit the job is the service's own,
/// and reading the whole service instead would cost an operator a full fetch
/// they never asked for.
#[test]
fn a_failed_job_fails_the_extraction() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());

    let failed = ArcgisSource::open_with(
        Box::new(job_fake(json!({ "status": "Failed", "resultUrl": "" }))),
        ROOT,
    )
    .expect("the fixture opens")
    .extract_since(delta.path(), OPERATOR, full.path())
    .expect_err("a failed job is not a delta");
    assert!(
        matches!(&failed, ArcgisError::ChangesFailed { status, .. } if status == "Failed"),
        "{failed}"
    );
    assert!(failed.to_string().contains("Failed"), "{failed}");
}

/// A service that refuses the request answers the way it answers everything,
/// with an error object. The local diff still knows how to find the same delta,
/// so it runs, and the report says the service would not.
#[test]
fn a_refused_request_falls_back_to_the_local_diff() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());
    let ids = minted(full.path());

    let fake = changed_attachments(
        Fake::new()
            .json("", tracking_root(NEXT_GEN))
            .json("/0", tracked_wells_layer())
            .json("/1", logs_table())
            .answering("/0/query", wells_changed)
            .answering("/1/query", logs_pages)
            .json(
                "/extractChanges",
                json!({ "error": { "code": 400, "message": "Invalid Sync model type", "details": [] } }),
            ),
    );
    let calls = fake.calls();
    let extraction = ArcgisSource::open_with(Box::new(fake), ROOT)
        .expect("the fixture opens")
        .extract_since(delta.path(), OPERATOR, full.path())
        .expect("the delta falls back rather than failing");

    // the whole layer was read again, which is what the local diff does
    assert!(
        calls
            .borrow()
            .iter()
            .any(|call| call.route == "/0/query" && call.param("where").is_some()),
        "the local diff did not read the layer"
    );
    let written = ops(delta.path(), "features/Wells.ndjson");
    assert_eq!(written.len(), 3, "{written:#?}");
    assert!(
        written
            .iter()
            .any(|op| matches!(op, FeatureOp::Delete(delete) if delete.feature_id == ids[&3]))
    );

    let entry = path_entry(&extraction);
    assert_eq!(
        entry.detail,
        "delta found by reading the service again and diffing it against the previous extraction"
    );
    let Action::CarriedWithLoss { losses } = &entry.action else {
        panic!("{entry:#?}");
    };
    assert!(
        losses[0].contains("refused extractChanges")
            && losses[0].contains("Invalid Sync model type"),
        "{losses:#?}"
    );
    // a fallback delta records no cursor: it never asked for one
    assert!(!delta.path().join(SERVER_GENS_FILE).exists());
}

/// An extraction made before generations were recorded, or of a service that
/// published none, has no cursor to send back.
#[test]
fn a_previous_without_recorded_generations_falls_back_to_the_local_diff() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());
    std::fs::remove_file(full.path().join(SERVER_GENS_FILE)).expect("the recorded generations");

    let extraction = ArcgisSource::open_with(Box::new(changed_fake()), ROOT)
        .expect("the fixture opens")
        .extract_since(delta.path(), OPERATOR, full.path())
        .expect("the delta falls back rather than failing");

    let Action::CarriedWithLoss { losses } = &path_entry(&extraction).action else {
        panic!("{:#?}", path_entry(&extraction));
    };
    assert!(
        losses[0].contains("recorded no server generations"),
        "{losses:#?}"
    );
    assert_eq!(ops(delta.path(), "features/Wells.ndjson").len(), 3);
}

/// Change tracking turned off between two extractions: the cursor is stale and
/// the service would not answer for the window anyway.
#[test]
fn a_service_that_stopped_tracking_falls_back_to_the_local_diff() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());

    let fake = changed_attachments(
        Fake::new()
            .json("", service_root())
            .json("/0", tracked_wells_layer())
            .json("/1", logs_table())
            .answering("/0/query", wells_changed)
            .answering("/1/query", logs_pages),
    );
    let extraction = ArcgisSource::open_with(Box::new(fake), ROOT)
        .expect("the fixture opens")
        .extract_since(delta.path(), OPERATOR, full.path())
        .expect("the delta falls back rather than failing");

    let Action::CarriedWithLoss { losses } = &path_entry(&extraction).action else {
        panic!("{:#?}", path_entry(&extraction));
    };
    assert!(
        losses[0].contains("no longer states ChangeTracking"),
        "{losses:#?}"
    );
    assert_eq!(ops(delta.path(), "features/Wells.ndjson").len(), 3);
}

/// A deleted id the previous extraction never held has no feature to delete.
/// Writing nothing is the only honest answer, and the count says it happened.
#[test]
fn a_deleted_id_the_previous_extraction_lacks_is_dropped() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());

    let mut file = change_file();
    file["edits"][0]["features"]["deleteIds"] = json!([3, 99]);
    let extraction = ArcgisSource::open_with(Box::new(changed_file_fake(file)), ROOT)
        .expect("the fixture opens")
        .extract_since(delta.path(), OPERATOR, full.path())
        .expect("the delta extraction runs");

    let deletes = ops(delta.path(), "features/Wells.ndjson")
        .iter()
        .filter(|op| matches!(op, FeatureOp::Delete(_)))
        .count();
    assert_eq!(deletes, 1);
    let said = extraction
        .sidecar
        .log
        .entries
        .iter()
        .any(|entry| match &entry.action {
            Action::CarriedWithLoss { losses } => losses.iter().any(|loss| {
                loss.contains("1 object id was deleted") && loss.contains("holds no feature")
            }),
            _ => false,
        });
    assert!(said, "{:#?}", extraction.sidecar.log.entries);
}

// ─── Attachment edits ───────────────────────────────────────────────

/// The delta of the standard window, and the full extraction it came off.
fn attachment_delta(full: &Path, delta: &Path) -> (Extraction, Extraction) {
    let first = extract_full(full);
    let second = ArcgisSource::open_with(Box::new(changed_fake()), ROOT)
        .expect("the fixture opens")
        .extract_since(delta, OPERATOR, full)
        .expect("the delta extraction runs");
    (first, second)
}

/// An attachment added to a feature that did not itself change. Its parent is
/// named by global id and that feature is not among the rows the delta fetched,
/// so the service is asked which object id the global id belongs to, and the
/// answer is paired with the feature the earlier extraction loaded.
#[test]
fn an_attachment_added_to_an_unchanged_feature_is_carried() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    let fake = changed_fake();
    let calls = fake.calls();
    extract_full(full.path());
    let extraction = ArcgisSource::open_with(Box::new(fake), ROOT)
        .expect("the fixture opens")
        .extract_since(delta.path(), OPERATOR, full.path())
        .expect("the delta extraction runs");

    // the parent was asked for by global id, through the service's own where
    let asked: Vec<String> = calls
        .borrow()
        .iter()
        .filter_map(|call| call.param("where"))
        .filter(|clause| clause.starts_with("globalid IN"))
        .map(str::to_string)
        .collect();
    assert_eq!(asked.len(), 1, "{asked:#?}");
    assert!(asked[0].contains(&well_guid(5)), "{}", asked[0]);
    assert!(
        !asked[0].contains(&well_guid(1)),
        "a feature the delta already fetched was asked for again: {}",
        asked[0]
    );

    let add = extraction
        .sidecar
        .attachments
        .iter()
        .find_map(|op| match op {
            AttachmentOp::Add(held) => Some(held),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{:#?}", extraction.sidecar.attachments));
    assert_eq!(add.name, "logs.pdf");
    assert_eq!(add.content_type.as_deref(), Some("application/pdf"));
    assert_eq!(add.global_id.as_deref(), Some(LOGS));
    assert_eq!(add.dataset, "Wells");
    // the feature the full extraction minted for well 5, which is the only
    // place that id is written down
    assert_eq!(add.feature_id, minted(full.path())[&5]);
    assert_eq!(add.metadata["parent_global_id"], well_guid(5));
    assert_eq!(add.metadata["object_id"], "5");
    // the bytes came off the URL the change file named
    let bytes = std::fs::read(delta.path().join(&add.file)).expect("the blob");
    assert_eq!(bytes, LOGS_BYTES);
}

/// A replacement pairs with the loaded copy through the basis: the change file
/// names the attachment by global id, and the full extraction wrote that id down
/// beside the feature and the name it went up under.
#[test]
fn an_attachment_replacement_pairs_through_the_basis() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    let (first, extraction) = attachment_delta(full.path(), delta.path());
    let (feature, name) = attached(&first);

    let update = extraction
        .sidecar
        .attachments
        .iter()
        .find_map(|op| match op {
            AttachmentOp::Update(held) => Some(held),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{:#?}", extraction.sidecar.attachments));
    assert_eq!(update.global_id.as_deref(), Some(PHOTO));
    assert_eq!(update.feature_id, feature);
    // the name ptolemy holds it under, which is what will find the copy to
    // delete before the new bytes go up
    assert_eq!(update.name, name);
    assert_eq!(
        std::fs::read(delta.path().join(&update.file)).expect("the blob"),
        PHOTO_AGAIN
    );
}

/// A deleted attachment pairs the same way and carries no bytes.
#[test]
fn a_deleted_attachment_is_carried_as_a_delete() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    let first = extract_full(full.path());
    let (feature, name) = attached(&first);
    let mut file = change_file();
    file["edits"][0]["attachments"] = json!({
        "adds": [],
        "updates": [],
        "deleteIds": [PHOTO]
    });
    let extraction = ArcgisSource::open_with(Box::new(changed_file_fake(file)), ROOT)
        .expect("the fixture opens")
        .extract_since(delta.path(), OPERATOR, full.path())
        .expect("the delta extraction runs");

    let [AttachmentOp::Delete(delete)] = extraction.sidecar.attachments.as_slice() else {
        panic!("{:#?}", extraction.sidecar.attachments);
    };
    assert_eq!(delete.global_id.as_deref(), Some(PHOTO));
    assert_eq!(delete.feature_id, feature);
    assert_eq!(delete.name, name);
    assert_eq!(delete.dataset, "Wells");
    assert_eq!(
        attachment_counts(&extraction).detail,
        "0 attachments added, 0 replaced, 1 deleted; 0 unchanged"
    );
    // and it is out of the index, so the next delta of the chain does not think
    // ptolemy still holds it
    assert!(
        attachment_index(delta.path())
            .iter()
            .all(|(global_id, _, _)| global_id != PHOTO),
        "{:#?}",
        attachment_index(delta.path())
    );
}

/// An edit to an attachment nothing was loaded under cannot be placed. It is
/// counted and named, and the rest of the window still lands: a window is not
/// worth throwing away over one blob whose history verne never saw.
#[test]
fn an_unpairable_delete_is_counted_rather_than_fatal() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    let (_, extraction) = attachment_delta(full.path(), delta.path());

    let named: Vec<&AttachmentOp> = extraction
        .sidecar
        .attachments
        .iter()
        .filter(|op| op.global_id() == Some(NEVER_LOADED))
        .collect();
    assert!(named.is_empty(), "{named:#?}");
    let Action::CarriedWithLoss { losses } = &attachment_counts(&extraction).action else {
        panic!("{:#?}", attachment_counts(&extraction));
    };
    assert!(
        losses
            .iter()
            .any(|loss| loss.contains(NEVER_LOADED) && loss.contains("nothing written down")),
        "{losses:#?}"
    );
    // the window still landed: the two edits that could be placed are there
    assert_eq!(extraction.sidecar.attachments.len(), 2);
    assert_eq!(ops(delta.path(), "features/Wells.ndjson").len(), 3);
}

/// A server can report an add for an attachment ptolemy already holds: a window
/// that ends where the next one begins carries the edits on that boundary twice,
/// which ptolemy's own generation scheme does on every run and a live service
/// does at the edges. What the basis says decides, so the add is dropped, no
/// bytes are fetched, and the loaded copy is left alone: uploading it again would
/// put two attachments of one name on the feature, which is the one state the
/// loader cannot act on afterwards.
#[test]
fn an_add_the_basis_already_holds_is_skipped() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    let first = extract_full(full.path());
    let (feature, name) = attached(&first);
    let fake = changed_file_fake(window_with(vec![photo_add(PHOTO_BYTES.len())]));
    let calls = fake.calls();
    let extraction = ArcgisSource::open_with(Box::new(fake), ROOT)
        .expect("the fixture opens")
        .extract_since(delta.path(), OPERATOR, full.path())
        .expect("the delta extraction runs");

    assert!(
        extraction.sidecar.attachments.is_empty(),
        "{:#?}",
        extraction.sidecar.attachments
    );
    let asked: Vec<String> = calls
        .borrow()
        .iter()
        .map(|call| call.route.clone())
        .collect();
    assert!(
        !asked.contains(&"/0/1/attachments/1".to_string()),
        "the bytes of an attachment already loaded were fetched anyway: {asked:#?}"
    );
    // the row says the window held nothing new rather than claiming a carry
    let Action::Skipped { reason } = &attachment_verdict(&extraction).action else {
        panic!("{:#?}", attachment_verdict(&extraction));
    };
    assert!(reason.contains("1 of them, is already loaded"), "{reason}");
    // and the index still says ptolemy holds it, so the next window pairs against
    // it the same way
    assert_eq!(
        attachment_index(delta.path()),
        vec![(PHOTO.to_string(), feature, name)]
    );
    // the features of the same window still landed
    assert_eq!(ops(delta.path(), "features/Wells.ndjson").len(), 3);
}

/// The same add with a size the loaded copy does not have is new bytes for it,
/// whatever section the service put it in, so it rides as the operation the
/// loader turns into a delete and an upload rather than as a second copy.
#[test]
fn an_add_of_new_bytes_for_a_loaded_attachment_is_a_replacement() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    let first = extract_full(full.path());
    let (feature, name) = attached(&first);
    let extraction = ArcgisSource::open_with(
        Box::new(changed_file_fake(window_with(vec![photo_add(
            PHOTO_AGAIN.len(),
        )]))),
        ROOT,
    )
    .expect("the fixture opens")
    .extract_since(delta.path(), OPERATOR, full.path())
    .expect("the delta extraction runs");

    let [AttachmentOp::Update(update)] = extraction.sidecar.attachments.as_slice() else {
        panic!("{:#?}", extraction.sidecar.attachments);
    };
    assert_eq!(update.global_id.as_deref(), Some(PHOTO));
    assert_eq!(update.feature_id, feature);
    assert_eq!(update.name, name);
    assert_eq!(
        std::fs::read(delta.path().join(&update.file)).expect("the blob"),
        PHOTO_AGAIN
    );
    assert_eq!(
        attachment_counts(&extraction).detail,
        "0 attachments added, 1 replaced, 0 deleted; 0 unchanged"
    );
}

/// A repeated add does not cost the window the rest of it: the add of something
/// nothing was ever loaded for still lands, and the report counts both.
#[test]
fn a_new_add_beside_a_repeated_one_still_lands() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());
    let extraction = ArcgisSource::open_with(
        Box::new(changed_file_fake(window_with(vec![
            photo_add(PHOTO_BYTES.len()),
            logs_add(),
        ]))),
        ROOT,
    )
    .expect("the fixture opens")
    .extract_since(delta.path(), OPERATOR, full.path())
    .expect("the delta extraction runs");

    let [AttachmentOp::Add(add)] = extraction.sidecar.attachments.as_slice() else {
        panic!("{:#?}", extraction.sidecar.attachments);
    };
    assert_eq!(add.global_id.as_deref(), Some(LOGS));
    assert_eq!(add.feature_id, minted(full.path())[&5]);
    assert_eq!(
        std::fs::read(delta.path().join(&add.file)).expect("the blob"),
        LOGS_BYTES
    );
    assert_eq!(
        attachment_counts(&extraction).detail,
        "1 attachment added, 0 replaced, 0 deleted; 1 unchanged"
    );
}

/// A layer with no global id column cannot have an attachment edit paired with
/// anything: the change file names every one of them by global id. The edits are
/// reported and skipped, and the features still come across.
#[test]
fn a_layer_without_a_global_id_field_skips_its_attachment_edits() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    // the layer as it is everywhere else in the tests: no globalIdField
    let bare = |root: serde_json::Value| {
        Fake::new()
            .json("", root)
            .json("/0", common::wells_layer())
            .json("/1", logs_table())
            .answering("/0/query", wells_full)
            .answering("/1/query", logs_pages)
            .json("/0/1/attachments", json!({ "attachmentInfos": [] }))
            .json("/0/2/attachments", json!({ "attachmentInfos": [] }))
            .json("/0/3/attachments", json!({ "attachmentInfos": [] }))
            .json("/0/5/attachments", json!({ "attachmentInfos": [] }))
    };
    ArcgisSource::open_with(Box::new(bare(tracking_root(FIRST_GEN))), ROOT)
        .expect("the fixture opens")
        .extract(full.path(), OPERATOR)
        .expect("the full extraction runs");

    let changed = Fake::new()
        .json("", tracking_root(NEXT_GEN))
        .json("/0", common::wells_layer())
        .json("/1", logs_table())
        .answering("/0/query", wells_changed)
        .answering("/1/query", logs_pages)
        .json(
            "/extractChanges",
            json!({ "statusUrl": format!("{ROOT}{STATUS_URL}") }),
        )
        .json(
            STATUS_URL,
            json!({ "status": "Completed", "resultUrl": format!("{ROOT}{RESULT_URL}") }),
        )
        .json(RESULT_URL, change_file());
    let extraction = ArcgisSource::open_with(Box::new(changed), ROOT)
        .expect("the fixture opens")
        .extract_since(delta.path(), OPERATOR, full.path())
        .expect("the delta extraction runs");

    assert!(
        extraction.sidecar.attachments.is_empty(),
        "{:#?}",
        extraction.sidecar.attachments
    );
    let Action::Skipped { reason } = &attachment_verdict(&extraction).action else {
        panic!("{:#?}", attachment_verdict(&extraction));
    };
    assert!(
        reason.contains("declares no global id field") && reason.contains("3 edits"),
        "{reason}"
    );
    assert_eq!(ops(delta.path(), "features/Wells.ndjson").len(), 3);
}

/// A local diff was told nothing about the attachments, so it lists them and
/// diffs them itself, and reaches the same two operations the change file named:
/// the photo on well 1 is not the size it was loaded at, and the log on well 5 is
/// there now. Both pair by global id, which is what the listing and the previous
/// extraction's sidecar have in common.
#[test]
fn a_local_diff_diffs_the_attachments_it_lists() {
    let full = tempfile::tempdir().expect("tempdir");
    let delta = tempfile::tempdir().expect("tempdir");
    let first = extract_full(full.path());
    let (feature, name) = attached(&first);
    std::fs::remove_file(full.path().join(SERVER_GENS_FILE)).expect("the recorded generations");
    let extraction = ArcgisSource::open_with(Box::new(changed_fake()), ROOT)
        .expect("the fixture opens")
        .extract_since(delta.path(), OPERATOR, full.path())
        .expect("the delta runs");

    let update = extraction
        .sidecar
        .attachments
        .iter()
        .find_map(|op| match op {
            AttachmentOp::Update(held) => Some(held),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{:#?}", extraction.sidecar.attachments));
    assert_eq!(update.global_id.as_deref(), Some(PHOTO));
    assert_eq!(update.feature_id, feature);
    assert_eq!(update.name, name);
    assert_eq!(
        std::fs::read(delta.path().join(&update.file)).expect("the blob"),
        PHOTO_AGAIN
    );

    let add = extraction
        .sidecar
        .attachments
        .iter()
        .find_map(|op| match op {
            AttachmentOp::Add(held) => Some(held),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{:#?}", extraction.sidecar.attachments));
    assert_eq!(add.global_id.as_deref(), Some(LOGS));
    assert_eq!(add.name, "logs.pdf");
    assert_eq!(add.feature_id, minted(full.path())[&5]);
    // the object id and the size ride in the metadata, which is what the next
    // local diff pairs on when the service keeps no global ids
    assert_eq!(add.metadata["object_id"], "5");
    assert_eq!(add.metadata["size"], LOGS_BYTES.len());
    assert_eq!(
        std::fs::read(delta.path().join(&add.file)).expect("the blob"),
        LOGS_BYTES
    );

    assert_eq!(
        attachment_counts(&extraction).detail,
        "1 attachment added, 1 replaced, 0 deleted; 0 unchanged"
    );
    assert_eq!(attachment_counts(&extraction).action, Action::Carried);
    // and the index the delta leaves behind says what ptolemy holds, so a later
    // run that can ride extractChanges pairs its edits against it
    let wells = minted(full.path());
    assert_eq!(
        attachment_index(delta.path()),
        vec![
            (
                PHOTO.to_string(),
                wells[&1].clone(),
                "photo.png".to_string()
            ),
            (LOGS.to_string(), wells[&5].clone(), "logs.pdf".to_string()),
        ]
    );
}

// ─── Chained deltas ─────────────────────────────────────────────────

/// The third state: 1 got deeper again, and 4, which the first delta inserted,
/// is gone. Nothing else moved, and nothing else is asked for.
fn later_row(oid: i64) -> serde_json::Value {
    match oid {
        1 => row(1, 14.0, Some(DRILLED), 3.5, 50.1),
        other => panic!(
            "the extraction asked for object id {other}, which the third state does not change"
        ),
    }
}

fn wells_later(params: &[(&str, String)]) -> Vec<u8> {
    if param(params, "returnCountOnly").is_some() {
        return serde_json::to_vec(&json!({ "count": 4 })).expect("the count serialises");
    }
    if let Some(clause) = pairing(params) {
        return by_global_id(&clause);
    }
    let oids = param(params, "objectIds").expect("the third state is only asked by object id");
    if param(params, "outFields") != Some("*") {
        return native_page(oids);
    }
    let rows: Vec<serde_json::Value> = oids
        .split(',')
        .map(|oid| later_row(oid.parse().expect("an object id")))
        .collect();
    serde_json::to_vec(&json!({ "objectIdFieldName": "objectid", "features": rows }))
        .expect("the page serialises")
}

/// The second window's change file: 1 edited again, 4 deleted, and the
/// attachment the first delta added deleted with it.
fn later_change_file() -> serde_json::Value {
    json!({
        "edits": [
            {
                "id": 0,
                "features": {
                    "adds": [],
                    "updates": [{
                        "attributes": { "objectid": 1, "globalid": well_guid(1), "status": 1, "depth": 999.0, "drilled": DRILLED },
                        "geometry": { "x": 3.5, "y": 50.1 }
                    }],
                    "deleteIds": [4]
                },
                "attachments": { "adds": [], "updates": [], "deleteIds": [LOGS] }
            },
            { "id": 1, "features": { "adds": [], "updates": [], "deleteIds": [] } }
        ],
        "layerServerGens": [
            { "id": 0, "serverGen": LATER_GEN },
            { "id": 1, "serverGen": LATER_GEN }
        ]
    })
}

fn later_fake() -> Fake {
    Fake::new()
        .json("", tracking_root(LATER_GEN))
        .json("/0", tracked_wells_layer())
        .json("/1", logs_table())
        .answering("/0/query", wells_later)
        .answering("/1/query", logs_pages)
        .json(
            "/extractChanges",
            json!({ "statusUrl": format!("{ROOT}{STATUS_URL}") }),
        )
        .json(
            STATUS_URL,
            json!({ "status": "Completed", "resultUrl": format!("{ROOT}{RESULT_URL}") }),
        )
        .json(RESULT_URL, later_change_file())
}

/// The feature id each object id was inserted under by a delta, off its
/// operation file.
fn inserted(directory: &Path) -> BTreeMap<i64, String> {
    ops(directory, "features/Wells.ndjson")
        .into_iter()
        .filter_map(|op| match op {
            FeatureOp::Insert(insert) => Some((
                insert.properties["objectid"].as_i64().expect("an oid"),
                insert.feature_id,
            )),
            _ => None,
        })
        .collect()
}

/// full, then a delta, then a delta of that delta.
fn chain(full: &Path, first: &Path, second: &Path) -> Extraction {
    extract_full(full);
    ArcgisSource::open_with(Box::new(changed_fake()), ROOT)
        .expect("the fixture opens")
        .extract_since(first, OPERATOR, full)
        .expect("the first delta runs");
    ArcgisSource::open_with(Box::new(later_fake()), ROOT)
        .expect("the fixture opens")
        .extract_since(second, OPERATOR, first)
        .expect("the second delta runs")
}

/// A row edited in two windows running is one feature throughout. The second
/// delta pairs it through the index the first wrote, which is the only place
/// the feature id of a row the first delta did not touch is written down.
#[test]
fn a_row_edited_twice_stays_one_feature_down_the_chain() {
    let full = tempfile::tempdir().expect("tempdir");
    let first = tempfile::tempdir().expect("tempdir");
    let second = tempfile::tempdir().expect("tempdir");
    let extraction = chain(full.path(), first.path(), second.path());
    let ids = minted(full.path());

    let written = ops(second.path(), "features/Wells.ndjson");
    let updates: Vec<_> = written
        .iter()
        .filter_map(|op| match op {
            FeatureOp::Update(update) => Some(update),
            _ => None,
        })
        .collect();
    assert_eq!(updates.len(), 1, "{written:#?}");
    assert_eq!(
        updates[0].feature_id, ids[&1],
        "the row was paired with something other than the feature the full extraction minted"
    );
    assert_eq!(
        updates[0]
            .properties
            .as_ref()
            .expect("properties ride on an update")["depth"],
        14.0
    );
    assert!(
        !written.iter().any(|op| matches!(op, FeatureOp::Insert(_))),
        "a row already in ptolemy came back as a second copy of itself: {written:#?}"
    );
    assert_eq!(
        path_entry(&extraction).detail,
        "delta read from extractChanges"
    );
    // the chain carries on: the cursor moved and the index was written again
    assert_eq!(
        recorded(second.path()),
        vec![
            ("Wells".to_string(), 0, LATER_GEN),
            ("Logs".to_string(), 1, LATER_GEN)
        ]
    );
}

/// A delete of a row an earlier delta inserted resolves to the feature id that
/// delta minted, which is again only in its index.
#[test]
fn a_delete_of_a_row_an_earlier_delta_inserted_resolves() {
    let full = tempfile::tempdir().expect("tempdir");
    let first = tempfile::tempdir().expect("tempdir");
    let second = tempfile::tempdir().expect("tempdir");
    chain(full.path(), first.path(), second.path());
    let by_first = inserted(first.path());

    let deletes: Vec<_> = ops(second.path(), "features/Wells.ndjson")
        .into_iter()
        .filter_map(|op| match op {
            FeatureOp::Delete(delete) => Some(delete.feature_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        deletes,
        vec![by_first[&4].clone()],
        "the delete did not name the feature the first delta inserted"
    );
    // and the index the second delta wrote carries the chain on: the row it
    // deleted is out of it, and a row neither delta ever touched is still in it
    // under the feature id the full extraction minted, which is the whole
    // reason the file exists
    let held = std::fs::read_to_string(second.path().join(OBJECT_IDS_DIR).join("Wells.ndjson"))
        .expect("the index");
    let ids = minted(full.path());
    assert!(!held.contains(&by_first[&4]), "{held}");
    assert!(held.contains(&ids[&2]), "{held}");
    assert!(held.contains(&ids[&1]), "{held}");
}

/// An attachment one delta created and the next one deleted pairs through the
/// attachment index the first delta wrote, which is the only place the feature it
/// went onto and the name it went up under are written down: the first delta's
/// sidecar says so too, but the second is not read against a sidecar.
#[test]
fn a_chained_delta_pairs_an_attachment_the_previous_delta_created() {
    let full = tempfile::tempdir().expect("tempdir");
    let first = tempfile::tempdir().expect("tempdir");
    let second = tempfile::tempdir().expect("tempdir");
    let extraction = chain(full.path(), first.path(), second.path());
    let wells = minted(full.path());

    // the first delta wrote both attachments down: the one it added, and the one
    // the full extraction loaded and it replaced
    let held = attachment_index(first.path());
    assert_eq!(
        held,
        vec![
            (
                PHOTO.to_string(),
                wells[&1].clone(),
                "photo.png".to_string()
            ),
            (LOGS.to_string(), wells[&5].clone(), "logs.pdf".to_string()),
        ]
    );

    // it also wrote down what the bytes are now, which is what lets the next
    // window tell an add it has already applied from a real one
    let rows = std::fs::read_to_string(first.path().join(ATTACHMENT_IDS_DIR).join("Wells.ndjson"))
        .expect("the index");
    assert!(
        rows.contains(&format!(r#""size":{}"#, PHOTO_AGAIN.len())),
        "{rows}"
    );

    let [AttachmentOp::Delete(delete)] = extraction.sidecar.attachments.as_slice() else {
        panic!("{:#?}", extraction.sidecar.attachments);
    };
    assert_eq!(delete.global_id.as_deref(), Some(LOGS));
    assert_eq!(
        delete.feature_id, wells[&5],
        "the delete did not name the feature the first delta attached it to"
    );
    assert_eq!(delete.name, "logs.pdf");
    // the chain carries on: what the window took out is gone from the index and
    // the attachment neither window touched is still in it
    assert_eq!(
        attachment_index(second.path()),
        vec![(
            PHOTO.to_string(),
            wells[&1].clone(),
            "photo.png".to_string()
        )]
    );
}

/// A delta written before the index existed has no index, and pairing against
/// its feature files would read every row it left alone as new. It is refused
/// by name, and nothing is asked of the service first.
#[test]
fn a_delta_without_an_index_is_refused() {
    let full = tempfile::tempdir().expect("tempdir");
    let first = tempfile::tempdir().expect("tempdir");
    let second = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());
    ArcgisSource::open_with(Box::new(changed_fake()), ROOT)
        .expect("the fixture opens")
        .extract_since(first.path(), OPERATOR, full.path())
        .expect("the first delta runs");
    std::fs::remove_dir_all(first.path().join(OBJECT_IDS_DIR)).expect("the index");

    let fake = later_fake();
    let calls = fake.calls();
    let refused = ArcgisSource::open_with(Box::new(fake), ROOT)
        .expect("the fixture opens")
        .extract_since(second.path(), OPERATOR, first.path())
        .expect_err("a delta with no index is not a basis");
    assert!(
        matches!(&refused, ArcgisError::DeltaPrevious { reason, .. } if reason.contains(OBJECT_IDS_DIR)),
        "{refused}"
    );
    assert!(
        refused.to_string().contains("Wells"),
        "the refusal does not name the dataset: {refused}"
    );
    assert!(
        !calls
            .borrow()
            .iter()
            .any(|call| call.route == "/extractChanges"),
        "a run that cannot be paired started a job on the server anyway"
    );
}

/// The local diff still refuses a delta as a basis: its feature files hold only
/// what changed, so every row it left alone would read as vanished. Here the
/// index is there and it is the service that has stopped tracking changes.
#[test]
fn a_delta_is_refused_where_the_local_diff_would_run() {
    let full = tempfile::tempdir().expect("tempdir");
    let first = tempfile::tempdir().expect("tempdir");
    let second = tempfile::tempdir().expect("tempdir");
    extract_full(full.path());
    ArcgisSource::open_with(Box::new(changed_fake()), ROOT)
        .expect("the fixture opens")
        .extract_since(first.path(), OPERATOR, full.path())
        .expect("the first delta runs");

    let untracked = Fake::new()
        .json("", service_root())
        .json("/0", tracked_wells_layer())
        .json("/1", logs_table())
        .answering("/0/query", wells_later)
        .answering("/1/query", logs_pages);
    let refused = ArcgisSource::open_with(Box::new(untracked), ROOT)
        .expect("the fixture opens")
        .extract_since(second.path(), OPERATOR, first.path())
        .expect_err("a local diff of a delta is not a delta");
    assert!(
        matches!(&refused, ArcgisError::DeltaPrevious { reason, .. } if reason.contains("read every row that delta left alone as vanished")),
        "{refused}"
    );
}

// ─── The result file's redirect ──────────────────────────────────────

/// One request a socket saw, and whether the token came with it.
struct Seen {
    target: String,
    authorization: Option<String>,
}

/// A socket that answers one request with `response` and hands back what it
/// saw.
fn serve(listener: TcpListener, response: String) -> JoinHandle<Seen> {
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("a connection");
        let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("a request line");
        let target = line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();
        let mut authorization = None;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).expect("a header");
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((name, value)) = header.split_once(':')
                && name.eq_ignore_ascii_case("x-esri-authorization")
            {
                authorization = Some(value.trim().to_string());
            }
        }
        stream.write_all(response.as_bytes()).expect("an answer");
        stream.flush().expect("a flushed answer");
        Seen {
            target,
            authorization,
        }
    })
}

/// A result URL answers with a redirect to a signed file on storage, and the
/// token must not go there. reqwest drops `Authorization` across a host
/// boundary and leaves a header it does not know alone, and the token rides in
/// `X-Esri-Authorization`, so this runs against sockets: what is under test is
/// which headers leave the machine.
///
/// Two ports on the loopback address are two origins by the same rule reqwest
/// applies, which is what makes the second request the cross-host one.
#[test]
fn the_signed_url_a_result_redirects_to_is_fetched_without_the_token() {
    let pointer = TcpListener::bind("127.0.0.1:0").expect("a port");
    let storage = TcpListener::bind("127.0.0.1:0").expect("a port");
    let pointer_url = format!(
        "http://{}/jobs/j1/result",
        pointer.local_addr().expect("the bound address")
    );
    let signed = format!(
        "http://{}/blob/changes.json?sig=not-a-real-signature",
        storage.local_addr().expect("the bound address")
    );
    // a live service answers with a 302 and an HTML body nothing reads
    let moved = "<html><head><title>Object moved</title></head><body></body></html>";
    let redirect = format!(
        "HTTP/1.1 302 Found\r\nLocation: {signed}\r\ncontent-type: text/html\r\ncontent-length: \
         {}\r\nconnection: close\r\n\r\n{moved}",
        moved.len()
    );
    let file = r#"{"edits":[],"layerServerGens":[{"id":0,"serverGen":7}]}"#;
    let answer = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: \
         close\r\n\r\n{file}",
        file.len()
    );
    let pointer = serve(pointer, redirect);
    let storage = serve(storage, answer);

    let fetch = HttpFetch::new(Credentials::Token("held-token".to_string())).expect("a client");
    let bytes = fetch.get_file(&pointer_url).expect("the change file");
    assert_eq!(String::from_utf8_lossy(&bytes), file);

    let pointer = pointer.join().expect("the pointer thread");
    let storage = storage.join().expect("the storage thread");
    assert_eq!(
        pointer.authorization.as_deref(),
        Some("Bearer held-token"),
        "the service's own route did not carry the token"
    );
    assert_eq!(
        storage.authorization, None,
        "the token followed the redirect to storage"
    );
    assert!(
        storage.target.starts_with("/blob/changes.json"),
        "the signed URL was not the one fetched: {}",
        storage.target
    );
}
