//! Minting a token from an app id and secret.
//!
//! These run against a socket rather than a canned [`Fetch`], because what is
//! under test is the header that leaves the machine and the number of times the
//! token route is asked. A fake `Fetch` sits above both.
//!
//! The server answers a fixed number of requests and is joined at the end, so
//! a test that stops making requests fails rather than racing. Nothing sleeps:
//! expiry is driven by an `expires_in` inside the re-mint margin, which makes
//! every request mint again.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread::JoinHandle;

use verne_arcgis::{Credentials, Fetch, HttpFetch};

const TOKEN_ROUTE: &str = "/sharing/rest/oauth2/token";
const SECRET: &str = "6f7a1c0e-not-a-real-secret";

/// One request the server saw.
struct Seen {
    target: String,
    authorization: Option<String>,
    body: String,
}

impl Seen {
    fn is_mint(&self) -> bool {
        self.target.starts_with(TOKEN_ROUTE)
    }
}

/// A server that answers `count` requests, then hands back what it saw.
struct Server {
    address: SocketAddr,
    handle: JoinHandle<Vec<Seen>>,
}

impl Server {
    /// `mints` are the bodies the token route answers with, in order; the last
    /// one is repeated if more mints arrive than were scripted. Every other
    /// route answers with an empty JSON object, which is all [`Fetch::get`]
    /// needs: the parsing above it is tested elsewhere.
    fn start(count: usize, mints: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a port");
        let address = listener.local_addr().expect("the bound address");
        let handle = std::thread::spawn(move || {
            let mut seen: Vec<Seen> = Vec::new();
            let mut minted = 0;
            while seen.len() < count {
                let (mut stream, _) = listener.accept().expect("a connection");
                let mut reader = BufReader::new(stream.try_clone().expect("a second handle"));
                // one request per connection in practice, since the answers say
                // to close, but reading until EOF keeps a reused connection from
                // stalling the count
                while seen.len() < count {
                    let Some(request) = read_request(&mut reader) else {
                        break;
                    };
                    let body = if request.is_mint() {
                        let body = mints
                            .get(minted)
                            .or_else(|| mints.last())
                            .expect("a scripted mint")
                            .clone();
                        minted += 1;
                        body
                    } else {
                        "{}".to_string()
                    };
                    answer(&mut stream, &body);
                    seen.push(request);
                }
            }
            seen
        });
        Server { address, handle }
    }

    fn service_url(&self) -> String {
        format!("http://{}/rest/services/Thing/FeatureServer", self.address)
    }

    fn token_url(&self) -> String {
        format!("http://{}{TOKEN_ROUTE}", self.address)
    }

    fn finish(self) -> Vec<Seen> {
        self.handle.join().expect("the server thread")
    }
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Seen> {
    let mut line = String::new();
    if reader.read_line(&mut line).expect("a request line") == 0 {
        return None;
    }
    let target = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();
    let mut authorization = None;
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).expect("a header");
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("x-esri-authorization") {
            authorization = Some(value.to_string());
        }
        if name.eq_ignore_ascii_case("content-length") {
            length = value.parse().expect("a content length");
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).expect("a body");
    Some(Seen {
        target,
        authorization,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn answer(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: \
         close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).expect("an answer");
    stream.flush().expect("a flushed answer");
}

fn client_credentials(server: &Server) -> Credentials {
    Credentials::ClientCredentials {
        token_url: server.token_url(),
        client_id: "an-app-id".to_string(),
        client_secret: SECRET.to_string(),
    }
}

#[test]
fn the_first_request_mints_and_carries_the_minted_token() {
    let server = Server::start(3, vec![mint("minted-token-1", 3600)]);
    let url = server.service_url();
    let fetch = HttpFetch::new(client_credentials(&server)).expect("a client");
    fetch.get(&url, &[]).expect("the first request");
    fetch.get(&url, &[]).expect("the second request");
    let seen = server.finish();

    let mints: Vec<&Seen> = seen.iter().filter(|request| request.is_mint()).collect();
    assert_eq!(mints.len(), 1, "a held token was minted twice");
    let body = &mints[0].body;
    for field in [
        "grant_type=client_credentials".to_string(),
        "client_id=an-app-id".to_string(),
        format!("client_secret={SECRET}"),
    ] {
        assert!(
            body.contains(&field),
            "the mint did not send {field}: {body}"
        );
    }

    let service: Vec<&Seen> = seen.iter().filter(|request| !request.is_mint()).collect();
    assert_eq!(service.len(), 2);
    for request in service {
        assert_eq!(
            request.authorization.as_deref(),
            Some("Bearer minted-token-1"),
            "{} did not carry the minted token",
            request.target
        );
    }
}

#[test]
fn a_token_that_expires_inside_the_margin_is_minted_again() {
    // 30 seconds is inside the 60 second re-mint margin, so the token is spent
    // the moment it arrives and every request buys a new one
    let server = Server::start(
        4,
        vec![mint("minted-token-1", 30), mint("minted-token-2", 30)],
    );
    let url = server.service_url();
    let fetch = HttpFetch::new(client_credentials(&server)).expect("a client");
    fetch.get(&url, &[]).expect("the first request");
    fetch.get(&url, &[]).expect("the second request");
    let seen = server.finish();

    let mints = seen.iter().filter(|request| request.is_mint()).count();
    assert_eq!(mints, 2, "an expiring token was not minted again");
    let carried: Vec<Option<&str>> = seen
        .iter()
        .filter(|request| !request.is_mint())
        .map(|request| request.authorization.as_deref())
        .collect();
    assert_eq!(
        carried,
        vec![Some("Bearer minted-token-1"), Some("Bearer minted-token-2")]
    );
}

#[test]
fn a_refused_mint_names_the_token_route_and_never_the_secret() {
    let refusal = r#"{"error":{"code":400,"error":"invalid_client_id","message":"Invalid client_id","details":[]}}"#;
    let server = Server::start(1, vec![refusal.to_string()]);
    let url = server.service_url();
    let token_url = server.token_url();
    let fetch = HttpFetch::new(client_credentials(&server)).expect("a client");
    let Err(refused) = fetch.get(&url, &[]) else {
        panic!("a refused mint let the request through unauthenticated");
    };
    let seen = server.finish();

    assert_eq!(seen.len(), 1, "the service was asked after the mint failed");
    assert!(seen[0].is_mint());
    let shown = refused.to_string();
    assert!(
        matches!(refused, verne_arcgis::ArcgisError::Service { .. }),
        "a refused mint is the service refusing: {refused:?}"
    );
    assert!(
        shown.contains(&token_url),
        "{shown} does not name the route"
    );
    assert!(
        shown.contains("Invalid client_id"),
        "{shown} does not say why"
    );
    assert!(
        !shown.contains(SECRET),
        "the error shows the secret: {shown}"
    );
    assert!(
        !format!("{refused:?}").contains(SECRET),
        "the error's Debug shows the secret"
    );
}

#[test]
fn a_token_the_operator_holds_is_sent_as_it_stands() {
    let server = Server::start(1, vec![mint("never-minted", 3600)]);
    let url = server.service_url();
    let fetch = HttpFetch::new(Credentials::Token("held-token".to_string())).expect("a client");
    fetch.get(&url, &[]).expect("the request");
    let seen = server.finish();

    assert_eq!(seen.len(), 1);
    assert!(
        !seen[0].is_mint(),
        "a held token still asked the token route"
    );
    assert_eq!(
        seen[0].authorization.as_deref(),
        Some("Bearer held-token"),
        "the held token was not sent as it stands"
    );
}

/// What the token route answers, per its reference: `expires_in` is seconds.
fn mint(token: &str, expires_in: u64) -> String {
    format!(r#"{{"access_token":"{token}","expires_in":{expires_in}}}"#)
}
