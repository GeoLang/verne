//! HTTP, and the one place the service's error convention is handled.
//!
//! ArcGIS can answer a failed request with HTTP 200 and an `error` object in
//! the body, so every JSON response is checked for one whatever the status
//! was. The token, when there is one, goes in the `X-Esri-Authorization`
//! header, which works for web-tier and token-authenticated deployments
//! alike, and never in the URL, where it would land in logs and process
//! lists.
//!
//! [`Fetch`] is a trait so the adapter can be exercised against canned
//! responses: everything above it is deterministic, and the tests feed it
//! JSON the docs describe rather than standing up a server.

use crate::ArcgisError;

/// One GET against the service. `params` go in the query string.
pub trait Fetch {
    fn get(&self, url: &str, params: &[(&str, String)]) -> Result<Vec<u8>, ArcgisError>;
}

/// The real client.
pub struct HttpFetch {
    client: reqwest::blocking::Client,
    token: Option<String>,
}

impl HttpFetch {
    pub fn new(token: Option<String>) -> Result<Self, ArcgisError> {
        let client = reqwest::blocking::Client::builder()
            .build()
            .map_err(|error| ArcgisError::Http {
                route: "building the HTTP client".into(),
                message: error.to_string(),
            })?;
        Ok(HttpFetch { client, token })
    }
}

impl Fetch for HttpFetch {
    fn get(&self, url: &str, params: &[(&str, String)]) -> Result<Vec<u8>, ArcgisError> {
        let mut request = self.client.get(url).query(params);
        if let Some(token) = &self.token {
            request = request.header("X-Esri-Authorization", format!("Bearer {token}"));
        }
        let response = request.send().map_err(|error| ArcgisError::Http {
            route: url.to_string(),
            message: error.to_string(),
        })?;
        let status = response.status();
        let bytes = response
            .bytes()
            .map_err(|error| ArcgisError::Http {
                route: url.to_string(),
                message: error.to_string(),
            })?
            .to_vec();
        if !status.is_success() {
            return Err(ArcgisError::Refused {
                route: url.to_string(),
                status: status.as_u16(),
                body: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        Ok(bytes)
    }
}

/// A JSON resource, with `f=json` asked for and the body's own error object
/// turned into an error whatever the HTTP status said.
pub fn json(
    fetch: &dyn Fetch,
    url: &str,
    params: &[(&str, String)],
) -> Result<serde_json::Value, ArcgisError> {
    let mut all: Vec<(&str, String)> = vec![("f", "json".to_string())];
    all.extend(params.iter().map(|(name, value)| (*name, value.clone())));
    let bytes = fetch.get(url, &all)?;
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| ArcgisError::BadJson {
            route: url.to_string(),
            message: error.to_string(),
        })?;
    if let Some(error) = value.get("error") {
        return Err(ArcgisError::Service {
            route: url.to_string(),
            code: error.get("code").and_then(serde_json::Value::as_i64),
            message: error
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("no message")
                .to_string(),
        });
    }
    Ok(value)
}
