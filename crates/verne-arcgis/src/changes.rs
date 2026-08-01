//! What the service itself says changed: `extractChanges`.
//!
//! A service that tracks changes hands out a generation number per layer, and
//! a full extraction records the ones it read at beside its sidecar. A later
//! `--since` sends them back, and the service answers with the rows edited
//! since, which is a fetch of what changed rather than of the whole service.
//!
//! The job is asynchronous, and every step of it was checked against a live
//! service: the POST answers with a status URL, the status answers `Completed`
//! with a result URL, and the result URL redirects to a signed file on storage
//! that is not the service, which is why the client follows that redirect by
//! hand.
//!
//! Of the features in that file only the object ids are used. The features
//! themselves come down `/query` the way every other extraction reads them, so
//! the date rewriting, the reference the geometry arrives in, the untransformed
//! originals and the feature file's format all stay on one code path. The
//! attachment records are read whole: the bytes are behind the URL each one
//! names, and no other route would list the same window again.
//!
//! `returnIdsOnly=true` is not asked for: on a live service it answers with
//! empty edits for windows the async job returns thousands of rows for, so the
//! change file is the only source that can be trusted.
//!
//! A delta on this path also writes down what ptolemy now holds, one line per
//! row per dataset. Its own feature files hold only the rows it touched, so
//! nothing else in them could tell the next delta which feature id an object id
//! belongs to, and without that a row edited two windows running would come
//! back as a second copy of itself. The attachments get a second index for the
//! same reason, keyed by the global id a change file names them by.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use verne_core::safe_file_name;

use crate::ArcgisError;
use crate::client::{Fetch, json, json_post, parse};
use crate::service::text;

/// The generations an extraction can be continued from, written beside the
/// sidecar and not into it: the loader reads the sidecar, and a cursor into
/// one service is nothing to it.
pub const SERVER_GENS_FILE: &str = "server-gens.json";

/// Where a delta writes down what ptolemy holds, one file per dataset.
pub const OBJECT_IDS_DIR: &str = "object-ids";

/// The same for the attachments, one file per dataset. A change file names an
/// attachment edit by the service's `globalId`, and ptolemy knows nothing about
/// one: this is where a delta writes which feature holds each attachment and
/// under what name, so the next delta of the chain can pair an edit to it.
pub const ATTACHMENT_IDS_DIR: &str = "attachment-ids";

/// How often a running job is asked whether it is done. A hundred thousand
/// edits took about a minute on a live service, so nothing is gained by
/// asking faster.
const POLL_EVERY: Duration = Duration::from_secs(3);

/// How long a job is waited for before the extraction gives up on it.
const POLL_FOR: Duration = Duration::from_secs(600);

/// One layer's generation, in the shape the service both publishes it in and
/// takes it back in.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerGen {
    pub id: i64,
    pub server_gen: u64,
}

/// The generations one extraction left behind, keyed by the dataset each layer
/// became as well as by the layer id: a delta names datasets and the service
/// names layer ids, so the file has to say both.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedGens {
    /// The service these are a cursor into, so the file says what it is of.
    pub service: String,
    pub layers: Vec<RecordedGen>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedGen {
    pub dataset: String,
    pub layer: i64,
    pub server_gen: u64,
}

impl RecordedGens {
    pub fn of_layer(&self, layer: i64) -> Option<u64> {
        self.layers
            .iter()
            .find(|held| held.layer == layer)
            .map(|held| held.server_gen)
    }
}

/// The generations recorded in an extraction directory, or `None` where the
/// extraction recorded none: a service that publishes no generation window
/// leaves no file, and neither does a delta that was diffed locally.
pub fn read(directory: &Path) -> Result<Option<RecordedGens>, ArcgisError> {
    let path = directory.join(SERVER_GENS_FILE);
    let held = match std::fs::read_to_string(&path) {
        Ok(held) => held,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ArcgisError::Read {
                path: path.display().to_string(),
                source,
            });
        }
    };
    serde_json::from_str(&held)
        .map(Some)
        .map_err(|error| ArcgisError::BadPrevious {
            path: path.display().to_string(),
            message: error.to_string(),
        })
}

pub fn write(directory: &Path, gens: &RecordedGens) -> Result<(), ArcgisError> {
    let path = directory.join(SERVER_GENS_FILE);
    let held = serde_json::to_string_pretty(gens).expect("generations hold only text and numbers");
    std::fs::write(&path, held + "\n").map_err(|source| ArcgisError::Write {
        path: path.display().to_string(),
        source,
    })
}

/// One row of a dataset as the index holds it: the object id the service knows
/// it by, the feature id ptolemy holds it under, and a hash of what was last
/// written for it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Indexed {
    pub oid: String,
    pub feature_id: String,
    pub hash: u64,
}

pub fn index_path(directory: &Path, dataset: &str) -> PathBuf {
    directory
        .join(OBJECT_IDS_DIR)
        .join(format!("{}.ndjson", safe_file_name(dataset)))
}

/// One dataset's index, as the delta that wrote it left it.
pub fn read_index(directory: &Path, dataset: &str) -> Result<Vec<Indexed>, ArcgisError> {
    let path = index_path(directory, dataset);
    let held = std::fs::read_to_string(&path).map_err(|source| ArcgisError::Read {
        path: path.display().to_string(),
        source,
    })?;
    let mut rows = Vec::new();
    for (number, line) in held.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        rows.push(
            serde_json::from_str(line).map_err(|error| ArcgisError::BadPrevious {
                path: path.display().to_string(),
                message: format!("line {}: {error}", number + 1),
            })?,
        );
    }
    Ok(rows)
}

/// Write one dataset's index. A line per row rather than one JSON document,
/// because a dataset holds as many rows as the service does and this file is
/// read a line at a time.
pub fn write_index(
    directory: &Path,
    dataset: &str,
    rows: &BTreeMap<String, (String, u64)>,
) -> Result<(), ArcgisError> {
    write_lines(
        &index_path(directory, dataset),
        rows.iter().map(|(oid, (feature_id, hash))| {
            serde_json::to_string(&Indexed {
                oid: oid.clone(),
                feature_id: feature_id.clone(),
                hash: *hash,
            })
            .expect("an index row holds only text and a number")
        }),
    )
}

/// An index file, a line at a time, with the directory made if it is not there.
fn write_lines(path: &Path, lines: impl Iterator<Item = String>) -> Result<(), ArcgisError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ArcgisError::Write {
            path: parent.display().to_string(),
            source,
        })?;
    }
    let file = std::fs::File::create(path).map_err(|source| ArcgisError::Write {
        path: path.display().to_string(),
        source,
    })?;
    use std::io::Write;
    let mut out = std::io::BufWriter::new(file);
    for line in lines {
        writeln!(out, "{line}").map_err(|source| ArcgisError::Write {
            path: path.display().to_string(),
            source,
        })?;
    }
    out.flush().map_err(|source| ArcgisError::Write {
        path: path.display().to_string(),
        source,
    })
}

/// One attachment as the index holds it: the id the service knows it by, the
/// feature ptolemy hangs it off, and the name it was uploaded under, which is
/// what the loader matches to find it again.
///
/// The last three are what the service said about the blob when it was loaded.
/// They are what lets the next delta tell an edit it has already applied from a
/// real one without fetching the bytes to look, and the object id is the only
/// handle it has where the service keeps no attachment global ids. All optional:
/// an index written before they existed says nothing about the bytes, and a run
/// reading one takes that for what it is.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexedAttachment {
    pub global_id: String,
    pub feature_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oid: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

pub fn attachment_index_path(directory: &Path, dataset: &str) -> PathBuf {
    directory
        .join(ATTACHMENT_IDS_DIR)
        .join(format!("{}.ndjson", safe_file_name(dataset)))
}

/// One dataset's attachment index, empty where the delta wrote none. Missing is
/// not an error the way a missing object id index is: a delta written before
/// attachments were carried has none, and an attachment edit that cannot be
/// paired is counted rather than fatal.
pub fn read_attachment_index(
    directory: &Path,
    dataset: &str,
) -> Result<Vec<IndexedAttachment>, ArcgisError> {
    let path = attachment_index_path(directory, dataset);
    let held = match std::fs::read_to_string(&path) {
        Ok(held) => held,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ArcgisError::Read {
                path: path.display().to_string(),
                source,
            });
        }
    };
    let mut rows = Vec::new();
    for (number, line) in held.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        rows.push(
            serde_json::from_str(line).map_err(|error| ArcgisError::BadPrevious {
                path: path.display().to_string(),
                message: format!("line {}: {error}", number + 1),
            })?,
        );
    }
    Ok(rows)
}

/// Write one dataset's attachment index, a line per attachment, in the order the
/// caller holds them, which is by the global id each carries.
pub fn write_attachment_index(
    directory: &Path,
    dataset: &str,
    rows: &[IndexedAttachment],
) -> Result<(), ArcgisError> {
    write_lines(
        &attachment_index_path(directory, dataset),
        rows.iter().map(|row| {
            serde_json::to_string(row).expect("an index row holds only text and a number")
        }),
    )
}

/// Ask the service what changed since `gens`, and get back the URL of the job
/// it started.
///
/// The layer list and the ids in the generations must match exactly: a service
/// answers a mismatch by refusing the whole request as an invalid sync model
/// rather than by naming the layer that was wrong.
pub fn submit(fetch: &dyn Fetch, url: &str, gens: &[LayerGen]) -> Result<String, ArcgisError> {
    let route = format!("{url}/extractChanges");
    let layers = gens
        .iter()
        .map(|held| held.id.to_string())
        .collect::<Vec<String>>()
        .join(",");
    let params = vec![
        ("layers", layers),
        (
            "layerServerGens",
            serde_json::to_string(gens).expect("generations hold only numbers"),
        ),
    ];
    let value = json_post(fetch, &route, &params)?;
    value
        .get("statusUrl")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| ArcgisError::BadShape {
            route,
            message: "extractChanges named no statusUrl, and the change file is only reachable \
                      through the job it names"
                .into(),
        })
}

/// Wait for the job to finish and read the change file it wrote.
pub fn collect(fetch: &dyn Fetch, status_url: &str) -> Result<ChangeFile, ArcgisError> {
    let result_url = wait(fetch, status_url)?;
    let bytes = fetch.get_file(&result_url)?;
    let value = parse(&result_url, &bytes)?;
    serde_json::from_value(value).map_err(|error| ArcgisError::BadShape {
        route: result_url,
        message: error.to_string(),
    })
}

/// Poll the job to `Completed` and hand back the result URL it names. A
/// `Completed` with an empty `resultUrl` is a job still writing its file, so
/// it is waited on like any other unfinished status.
fn wait(fetch: &dyn Fetch, status_url: &str) -> Result<String, ArcgisError> {
    let deadline = Instant::now() + POLL_FOR;
    loop {
        let value = json(fetch, status_url, &[])?;
        let status = value
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let result = value
            .get("resultUrl")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match status {
            "Completed" if !result.is_empty() => return Ok(result.to_string()),
            "Failed" | "CompletedWithErrors" => {
                return Err(ArcgisError::ChangesFailed {
                    route: status_url.to_string(),
                    status: status.to_string(),
                });
            }
            _ => {}
        }
        if Instant::now() + POLL_EVERY >= deadline {
            return Err(ArcgisError::ChangesTimedOut {
                route: status_url.to_string(),
                minutes: POLL_FOR.as_secs() / 60,
            });
        }
        std::thread::sleep(POLL_EVERY);
    }
}

/// The change file, read for the ids it names and the generations it ends at.
/// The features in it are not converted: they are fetched again through the
/// query route every other pass uses.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangeFile {
    #[serde(default)]
    edits: Vec<RawEdits>,
    /// Where the window ends, which the delta records as its own cursor.
    #[serde(default)]
    pub layer_server_gens: Vec<LayerGen>,
}

/// One layer's section of the change file. Both edit sets are optional and
/// nullable: a layer with no attachments states none.
#[derive(Debug, Deserialize)]
struct RawEdits {
    id: i64,
    #[serde(default)]
    features: Option<RawEditSet>,
    #[serde(default)]
    attachments: Option<RawAttachmentEdits>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEditSet {
    #[serde(default)]
    adds: Vec<serde_json::Value>,
    #[serde(default)]
    updates: Vec<serde_json::Value>,
    #[serde(default)]
    delete_ids: Vec<serde_json::Value>,
}

/// The attachment half, which unlike the feature half is read for what is in
/// it: an attachment's bytes are behind the URL each record names, and there is
/// no second route that would list the same window again.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAttachmentEdits {
    #[serde(default)]
    adds: Vec<AttachmentRecord>,
    #[serde(default)]
    updates: Vec<AttachmentRecord>,
    /// Attachment global ids, not the numeric ids the same file's `add`
    /// records carry as `attachmentId`.
    #[serde(default)]
    delete_ids: Vec<serde_json::Value>,
}

/// One added or updated attachment, as a live service writes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRecord {
    #[serde(default)]
    pub attachment_id: Option<i64>,
    /// The service's own id for the attachment, a GUID in braces. What pairs
    /// this edit with the copy an earlier extraction loaded.
    #[serde(default)]
    pub global_id: Option<String>,
    /// The feature it hangs off, by the value of the layer's globalIdField
    /// rather than by object id.
    #[serde(default)]
    pub parent_global_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    /// Where the bytes are: absolute, on the service's own host, and carrying
    /// no signature, so the token rides as it does on every other request.
    #[serde(default)]
    pub url: Option<String>,
}

/// What the service says changed on one layer.
#[derive(Debug, Default)]
pub struct LayerChanges {
    /// The object ids of the rows the service added or updated. Which of the
    /// two a row is gets decided by the previous extraction, not by the
    /// section the service put it in.
    pub touched: Vec<String>,
    pub deleted: Vec<String>,
    pub attachments: AttachmentEdits,
}

/// The attachment edits the service named in the window, which a delta carries:
/// the bytes of an add or an update come off the URL its record names, and a
/// delete names the global id of what is gone.
#[derive(Debug, Default, Clone)]
pub struct AttachmentEdits {
    pub adds: Vec<AttachmentRecord>,
    pub updates: Vec<AttachmentRecord>,
    pub deleted: Vec<String>,
}

impl ChangeFile {
    /// One layer's changes, by the object id field that layer names now. A
    /// layer the file says nothing about changed in no way.
    pub fn layer(&self, id: i64, oid_field: &str) -> LayerChanges {
        let Some(edits) = self.edits.iter().find(|held| held.id == id) else {
            return LayerChanges::default();
        };
        let nothing = RawEditSet::default();
        let no_attachments = RawAttachmentEdits::default();
        let features = edits.features.as_ref().unwrap_or(&nothing);
        let attachments = edits.attachments.as_ref().unwrap_or(&no_attachments);
        // a set, so an id in both sections is fetched once
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for row in features.adds.iter().chain(features.updates.iter()) {
            if let Some(oid) = row
                .get("attributes")
                .and_then(|attributes| attributes.get(oid_field))
            {
                touched.insert(text(oid));
            }
        }
        LayerChanges {
            touched: touched.into_iter().collect(),
            deleted: features.delete_ids.iter().map(text).collect(),
            attachments: AttachmentEdits {
                adds: attachments.adds.clone(),
                updates: attachments.updates.clone(),
                deleted: attachments.delete_ids.iter().map(text).collect(),
            },
        }
    }

    /// The generations the window ended at, by layer id.
    pub fn gens(&self) -> BTreeMap<i64, u64> {
        self.layer_server_gens
            .iter()
            .map(|held| (held.id, held.server_gen))
            .collect()
    }
}
