//! ArcGIS Feature Service adapter: inventory and extraction over REST.
//!
//! The hosted half of the Esri story. The gdb adapter reads a `.gdb` on disk
//! through GDAL; this crate reads a feature service over HTTP and needs no
//! GDAL at all, so it is always built. Reading is read-only by construction:
//! every request reads (the one POST is a query whose id list outgrows a
//! URL), and the credentials come from the environment rather than an
//! argument, so they are never in a process list.
//!
//! The service does the reprojecting. Every feature query asks for EPSG:4326
//! in `outSR` and verne does no coordinate arithmetic, which is what lets the
//! loader's whole path stay GDAL-free; the untransformed original is fetched
//! in a second per-page pass and rides on each insert where its reference can
//! be declared at all. There is no GeoPackage on this path, because writing
//! one is GDAL's work.

use verne_core::{Item, Source, SourceDescription};

mod client;
mod extract;
mod geometry;
mod portal;
mod service;
mod verdict;

pub use client::{Fetch, HttpFetch};
pub use extract::Extraction;
pub use geometry::{EsriGeometry, Position};
pub use portal::{PortalService, feature_services};
pub use service::{Layer, Service};

/// The environment variables the tokens and secrets are read from, never
/// arguments: an argument is in the process list of every other user on the
/// machine.
pub const TOKEN_VAR: &str = "VERNE_ARCGIS_TOKEN";
pub const CLIENT_ID_VAR: &str = "VERNE_ARCGIS_CLIENT_ID";
pub const CLIENT_SECRET_VAR: &str = "VERNE_ARCGIS_CLIENT_SECRET";
pub const PORTAL_VAR: &str = "VERNE_ARCGIS_PORTAL";

/// How verne proves who it is to the service.
pub enum Credentials {
    /// A public service, which needs nothing.
    Anonymous,
    /// A token the operator already holds, sent as it stands.
    Token(String),
    /// An app id and secret verne mints a token from itself, so a long run
    /// does not die when the token it was handed expires. `token_url` is the
    /// portal's `oauth2/token` route.
    ClientCredentials {
        token_url: String,
        client_id: String,
        client_secret: String,
    },
}

/// Written by hand rather than derived: a derived `Debug` would put the secret
/// in whatever log line or panic printed it.
impl std::fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credentials::Anonymous => formatter.write_str("Anonymous"),
            Credentials::Token(_) => formatter.write_str("Token(redacted)"),
            Credentials::ClientCredentials {
                token_url,
                client_id,
                ..
            } => formatter
                .debug_struct("ClientCredentials")
                .field("token_url", token_url)
                .field("client_id", client_id)
                .field("client_secret", &"redacted")
                .finish(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ArcgisError {
    #[error("{0} is not a feature service URL: {1}")]
    BadUrl(String, String),
    #[error("cannot reach {route}: {message}")]
    Http { route: String, message: String },
    #[error("{route} answered {status}: {body}")]
    Refused {
        route: String,
        status: u16,
        body: String,
    },
    #[error("{route} answered with an error{}: {message}", code.map(|code| format!(" ({code})")).unwrap_or_default())]
    Service {
        route: String,
        code: Option<i64>,
        message: String,
    },
    #[error("{route} did not answer with JSON: {message}")]
    BadJson { route: String, message: String },
    #[error("{route} answered with a shape verne does not know: {message}")]
    BadShape { route: String, message: String },
    #[error(
        "the service lists no layers and no tables; an empty inventory must not be mistaken for a clean source"
    )]
    NothingFound,
    #[error("cannot write {path}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not an extraction verne wrote: {message}")]
    BadPrevious { path: String, message: String },
    #[error(
        "the extraction at {path} is itself a delta; --since diffs against the full extraction the datasets were first loaded from"
    )]
    DeltaPrevious { path: String },
}

/// A feature service, its metadata fetched at open. The queries an extraction
/// makes go through the same [`Fetch`] the open did.
pub struct ArcgisSource {
    service: Service,
    fetch: Box<dyn Fetch>,
}

impl ArcgisSource {
    /// Fetch the service and every layer it lists. The credentials are sent on
    /// every request; a public service needs none.
    ///
    /// `gdb_version` is a named geodatabase version to read instead of the
    /// default, such as `SDE.DEFAULT` or an editor's own. It rides on every
    /// query, so the counts, the features and the attachments all describe
    /// that version's state. No REST resource here lists versions, so the
    /// name is the operator's to know; a wrong one fails the open loudly.
    pub fn open(
        url: &str,
        credentials: Credentials,
        gdb_version: Option<String>,
    ) -> Result<Self, ArcgisError> {
        let fetch = HttpFetch::new(credentials)?;
        Self::open_with_version(Box::new(fetch), url, gdb_version)
    }

    /// [`Self::open_with_version`] against the default version.
    pub fn open_with(fetch: Box<dyn Fetch>, url: &str) -> Result<Self, ArcgisError> {
        Self::open_with_version(fetch, url, None)
    }

    /// The open against any [`Fetch`], which is what lets the tests feed
    /// canned responses instead of standing up a server.
    ///
    /// A URL ending in a layer id, which is how a portal names its items,
    /// scopes the source to that one layer: only it is fetched, inventoried
    /// and extracted, and a relationship whose other side is out of scope is
    /// reported rather than followed.
    pub fn open_with_version(
        fetch: Box<dyn Fetch>,
        url: &str,
        gdb_version: Option<String>,
    ) -> Result<Self, ArcgisError> {
        let (url, format, scope) = normalize(url)?;
        let root = client::json(fetch.as_ref(), &url, &[])?;
        let (head, ids) =
            service::parse_service(&root).map_err(|message| ArcgisError::BadShape {
                route: url.clone(),
                message,
            })?;
        let ids: Vec<i64> = match scope {
            Some(scoped) => {
                if !ids.contains(&scoped) {
                    return Err(ArcgisError::BadUrl(
                        format!("{url}/{scoped}"),
                        format!("the service lists no layer or table with id {scoped}"),
                    ));
                }
                vec![scoped]
            }
            None => ids,
        };
        let mut layers = Vec::new();
        for id in ids {
            let route = format!("{url}/{id}");
            let raw = client::json(fetch.as_ref(), &route, &[])?;
            let mut layer =
                service::parse_layer(&raw).map_err(|message| ArcgisError::BadShape {
                    route: route.clone(),
                    message,
                })?;
            // a group or raster layer holds no rows to count, and a real
            // MapServer refuses the question
            if layer.queryable() {
                layer.count = match count(fetch.as_ref(), &url, layer.id, gdb_version.as_deref()) {
                    Ok(counted) => counted,
                    // a named version that fails is the operator's typo
                    // and must fail the open, not read as an empty layer
                    Err(error) if gdb_version.is_some() => return Err(error),
                    Err(_) => None,
                };
            }
            layers.push(layer);
        }
        if layers.is_empty() {
            return Err(ArcgisError::NothingFound);
        }
        Ok(ArcgisSource {
            service: Service {
                url,
                format,
                scope,
                gdb_version,
                description: head.description,
                versioned: head.versioned,
                change_tracking: head.change_tracking,
                change_generations: head.change_generations,
                layers,
            },
            fetch,
        })
    }

    pub fn service(&self) -> &Service {
        &self.service
    }
}

/// How many features a layer holds, asked for cheaply. The caller decides
/// what a refusal means: without a named version it is a layer with no
/// count, with one it is the operator's typo.
fn count(
    fetch: &dyn Fetch,
    url: &str,
    id: i64,
    gdb_version: Option<&str>,
) -> Result<Option<u64>, ArcgisError> {
    let mut params = vec![
        ("where", "1=1".to_string()),
        ("returnCountOnly", "true".to_string()),
    ];
    if let Some(version) = gdb_version {
        params.push(("gdbVersion", version.to_string()));
    }
    let value = client::json(fetch, &format!("{url}/{id}/query"), &params)?;
    Ok(value.get("count").and_then(serde_json::Value::as_u64))
}

/// The FeatureServer or MapServer root, what to call it, and the one layer
/// the URL scoped verne to when it ended in a layer id, which is how a portal
/// names its items. Both roots answer the same layer and query contract; what
/// a MapServer adds is group and raster layers, which the verdicts handle.
fn normalize(url: &str) -> Result<(String, &'static str, Option<i64>), ArcgisError> {
    let trimmed = url.trim_end_matches('/');
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(ArcgisError::BadUrl(
            url.to_string(),
            "expected http:// or https://".into(),
        ));
    }
    let (root, scope) = match trimmed.rsplit_once('/') {
        Some((root, last)) if !last.is_empty() && last.chars().all(|c| c.is_ascii_digit()) => {
            let id = last.parse().map_err(|_| {
                ArcgisError::BadUrl(url.to_string(), format!("layer {last} is not an id"))
            })?;
            (root, Some(id))
        }
        _ => (trimmed, None),
    };
    let format = match root.rsplit('/').next() {
        Some("FeatureServer") => "ArcGIS Feature Service",
        Some("MapServer") => "ArcGIS Map Service",
        _ => {
            return Err(ArcgisError::BadUrl(
                url.to_string(),
                "expected a URL ending in /FeatureServer or /MapServer, with or without a layer id"
                    .into(),
            ));
        }
    };
    Ok((root.to_string(), format, scope))
}

impl Source for ArcgisSource {
    type Error = ArcgisError;

    fn describe(&self) -> SourceDescription {
        // counted by what each is, so a map service's groups and rasters are
        // not mistaken for tables
        let mut layers = 0;
        let mut tables = 0;
        let mut groups = 0;
        let mut rasters = 0;
        for layer in &self.service.layers {
            match (layer.kind.as_deref(), &layer.geometry_type) {
                (Some("Group Layer"), _) => groups += 1,
                (Some("Raster Layer"), _) => rasters += 1,
                (_, Some(_)) => layers += 1,
                (_, None) => tables += 1,
            }
        }
        let mut detail = format!(
            "{layers} layer{}, {tables} table{}",
            if layers == 1 { "" } else { "s" },
            if tables == 1 { "" } else { "s" }
        );
        if groups > 0 {
            detail.push_str(&format!(
                ", {groups} group layer{}",
                if groups == 1 { "" } else { "s" }
            ));
        }
        if rasters > 0 {
            detail.push_str(&format!(
                ", {rasters} raster layer{}",
                if rasters == 1 { "" } else { "s" }
            ));
        }
        if let Some(version) = &self.service.gdb_version {
            detail.push_str(&format!(", read at version {version}"));
        }
        if let Some(description) = &self.service.description {
            detail.push_str(&format!(". {description}"));
        }
        // the location is the URL as the operator gave it, scope included, so
        // the report and the sidecar say what was pointed at
        let location = match self.service.scope {
            Some(id) => format!("{}/{id}", self.service.url),
            None => self.service.url.clone(),
        };
        SourceDescription::new(self.service.format, location).with_detail(detail)
    }

    fn inventory(&self) -> Result<Vec<Item>, Self::Error> {
        let items = verdict::items(&self.service);
        if items.is_empty() {
            return Err(ArcgisError::NothingFound);
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    /// A portal names its items by layer URL, so one scopes the source rather
    /// than being refused.
    #[test]
    fn a_layer_url_scopes_the_source_to_that_layer() {
        let (root, _, scope) =
            normalize("https://host/arcgis/rest/services/x/FeatureServer/3").expect("accepted");
        assert_eq!(root, "https://host/arcgis/rest/services/x/FeatureServer");
        assert_eq!(scope, Some(3));
    }

    /// Both roots answer the same layer and query contract, and the format
    /// names which one was given.
    #[test]
    fn a_mapserver_url_is_accepted_and_named() {
        let (root, format, scope) =
            normalize("https://host/arcgis/rest/services/x/MapServer/3").expect("accepted");
        assert_eq!(root, "https://host/arcgis/rest/services/x/MapServer");
        assert_eq!(format, "ArcGIS Map Service");
        assert_eq!(scope, Some(3));
    }

    #[test]
    fn an_imageserver_url_is_refused() {
        assert!(normalize("https://host/arcgis/rest/services/x/ImageServer").is_err());
    }

    #[test]
    fn a_trailing_slash_is_trimmed() {
        let (url, format, scope) =
            normalize("https://host/rest/services/x/FeatureServer/").expect("accepted");
        assert_eq!(url, "https://host/rest/services/x/FeatureServer");
        assert_eq!(format, "ArcGIS Feature Service");
        assert_eq!(scope, None);
    }
}
