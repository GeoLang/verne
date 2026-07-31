//! The canned feature service the fixture tests read.
//!
//! Every response here is the shape the REST API answers with, checked against
//! a live service while the adapter was written, so changing one is a claim
//! about what ArcGIS sends. A URL with no route is a panic rather than an
//! empty answer: a missing fixture must be louder than a wrong assertion.

// every test binary compiles this module whole, so a fixture one of them does
// not read is unused there and used in the next one over
#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use verne_arcgis::{ArcgisError, Fetch};

/// The FeatureServer root every route hangs off.
pub const ROOT: &str = "https://example.invalid/arcgis/rest/services/Fixture/FeatureServer";

/// One request the adapter made, as the fake saw it.
#[derive(Debug)]
pub struct Call {
    /// The URL past the fake's root, so `/0/query` rather than the whole thing.
    pub route: String,
    /// "GET" or "POST"; a route answers either, and which was used is part of
    /// what a test asserts.
    pub method: &'static str,
    pub params: Vec<(String, String)>,
}

impl Call {
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(held, _)| held == name)
            .map(|(_, value)| value.as_str())
    }
}

type Handler = Box<dyn Fn(&[(&str, String)]) -> Vec<u8>>;

/// A [`Fetch`] answering from routes instead of from the network.
pub struct Fake {
    root: String,
    routes: Vec<(String, Handler)>,
    calls: Rc<RefCell<Vec<Call>>>,
}

impl Fake {
    pub fn new() -> Self {
        Fake::at(ROOT)
    }

    /// A fake hung off some other root, for the routes that are not under a
    /// FeatureServer at all: a portal's search is one.
    pub fn at(root: &str) -> Self {
        Fake {
            root: root.to_string(),
            routes: Vec::new(),
            calls: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// A route answering the same JSON however it is asked for.
    pub fn json(self, suffix: &str, body: serde_json::Value) -> Self {
        self.answering(suffix, move |_| {
            serde_json::to_vec(&body).expect("the fixture serialises")
        })
    }

    /// A route whose answer depends on the query string: a page, a count, or a
    /// blob.
    pub fn answering(
        mut self,
        suffix: &str,
        handler: impl Fn(&[(&str, String)]) -> Vec<u8> + 'static,
    ) -> Self {
        self.routes.push((suffix.to_string(), Box::new(handler)));
        self
    }

    /// Every request made so far, shared with the fake, so a test can look at
    /// them after `open_with` has taken the box.
    pub fn calls(&self) -> Rc<RefCell<Vec<Call>>> {
        Rc::clone(&self.calls)
    }
}

impl Fake {
    fn answer(
        &self,
        method: &'static str,
        url: &str,
        params: &[(&str, String)],
    ) -> Result<Vec<u8>, ArcgisError> {
        let suffix = url
            .strip_prefix(self.root.as_str())
            .unwrap_or_else(|| panic!("the adapter asked for {url}, which is off the fixture"));
        self.calls.borrow_mut().push(Call {
            route: suffix.to_string(),
            method,
            params: params
                .iter()
                .map(|(name, value)| ((*name).to_string(), value.clone()))
                .collect(),
        });
        match self.routes.iter().find(|(held, _)| held == suffix) {
            Some((_, handler)) => Ok(handler(params)),
            None => panic!("no fixture for {url} with {params:?}"),
        }
    }
}

impl Fetch for Fake {
    fn get(&self, url: &str, params: &[(&str, String)]) -> Result<Vec<u8>, ArcgisError> {
        self.answer("GET", url, params)
    }

    fn post_form(&self, url: &str, params: &[(&str, String)]) -> Result<Vec<u8>, ArcgisError> {
        self.answer("POST", url, params)
    }

    /// The redirect a live service answers a result URL with is the client's
    /// business, and it is tested against a socket; here the route answers the
    /// file itself.
    fn get_file(&self, url: &str) -> Result<Vec<u8>, ArcgisError> {
        self.answer("GET", url, &[])
    }
}

/// The service root: one layer and one table.
pub fn service_root() -> serde_json::Value {
    json!({
        "serviceDescription": "Wells and their logs",
        "hasVersionedData": false,
        "layers": [{ "id": 0, "name": "Wells" }],
        "tables": [{ "id": 1, "name": "Logs" }]
    })
}

/// The same root from a service that tracks changes and publishes the
/// generation window `extractChanges` takes back, one generation per layer.
pub fn tracking_root(server_gen: u64) -> serde_json::Value {
    let mut root = service_root();
    let object = root.as_object_mut().expect("the root is an object");
    object.insert("capabilities".into(), "Query,ChangeTracking".into());
    object.insert(
        "changeTrackingInfo".into(),
        json!({
            "lastSyncDate": 1743630690000i64,
            "layerServerGens": [
                { "id": 0, "serverGen": server_gen },
                { "id": 1, "serverGen": server_gen }
            ]
        }),
    );
    root
}

/// The point layer: aliases, a coded and a range domain, a Date column, one
/// subtype, attachments, a renderer, and the origin end of a relationship. Its
/// extent is web mercator, so every reprojection loss is in play.
pub fn wells_layer() -> serde_json::Value {
    json!({
        "id": 0,
        "name": "Wells",
        "geometryType": "esriGeometryPoint",
        "hasZ": false,
        "hasM": false,
        "extent": { "spatialReference": { "wkid": 102100, "latestWkid": 3857 } },
        "objectIdField": "objectid",
        "maxRecordCount": 2,
        "hasAttachments": true,
        "advancedQueryCapabilities": {
            "supportsPagination": true,
            "supportsQueryAttachments": false
        },
        "drawingInfo": { "renderer": { "type": "simple" } },
        "fields": [
            { "name": "objectid", "type": "esriFieldTypeOID", "alias": "OBJECTID" },
            {
                "name": "status",
                "type": "esriFieldTypeInteger",
                "alias": "Status",
                "domain": {
                    "type": "codedValue",
                    "name": "StatusCodes",
                    "codedValues": [
                        { "name": "Active", "code": 1 },
                        { "name": "Plugged", "code": 2 }
                    ]
                }
            },
            {
                "name": "depth",
                "type": "esriFieldTypeDouble",
                "domain": { "type": "range", "name": "DepthRange", "range": [0, 5000] }
            },
            { "name": "drilled", "type": "esriFieldTypeDate" }
        ],
        "subtypeField": "status",
        "defaultSubtypeCode": 1,
        "subtypes": [{
            "code": 1,
            "name": "Active",
            "defaultValues": { "depth": 100 },
            "domains": { "depth": { "type": "range", "name": "DepthRange", "range": [0, 5000] } }
        }],
        "relationships": [{
            "id": 0,
            "name": "wells_logs",
            "relatedTableId": 1,
            "cardinality": "esriRelCardinalityOneToMany",
            "role": "esriRelRoleOrigin",
            "keyField": "objectid",
            "composite": true
        }]
    })
}

/// The table: no `geometryType`, and the destination end of the same
/// relationship, whose key field is the foreign key on this side.
pub fn logs_table() -> serde_json::Value {
    json!({
        "id": 1,
        "name": "Logs",
        "hasZ": false,
        "hasM": false,
        "objectIdField": "objectid",
        "maxRecordCount": 1000,
        "advancedQueryCapabilities": {
            "supportsPagination": true,
            "supportsQueryAttachments": false
        },
        "fields": [
            { "name": "objectid", "type": "esriFieldTypeOID" },
            { "name": "well_id", "type": "esriFieldTypeInteger" },
            { "name": "note", "type": "esriFieldTypeString" }
        ],
        "relationships": [{
            "id": 0,
            "name": "wells_logs",
            "relatedTableId": 0,
            "cardinality": "esriRelCardinalityOneToMany",
            "role": "esriRelRoleDestination",
            "keyField": "well_id",
            "composite": true
        }]
    })
}
