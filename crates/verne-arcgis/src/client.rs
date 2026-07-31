//! HTTP, and the one place the service's error convention is handled.
//!
//! ArcGIS can answer a failed request with HTTP 200 and an `error` object in
//! the body, so every JSON response is checked for one whatever the status
//! was. The token, when there is one, goes in the `X-Esri-Authorization`
//! header, which works for web-tier and token-authenticated deployments
//! alike, and never in the URL, where it would land in logs and process
//! lists. The one route it is dropped on rather than sent is a job's result
//! file, whose pointer redirects to a signed URL on a host that is not the
//! service.
//!
//! [`Fetch`] is a trait so the adapter can be exercised against canned
//! responses: everything above it is deterministic, and the tests feed it
//! JSON the docs describe rather than standing up a server.
//!
//! Minting sits below [`Fetch`] rather than behind it: the trait is the
//! service's surface, and the token route is not part of it. An app id and
//! secret buy a token on the first request that needs one, and the token is
//! held until it is nearly spent.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::{ArcgisError, Credentials};

/// A token is re-minted this far ahead of its expiry, so a request cannot go
/// out holding one that dies in flight.
const MARGIN: Duration = Duration::from_secs(60);

/// Esri documents two weeks as the longest token it will issue. Clamping to it
/// keeps a nonsense `expires_in` from overflowing the deadline.
const LONGEST_LIFETIME: Duration = Duration::from_secs(20_160 * 60);

/// How many redirects a job's result file may take before verne stops
/// following. A live service takes one: the pointer answers with the signed
/// URL of a file on storage.
const REDIRECTS: usize = 4;

/// One request against the service.
pub trait Fetch {
    /// GET, with `params` in the query string.
    fn get(&self, url: &str, params: &[(&str, String)]) -> Result<Vec<u8>, ArcgisError>;

    /// POST, with `params` as a form body. The query route takes the same
    /// parameters either way, and the body is where an object id list goes
    /// when it would blow past what a URL can carry.
    fn post_form(&self, url: &str, params: &[(&str, String)]) -> Result<Vec<u8>, ArcgisError>;

    /// GET a file the service points at rather than serves: the result of an
    /// `extractChanges` job, whose URL answers with a redirect to a signed one
    /// on storage that is not the service. The token must not follow it there,
    /// and the signed URL needs none.
    fn get_file(&self, url: &str) -> Result<Vec<u8>, ArcgisError>;
}

/// The real client.
pub struct HttpFetch {
    client: reqwest::blocking::Client,
    /// A second client that follows nothing, for the result file whose pointer
    /// redirects to another host. reqwest drops `Authorization` across a host
    /// boundary and leaves a header it does not know alone, which is where the
    /// token rides, so the redirect is followed by hand instead.
    unfollowing: reqwest::blocking::Client,
    credentials: Credentials,
    /// The minted token, behind a lock because [`Fetch`] hands out `&self` and
    /// the mint is a write.
    minted: Mutex<Option<Minted>>,
}

/// A token verne minted, and when it stops being worth sending.
struct Minted {
    token: String,
    expires_at: Instant,
}

impl HttpFetch {
    pub fn new(credentials: Credentials) -> Result<Self, ArcgisError> {
        let built = |builder: reqwest::blocking::ClientBuilder| {
            builder.build().map_err(|error| ArcgisError::Http {
                route: "building the HTTP client".into(),
                message: error.to_string(),
            })
        };
        Ok(HttpFetch {
            client: built(reqwest::blocking::Client::builder())?,
            unfollowing: built(
                reqwest::blocking::Client::builder().redirect(reqwest::redirect::Policy::none()),
            )?,
            credentials,
            minted: Mutex::new(None),
        })
    }

    /// The token to send with the next request, minted now if there is nothing
    /// held or what is held is nearly spent.
    fn bearer(&self) -> Result<Option<String>, ArcgisError> {
        let (token_url, client_id, client_secret) = match &self.credentials {
            Credentials::Anonymous => return Ok(None),
            Credentials::Token(token) => return Ok(Some(token.clone())),
            Credentials::ClientCredentials {
                token_url,
                client_id,
                client_secret,
            } => (token_url, client_id, client_secret),
        };
        let mut held = self.minted.lock().expect("the minted token lock");
        if let Some(minted) = held.as_ref()
            && minted.expires_at.saturating_duration_since(Instant::now()) > MARGIN
        {
            return Ok(Some(minted.token.clone()));
        }
        let fresh = self.mint(token_url, client_id, client_secret)?;
        let token = fresh.token.clone();
        *held = Some(fresh);
        Ok(Some(token))
    }

    /// Buy a token with the app id and secret. Nothing here goes through
    /// [`Fetch`], and nothing on the error paths carries the request body: the
    /// secret is in it.
    fn mint(
        &self,
        token_url: &str,
        client_id: &str,
        client_secret: &str,
    ) -> Result<Minted, ArcgisError> {
        // counted from before the request, so the round trip is spent out of
        // the token's life rather than added to it
        let asked_at = Instant::now();
        let response = self
            .client
            .post(token_url)
            .form(&[
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("grant_type", "client_credentials"),
            ])
            .send()
            .map_err(|error| ArcgisError::Http {
                route: token_url.to_string(),
                message: error.to_string(),
            })?;
        let status = response.status();
        let bytes = response.bytes().map_err(|error| ArcgisError::Http {
            route: token_url.to_string(),
            message: error.to_string(),
        })?;
        // the token route answers a refused mint the way every other route
        // answers a refusal, with 200 and an error object, so the body is read
        // before the status is judged
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|error| ArcgisError::BadJson {
                route: token_url.to_string(),
                message: error.to_string(),
            })?;
        if let Some(error) = value.get("error") {
            return Err(ArcgisError::Service {
                route: token_url.to_string(),
                code: error.get("code").and_then(serde_json::Value::as_i64),
                message: error
                    .get("message")
                    .or_else(|| error.get("error_description"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("no message")
                    .to_string(),
            });
        }
        if !status.is_success() {
            return Err(ArcgisError::Refused {
                route: token_url.to_string(),
                status: status.as_u16(),
                body: "no error object, and a token route's body is not shown because it carries \
                       a token"
                    .into(),
            });
        }
        let token = value
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ArcgisError::BadShape {
                route: token_url.to_string(),
                message: "no access_token, and verne will not send a request unauthenticated that \
                          was meant to carry a minted token"
                    .into(),
            })?;
        // seconds from now, per the token route's reference
        let expires_in = value
            .get("expires_in")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| ArcgisError::BadShape {
                route: token_url.to_string(),
                message: "no expires_in, so verne cannot tell when to mint again".into(),
            })?;
        Ok(Minted {
            token: token.to_string(),
            expires_at: asked_at + Duration::from_secs(expires_in).min(LONGEST_LIFETIME),
        })
    }

    fn send(
        &self,
        url: &str,
        request: reqwest::blocking::RequestBuilder,
    ) -> Result<Vec<u8>, ArcgisError> {
        let request = match self.bearer()? {
            Some(token) => request.header("X-Esri-Authorization", format!("Bearer {token}")),
            None => request,
        };
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

impl Fetch for HttpFetch {
    fn get(&self, url: &str, params: &[(&str, String)]) -> Result<Vec<u8>, ArcgisError> {
        self.send(url, self.client.get(url).query(params))
    }

    fn post_form(&self, url: &str, params: &[(&str, String)]) -> Result<Vec<u8>, ArcgisError> {
        self.send(url, self.client.post(url).form(params))
    }

    fn get_file(&self, url: &str) -> Result<Vec<u8>, ArcgisError> {
        let first = reqwest::Url::parse(url).map_err(|error| ArcgisError::Http {
            route: url.to_string(),
            message: error.to_string(),
        })?;
        let mut next = first.clone();
        for _ in 0..REDIRECTS {
            let mut request = self.unfollowing.get(next.clone());
            // the token goes to the service and to nowhere else: past the
            // redirect the host is storage, and the URL is signed already
            if same_host(&next, &first)
                && let Some(token) = self.bearer()?
            {
                request = request.header("X-Esri-Authorization", format!("Bearer {token}"));
            }
            let response = request.send().map_err(|error| ArcgisError::Http {
                route: next.to_string(),
                message: error.to_string(),
            })?;
            let status = response.status();
            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| ArcgisError::BadShape {
                        route: next.to_string(),
                        message: format!(
                            "answered {status} with no Location, so the file it points at cannot be reached"
                        ),
                    })?;
                next = next.join(location).map_err(|error| ArcgisError::BadShape {
                    route: next.to_string(),
                    message: format!("redirected to {location}, which is not a URL: {error}"),
                })?;
                continue;
            }
            let bytes = response
                .bytes()
                .map_err(|error| ArcgisError::Http {
                    route: next.to_string(),
                    message: error.to_string(),
                })?
                .to_vec();
            if !status.is_success() {
                return Err(ArcgisError::Refused {
                    route: next.to_string(),
                    status: status.as_u16(),
                    body: String::from_utf8_lossy(&bytes).into_owned(),
                });
            }
            return Ok(bytes);
        }
        Err(ArcgisError::Http {
            route: url.to_string(),
            message: format!("redirected more than {REDIRECTS} times"),
        })
    }
}

/// Whether two URLs are the same origin, by the same rule reqwest drops a
/// sensitive header on: host, port and scheme all have to match.
fn same_host(next: &reqwest::Url, first: &reqwest::Url) -> bool {
    next.host_str() == first.host_str()
        && next.port_or_known_default() == first.port_or_known_default()
        && next.scheme() == first.scheme()
}

/// A JSON resource, with `f=json` asked for and the body's own error object
/// turned into an error whatever the HTTP status said.
pub fn json(
    fetch: &dyn Fetch,
    url: &str,
    params: &[(&str, String)],
) -> Result<serde_json::Value, ArcgisError> {
    let bytes = fetch.get(url, &with_format(params))?;
    parse(url, &bytes)
}

/// The same resource asked for with a POST, for a parameter list too big to
/// be a URL.
pub fn json_post(
    fetch: &dyn Fetch,
    url: &str,
    params: &[(&str, String)],
) -> Result<serde_json::Value, ArcgisError> {
    let bytes = fetch.post_form(url, &with_format(params))?;
    parse(url, &bytes)
}

fn with_format<'a>(params: &[(&'a str, String)]) -> Vec<(&'a str, String)> {
    let mut all: Vec<(&str, String)> = vec![("f", "json".to_string())];
    all.extend(params.iter().map(|(name, value)| (*name, value.clone())));
    all
}

/// A JSON body, with the service's error convention applied to it whatever
/// route it came off.
pub fn parse(url: &str, bytes: &[u8]) -> Result<serde_json::Value, ArcgisError> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|error| ArcgisError::BadJson {
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
