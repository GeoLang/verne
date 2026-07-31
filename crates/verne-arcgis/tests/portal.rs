//! Enumerating a canned portal. The pages here are the shape arcgis.com
//! answered with while this was written, so changing one is a claim about what
//! the sharing API sends.

mod common;

use common::{Call, Fake};
use serde_json::json;
use verne_arcgis::feature_services;

/// The portal the fixture routes hang off. The fake strips this, so the search
/// route arrives as `/search` whichever form the argument took.
const PORTAL: &str = "https://portal.invalid/sharing/rest";

/// Two pages, the first handing on a `nextStart` and the second ending the walk
/// with -1. The last item on the second page is registered content with no url.
fn fake() -> Fake {
    Fake::at(PORTAL).answering("/search", |params| {
        let start = params
            .iter()
            .find(|(name, _)| *name == "start")
            .map(|(_, value)| value.as_str())
            .expect("the search asked for a page");
        let page = match start {
            "1" => json!({
                "total": 103,
                "start": 1,
                "num": 100,
                "nextStart": 101,
                "results": [
                    {
                        "id": "aaa",
                        "owner": "gis_admin",
                        "title": "Wells",
                        "type": "Feature Service",
                        "url": "https://services.invalid/arcgis/rest/services/Wells/FeatureServer"
                    },
                    {
                        "id": "bbb",
                        "owner": "surveyor",
                        "title": "Parcels",
                        "type": "Feature Service",
                        "url": "https://services.invalid/arcgis/rest/services/Parcels/FeatureServer"
                    }
                ]
            }),
            "101" => json!({
                "total": 103,
                "start": 101,
                "num": 100,
                "nextStart": -1,
                "results": [
                    {
                        "id": "ccc",
                        "owner": "gis_admin",
                        "title": "Roads",
                        "type": "Feature Service",
                        "url": "https://services.invalid/arcgis/rest/services/Roads/FeatureServer"
                    },
                    {
                        "id": "ddd",
                        "owner": "gis_admin",
                        "title": "Registered with no route",
                        "type": "Feature Service"
                    }
                ]
            }),
            other => panic!("the search asked for start={other}"),
        };
        serde_json::to_vec(&page).expect("the fixture serialises")
    })
}

fn titles(portal: &str, owner: Option<&str>) -> Vec<String> {
    let fake = fake();
    feature_services(&fake, portal, owner)
        .expect("the fixture lists")
        .into_iter()
        .map(|service| service.title)
        .collect()
}

#[test]
fn both_pages_of_a_walked_search_come_back_in_order() {
    let fake = fake();
    let calls = fake.calls();
    let services = feature_services(&fake, PORTAL, None).expect("the fixture lists");
    let listed: Vec<(String, String, String)> = services
        .into_iter()
        .map(|service| (service.url, service.title, service.owner))
        .collect();
    assert_eq!(
        listed,
        [
            (
                "https://services.invalid/arcgis/rest/services/Wells/FeatureServer".to_string(),
                "Wells".to_string(),
                "gis_admin".to_string()
            ),
            (
                "https://services.invalid/arcgis/rest/services/Parcels/FeatureServer".to_string(),
                "Parcels".to_string(),
                "surveyor".to_string()
            ),
            (
                "https://services.invalid/arcgis/rest/services/Roads/FeatureServer".to_string(),
                "Roads".to_string(),
                "gis_admin".to_string()
            ),
        ]
    );
    // -1 ends the walk, so the second page is the last request made
    let calls = calls.borrow();
    let starts: Vec<&str> = calls
        .iter()
        .map(|call| call.param("start").expect("a page was asked for"))
        .collect();
    assert_eq!(starts, ["1", "101"]);
}

#[test]
fn every_page_is_a_get_for_a_hundred_json_results_off_the_search_route() {
    let fake = fake();
    let calls = fake.calls();
    feature_services(&fake, PORTAL, None).expect("the fixture lists");
    let calls = calls.borrow();
    assert_eq!(calls.len(), 2);
    for call in calls.iter() {
        assert_eq!(call.route, "/search");
        assert_eq!(call.method, "GET");
        assert_eq!(call.param("f"), Some("json"));
        assert_eq!(call.param("num"), Some("100"));
    }
}

#[test]
fn an_item_the_portal_gave_no_url_for_is_skipped() {
    let listed = titles(PORTAL, None);
    assert!(
        !listed
            .iter()
            .any(|title| title == "Registered with no route"),
        "{listed:?}"
    );
    assert_eq!(listed.len(), 3);
}

#[test]
fn the_type_is_asked_for_in_the_query_and_the_owner_filter_joins_it() {
    let unfiltered = query_of(None);
    assert_eq!(unfiltered, r#"type:"Feature Service""#);
    let filtered = query_of(Some("gis_admin"));
    assert_eq!(filtered, r#"type:"Feature Service" AND owner:"gis_admin""#);
}

/// The `q` the search was asked with, which is the only place the owner filter
/// may land: a portal narrows nothing verne passes beside it.
fn query_of(owner: Option<&str>) -> String {
    let fake = fake();
    let calls = fake.calls();
    feature_services(&fake, PORTAL, owner).expect("the fixture lists");
    let calls = calls.borrow();
    let first: &Call = calls.first().expect("a search was made");
    first.param("q").expect("a query was sent").to_string()
}

#[test]
fn a_portal_home_and_its_rest_root_ask_the_same_route() {
    assert_eq!(
        titles("https://portal.invalid", None),
        titles("https://portal.invalid/sharing/rest/", None)
    );
}
