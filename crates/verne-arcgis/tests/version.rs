//! Reading a named geodatabase version: `gdbVersion` must ride on every
//! query, and a wrong name must fail the open rather than read as an empty
//! service. Checked live against a versioned service: `SDE.DEFAULT` answers
//! and an unknown name comes back as an Esri error object.

mod common;

use common::{Fake, ROOT};
use serde_json::json;
use verne_arcgis::{ArcgisError, ArcgisSource};
use verne_core::Source;

const VERSION: &str = "SDE.DEFAULT";

fn param<'a>(params: &'a [(&str, String)], name: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(held, _)| *held == name)
        .map(|(_, value)| value.as_str())
}

fn versioned_root() -> serde_json::Value {
    json!({
        "hasVersionedData": true,
        "capabilities": "Query,Sync,ChangeTracking",
        "layers": [{ "id": 0, "name": "Mains" }],
        "tables": []
    })
}

fn mains_layer() -> serde_json::Value {
    json!({
        "id": 0,
        "name": "Mains",
        "geometryType": "esriGeometryPolyline",
        "isDataVersioned": true,
        "extent": { "spatialReference": { "wkid": 4326 } },
        "objectIdField": "objectid",
        "maxRecordCount": 1000,
        "hasAttachments": true,
        "advancedQueryCapabilities": {
            "supportsPagination": true,
            "supportsQueryAttachments": false
        },
        "fields": [
            { "name": "objectid", "type": "esriFieldTypeOID" },
            { "name": "label", "type": "esriFieldTypeString" }
        ]
    })
}

/// One page of one feature, asserting the version on the way through.
fn mains_page(params: &[(&str, String)]) -> Vec<u8> {
    assert_eq!(param(params, "gdbVersion"), Some(VERSION), "{params:?}");
    if param(params, "returnCountOnly").is_some() {
        return serde_json::to_vec(&json!({ "count": 1 })).expect("the count serialises");
    }
    serde_json::to_vec(&json!({
        "objectIdFieldName": "objectid",
        "features": [{
            "attributes": { "objectid": 7, "label": "main seven" },
            "geometry": { "paths": [[[0.0, 0.0], [1.0, 1.0]]] }
        }]
    }))
    .expect("the page serialises")
}

fn fake() -> Fake {
    Fake::new()
        .json("", versioned_root())
        .json("/0", mains_layer())
        .answering("/0/query", mains_page)
        .answering("/0/7/attachments", |params| {
            assert_eq!(param(params, "gdbVersion"), Some(VERSION), "{params:?}");
            serde_json::to_vec(&json!({ "attachmentInfos": [] })).expect("the listing serialises")
        })
}

#[test]
fn the_named_version_rides_on_every_query() {
    let source = ArcgisSource::open_with_version(Box::new(fake()), ROOT, Some(VERSION.to_string()))
        .expect("the versioned fixture opens");
    let directory = tempfile::tempdir().expect("tempdir");
    // the layer is 4326 already, so there is no native pass; the page and the
    // attachment listing each assert the version themselves
    let extraction = source
        .extract(directory.path(), "verne-arcgis test")
        .expect("the versioned extraction runs");
    assert_eq!(extraction.sidecar.datasets.len(), 1);

    let described = source.describe();
    assert!(
        described
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("read at version SDE.DEFAULT")),
        "{described:?}"
    );
    let versioning = extraction
        .sidecar
        .log
        .entries
        .iter()
        .find(|entry| entry.detail.contains("versioning"))
        .expect("a versioning entry");
    let verne_core::Action::Skipped { reason } = &versioning.action else {
        panic!("versioning was carried: {versioning:#?}");
    };
    assert!(reason.contains("version SDE.DEFAULT"), "{reason}");

    // the service tracks changes but publishes no generation window, and the
    // report says which half of not extracting changes is whose
    let tracking = extraction
        .sidecar
        .log
        .entries
        .iter()
        .find(|entry| entry.detail.contains("change tracking"))
        .expect("a change tracking entry");
    let verne_core::Action::Skipped { reason } = &tracking.action else {
        panic!("change tracking was carried: {tracking:#?}");
    };
    assert!(reason.contains("no changeTrackingInfo"), "{reason}");
}

/// A wrong version name is an Esri error on the first count, and with a named
/// version that must fail the open: reading it as an empty service would tell
/// the operator their typo holds no data.
#[test]
fn a_wrong_version_name_fails_the_open() {
    let fake = Fake::new()
        .json("", versioned_root())
        .json("/0", mains_layer())
        .json(
            "/0/query",
            json!({ "error": { "code": 400, "message": "Failed to find Version 'NOPE'." } }),
        );
    let refused = ArcgisSource::open_with_version(Box::new(fake), ROOT, Some("NOPE".to_string()));
    let Err(ArcgisError::Service { message, .. }) = refused else {
        panic!("the wrong version was not refused");
    };
    assert!(message.contains("NOPE"), "{message}");
}

/// Without a named version nothing about versions is sent at all: the default
/// is the service's to pick, not a parameter verne invents.
#[test]
fn no_version_parameter_is_sent_unless_one_was_named() {
    let fake = Fake::new()
        .json("", versioned_root())
        .json("/0", mains_layer())
        .answering("/0/query", |params| {
            assert_eq!(param(params, "gdbVersion"), None, "{params:?}");
            serde_json::to_vec(&json!({ "count": 1 })).expect("the count serialises")
        });
    let source = ArcgisSource::open_with(Box::new(fake), ROOT).expect("the fixture opens");
    assert!(
        source
            .describe()
            .detail
            .as_deref()
            .is_some_and(|detail| !detail.contains("read at version")),
        "no version was named"
    );
}
