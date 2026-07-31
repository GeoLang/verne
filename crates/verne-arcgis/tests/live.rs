//! Reading a real feature service.
//!
//! Gated on `VERNE_ARCGIS_URL`, so verne's CI does not cover it. A fixture
//! proves the adapter agrees with what Esri documents; the failure this test
//! exists to catch is a real service answering something the docs do not
//! describe, and no canned response can see that. No URL is hardcoded: the
//! services worth pointing this at belong to whoever runs it.
//!
//! ```bash
//! export VERNE_ARCGIS_URL=https://host/arcgis/rest/services/Thing/FeatureServer
//! export VERNE_ARCGIS_TOKEN=<a token, when the service needs one>
//! cargo test -p verne-arcgis --test live -- --nocapture
//! ```

use verne_arcgis::{ArcgisSource, Credentials, TOKEN_VAR};
use verne_core::Source;

#[test]
fn a_live_feature_service_inventories_and_extracts() {
    let Ok(url) = std::env::var("VERNE_ARCGIS_URL") else {
        eprintln!(
            "skipping the live read: set VERNE_ARCGIS_URL to a feature service root (a URL \
             ending in /FeatureServer) and {TOKEN_VAR} when the service needs a token"
        );
        return;
    };
    // a public service needs none, so an unset token is not a skip
    let credentials = match std::env::var(TOKEN_VAR)
        .ok()
        .filter(|held| !held.is_empty())
    {
        Some(token) => Credentials::Token(token),
        None => Credentials::Anonymous,
    };
    let source = ArcgisSource::open(&url, credentials, None).expect("the service opens");
    eprintln!("{:?}", source.describe());

    let items = source.inventory().expect("the service inventories");
    assert!(!items.is_empty(), "a service that opened listed nothing");
    for item in &items {
        eprintln!("{} | {} | {}", item.location, item.kind, item.detail);
    }

    let directory = tempfile::tempdir().expect("tempdir");
    let extraction = source
        .extract(directory.path(), "verne-arcgis live test")
        .expect("the service extracts");
    eprintln!("{}", extraction.sidecar.log.counts().sentence());

    for plan in &extraction.sidecar.datasets {
        let Some(relative) = &plan.features else {
            continue;
        };
        let path = extraction.directory.join(relative);
        assert!(
            path.is_file(),
            "{} names {relative}, which was not written",
            plan.dataset.name
        );
    }
    for attachment in &extraction.sidecar.attachments {
        let path = extraction.directory.join(&attachment.file);
        assert!(
            path.is_file(),
            "{} names {}, which was not written",
            attachment.name,
            attachment.file
        );
    }
}
