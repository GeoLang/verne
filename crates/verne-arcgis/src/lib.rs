//! ArcGIS Feature Service adapter: inventory and extraction over REST.
//!
//! The hosted half of the Esri story. The gdb adapter reads a `.gdb` on disk
//! through GDAL; this crate reads a feature service over HTTP and needs no
//! GDAL at all, so it is always built. Reading is read-only by construction:
//! every request is a GET, and the one credential comes from the environment
//! rather than an argument, so it is never in a process list.
//!
//! The service does the reprojecting. Every feature query asks for EPSG:4326
//! in `outSR` and verne does no coordinate arithmetic, which is what lets the
//! loader's whole path stay GDAL-free. The cost is named in the report: no
//! native original rides on the inserts, and there is no GeoPackage on this
//! path, because writing one is GDAL's work.

use verne_core::{Item, Source, SourceDescription};

mod client;
mod extract;
mod geometry;
mod service;
mod verdict;

pub use client::{Fetch, HttpFetch};
pub use extract::Extraction;
pub use geometry::{EsriGeometry, Position};
pub use service::{Layer, Service};

/// The environment variable a token is read from, never an argument.
pub const TOKEN_VAR: &str = "VERNE_ARCGIS_TOKEN";

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
}

/// A feature service, its metadata fetched at open. The queries an extraction
/// makes go through the same [`Fetch`] the open did.
pub struct ArcgisSource {
    service: Service,
    fetch: Box<dyn Fetch>,
}

impl ArcgisSource {
    /// Fetch the service and every layer it lists. `token` is sent on every
    /// request when given; a public service needs none.
    pub fn open(url: &str, token: Option<String>) -> Result<Self, ArcgisError> {
        let fetch = HttpFetch::new(token)?;
        Self::open_with(Box::new(fetch), url)
    }

    /// The same open against any [`Fetch`], which is what lets the tests feed
    /// canned responses instead of standing up a server.
    pub fn open_with(fetch: Box<dyn Fetch>, url: &str) -> Result<Self, ArcgisError> {
        let url = normalize(url)?;
        let root = client::json(fetch.as_ref(), &url, &[])?;
        let (head, ids) =
            service::parse_service(&root).map_err(|message| ArcgisError::BadShape {
                route: url.clone(),
                message,
            })?;
        let mut layers = Vec::new();
        for id in ids {
            let route = format!("{url}/{id}");
            let raw = client::json(fetch.as_ref(), &route, &[])?;
            let mut layer =
                service::parse_layer(&raw).map_err(|message| ArcgisError::BadShape {
                    route: route.clone(),
                    message,
                })?;
            layer.count = count(fetch.as_ref(), &url, layer.id);
            layers.push(layer);
        }
        if layers.is_empty() {
            return Err(ArcgisError::NothingFound);
        }
        Ok(ArcgisSource {
            service: Service {
                url,
                description: head.description,
                versioned: head.versioned,
                layers,
            },
            fetch,
        })
    }

    pub fn service(&self) -> &Service {
        &self.service
    }
}

/// How many features a layer holds, asked for cheaply. A layer that will not
/// answer is a layer with no count, not a failed open: the inventory still
/// stands and the extraction will name the refusal itself.
fn count(fetch: &dyn Fetch, url: &str, id: i64) -> Option<u64> {
    let value = client::json(
        fetch,
        &format!("{url}/{id}/query"),
        &[
            ("where", "1=1".to_string()),
            ("returnCountOnly", "true".to_string()),
        ],
    )
    .ok()?;
    value.get("count").and_then(serde_json::Value::as_u64)
}

/// The FeatureServer root, held to exactly that: a layer id on the end would
/// make every route this crate builds wrong, and a MapServer answers to a
/// different contract than the one verne was written against.
fn normalize(url: &str) -> Result<String, ArcgisError> {
    let trimmed = url.trim_end_matches('/');
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(ArcgisError::BadUrl(
            url.to_string(),
            "expected http:// or https://".into(),
        ));
    }
    let last = trimmed.rsplit('/').next().unwrap_or_default();
    if last.chars().all(|c| c.is_ascii_digit()) && !last.is_empty() {
        return Err(ArcgisError::BadUrl(
            url.to_string(),
            format!(
                "this points at layer {last}; give the FeatureServer root and verne will read every layer in it"
            ),
        ));
    }
    if last != "FeatureServer" {
        return Err(ArcgisError::BadUrl(
            url.to_string(),
            "expected a URL ending in /FeatureServer".into(),
        ));
    }
    Ok(trimmed.to_string())
}

impl Source for ArcgisSource {
    type Error = ArcgisError;

    fn describe(&self) -> SourceDescription {
        let layers = self
            .service
            .layers
            .iter()
            .filter(|layer| layer.geometry_type.is_some())
            .count();
        let tables = self.service.layers.len() - layers;
        let mut detail = format!(
            "{layers} layer{}, {tables} table{}",
            if layers == 1 { "" } else { "s" },
            if tables == 1 { "" } else { "s" }
        );
        if let Some(description) = &self.service.description {
            detail.push_str(&format!(". {description}"));
        }
        SourceDescription::new("ArcGIS Feature Service", self.service.url.clone())
            .with_detail(detail)
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

    #[test]
    fn a_layer_url_is_refused_with_directions() {
        let refused =
            normalize("https://host/arcgis/rest/services/x/FeatureServer/3").expect_err("refused");
        assert!(refused.to_string().contains("layer 3"), "{refused}");
    }

    #[test]
    fn a_mapserver_url_is_refused() {
        assert!(normalize("https://host/arcgis/rest/services/x/MapServer").is_err());
    }

    #[test]
    fn a_trailing_slash_is_trimmed() {
        let url = normalize("https://host/rest/services/x/FeatureServer/").expect("accepted");
        assert_eq!(url, "https://host/rest/services/x/FeatureServer");
    }
}
