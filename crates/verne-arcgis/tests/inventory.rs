//! Verdicts on a canned feature service. These say what GeoLang can hold of
//! what the REST API exposes, so a change here is a claim about a component.

mod common;

use common::{Fake, ROOT, logs_table, service_root, wells_layer};
use serde_json::json;
use verne_arcgis::{ArcgisError, ArcgisSource};
use verne_core::{Item, ItemKind, Outcome, Source, Verdict};

/// The fixture service, ready to open.
fn fake() -> Fake {
    Fake::new()
        .json("", service_root())
        .json("/0", wells_layer())
        .json("/1", logs_table())
        // the count is asked for as a count, not as a page verne counts itself
        .answering("/0/query", |params| {
            assert_eq!(
                params
                    .iter()
                    .find(|(name, _)| *name == "returnCountOnly")
                    .map(|(_, value)| value.as_str()),
                Some("true"),
                "the count query asked for features: {params:?}"
            );
            br#"{"count": 3}"#.to_vec()
        })
        .json("/1/query", json!({ "count": 2 }))
}

/// Every fixture goes through here, so the invariants hold for all of them.
fn inventory() -> Vec<Item> {
    let source = ArcgisSource::open_with(Box::new(fake()), ROOT).expect("the fixture opens");
    let items = source.inventory().expect("the fixture inventories");
    for item in &items {
        match &item.verdict {
            Verdict::Approximated { losses, .. } => {
                assert!(losses.count() >= 1, "{} names no loss", item.location);
                assert!(
                    !item.verdict.shortfall().is_empty(),
                    "{} has an empty shortfall",
                    item.location
                );
            }
            Verdict::Unsupported { reason } => {
                assert!(!reason.is_empty(), "{} gives no reason", item.location);
                assert_eq!(item.verdict.target(), None);
            }
            // unlike a file, a service can be asked about a feature it does not
            // have: no versioned data is beside the question, not a shortfall
            Verdict::NotApplicable { reason } => {
                assert!(!reason.is_empty(), "{} gives no reason", item.location);
                assert_eq!(item.verdict.target(), None);
            }
            Verdict::Faithful { .. } => {}
        }
    }
    items
}

fn only(items: &[Item], kind: ItemKind) -> &Item {
    let mut matching = items.iter().filter(|item| item.kind == kind);
    let found = matching
        .next()
        .unwrap_or_else(|| panic!("no {kind} row in {items:#?}"));
    assert!(matching.next().is_none(), "more than one {kind} row");
    found
}

fn only_matching<'a>(items: &'a [Item], kind: ItemKind, needle: &str) -> &'a Item {
    let mut matching = items
        .iter()
        .filter(|item| item.kind == kind && item.detail.contains(needle));
    let found = matching
        .next()
        .unwrap_or_else(|| panic!("no {kind} row mentioning {needle} in {items:#?}"));
    assert!(
        matching.next().is_none(),
        "more than one {kind} row mentioning {needle}"
    );
    found
}

#[test]
fn the_layer_and_the_table_each_get_a_row_with_the_count_the_service_gave() {
    let items = inventory();
    let wells = only_matching(&items, ItemKind::FeatureCollection, "Point");
    assert_eq!(wells.location, "Wells");
    assert!(wells.detail.contains("3 features"), "{}", wells.detail);
    assert!(wells.detail.contains("4 fields"), "{}", wells.detail);

    let logs = only_matching(&items, ItemKind::FeatureCollection, "table");
    assert_eq!(logs.location, "Logs");
    assert!(logs.detail.contains("2 rows"), "{}", logs.detail);
    // a table has no geometry and ptolemy's insert takes one
    assert_eq!(logs.verdict.outcome(), Outcome::Approximated);
    assert!(
        logs.verdict
            .shortfall()
            .contains("empty geometry collection"),
        "{}",
        logs.verdict.shortfall()
    );
}

/// A portal names its items by layer URL, so pointing verne at one reads that
/// layer alone, and a relationship whose other side is out of scope says so.
#[test]
fn a_layer_url_scopes_the_inventory_to_that_layer() {
    let source = ArcgisSource::open_with(Box::new(fake()), &format!("{ROOT}/0"))
        .expect("the scoped fixture opens");
    let items = source.inventory().expect("the scoped fixture inventories");
    let collections: Vec<&Item> = items
        .iter()
        .filter(|item| item.kind == ItemKind::FeatureCollection)
        .collect();
    assert_eq!(collections.len(), 1, "{items:#?}");
    assert_eq!(collections[0].location, "Wells");
    let relationship = only(&items, ItemKind::Relationship);
    assert!(
        relationship
            .verdict
            .shortfall()
            .contains("not among the layers verne was pointed at"),
        "{}",
        relationship.verdict.shortfall()
    );
    assert_eq!(
        source.describe().location,
        format!("{ROOT}/0"),
        "the report says what was pointed at"
    );
}

#[test]
fn open_reads_the_service_then_every_layer_and_its_count() {
    let fake = fake();
    let calls = fake.calls();
    ArcgisSource::open_with(Box::new(fake), ROOT).expect("the fixture opens");
    let routes: Vec<String> = calls
        .borrow()
        .iter()
        .map(|call| call.route.clone())
        .collect();
    assert_eq!(routes, ["", "/0", "/0/query", "/1", "/1/query"]);
    // f=json rides on every one of them, and an open never POSTs
    assert!(
        calls
            .borrow()
            .iter()
            .all(|call| call.param("f") == Some("json") && call.method == "GET"),
        "{routes:?}"
    );
}

#[test]
fn a_field_alias_is_kept_and_never_shown() {
    let items = inventory();
    let wells = only_matching(&items, ItemKind::FeatureCollection, "Point");
    let shortfall = wells.verdict.shortfall();
    assert!(shortfall.contains(r#"status "Status""#), "{shortfall}");
    assert!(shortfall.contains("never shown"), "{shortfall}");
}

#[test]
fn a_date_column_is_named_among_the_retyped_ones() {
    let items = inventory();
    let wells = only_matching(&items, ItemKind::FeatureCollection, "Point");
    let shortfall = wells.verdict.shortfall();
    assert!(
        shortfall.contains("drilled (esriFieldTypeDate)"),
        "{shortfall}"
    );
    assert!(shortfall.contains("nearest"), "{shortfall}");
    // the integer and float columns fit ptolemy exactly, so they are not in it
    assert!(!shortfall.contains("depth ("), "{shortfall}");
}

#[test]
fn the_reprojection_out_of_web_mercator_is_named_on_the_layer() {
    let items = inventory();
    let wells = only_matching(&items, ItemKind::FeatureCollection, "Point");
    let shortfall = wells.verdict.shortfall();
    // latestWkid wins over the wkid beside it, and the row says the original
    // comes back in a second pass rather than being lost
    assert!(shortfall.contains("EPSG:3857"), "{shortfall}");
    assert!(shortfall.contains("second pass"), "{shortfall}");
}

#[test]
fn a_coded_domain_lists_its_values_and_says_where_the_binding_goes() {
    let items = inventory();
    let coded = only_matching(&items, ItemKind::AttributeSchema, "StatusCodes\", coded");
    assert!(coded.detail.contains("1=Active"), "{}", coded.detail);
    assert!(coded.detail.contains("2=Plugged"), "{}", coded.detail);
    assert!(
        coded.detail.contains("used by Wells.status"),
        "{}",
        coded.detail
    );
    assert_eq!(coded.verdict.outcome(), Outcome::Approximated);
    let shortfall = coded.verdict.shortfall();
    assert!(shortfall.contains("domain_assignments"), "{shortfall}");
    assert!(shortfall.contains("Wells.status is bound"), "{shortfall}");
}

#[test]
fn a_range_domain_carries_both_bounds() {
    let items = inventory();
    let range = only_matching(&items, ItemKind::AttributeSchema, "DepthRange\", range");
    assert!(
        range.detail.contains("range: 0 to 5000"),
        "{}",
        range.detail
    );
    // used by the field and by the subtype assignment, and only the field's own
    // binding is a loss
    assert!(
        range.detail.contains("under subtype Active"),
        "{}",
        range.detail
    );
    assert_eq!(range.verdict.outcome(), Outcome::Approximated);
}

#[test]
fn the_subtype_row_names_the_default_code_and_the_domain_swap() {
    let items = inventory();
    let subtypes = only_matching(&items, ItemKind::AttributeSchema, "subtype on Wells.status");
    assert!(subtypes.detail.contains("1 Active"), "{}", subtypes.detail);
    assert!(subtypes.detail.contains("default 1"), "{}", subtypes.detail);
    let shortfall = subtypes.verdict.shortfall();
    assert!(
        shortfall.contains("which code is the default"),
        "{shortfall}"
    );
    assert!(shortfall.contains("DepthRange"), "{shortfall}");
    assert!(shortfall.contains("swapped for ids"), "{shortfall}");
}

#[test]
fn the_relationship_is_told_once_from_its_origin_end() {
    let items = inventory();
    let class = only(&items, ItemKind::Relationship);
    assert_eq!(class.location, "service root");
    assert_eq!(
        class.detail,
        "wells_logs: Wells.objectid -> Logs.well_id, one to many, composite"
    );
    let shortfall = class.verdict.shortfall();
    assert!(shortfall.contains("is_composite"), "{shortfall}");
    assert!(
        shortfall.contains("no forward or backward label"),
        "{shortfall}"
    );
}

#[test]
fn attachments_are_a_row_of_their_own() {
    let items = inventory();
    let attachments = only(&items, ItemKind::EmbeddedResource);
    assert_eq!(attachments.location, "Wells");
    assert_eq!(attachments.detail, "attachments on Wells");
    assert_eq!(attachments.verdict.outcome(), Outcome::Approximated);
    assert_eq!(
        attachments
            .verdict
            .target()
            .map(|target| target.component()),
        Some("ptolemy")
    );
}

/// The drawing info reaches ptolemy whole, so the row is approximated for one
/// reason only: nothing in the platform reads the format yet.
#[test]
fn the_drawing_info_goes_to_ptolemy_verbatim() {
    let items = inventory();
    let styling = only(&items, ItemKind::Styling);
    assert_eq!(styling.detail, "simple renderer");
    assert_eq!(styling.verdict.outcome(), Outcome::Approximated);
    assert_eq!(
        styling.verdict.target().map(|target| target.component()),
        Some("ptolemy")
    );
    let shortfall = styling.verdict.shortfall();
    assert!(shortfall.contains("carried verbatim"), "{shortfall}");
    assert!(shortfall.contains("reads that format yet"), "{shortfall}");
}

#[test]
fn a_service_with_no_versioned_data_is_beside_the_question() {
    let items = inventory();
    let versioning = only_matching(&items, ItemKind::Temporal, "versioning");
    assert_eq!(versioning.location, "service root");
    assert_eq!(versioning.verdict.outcome(), Outcome::NotApplicable);
    assert!(
        versioning.verdict.shortfall().contains("no versioned data"),
        "{}",
        versioning.verdict.shortfall()
    );
}

#[test]
fn an_error_object_in_a_successful_body_is_still_an_error() {
    let fake = Fake::new().json(
        "",
        json!({ "error": { "code": 499, "message": "Token Required" } }),
    );
    let Err(refused) = ArcgisSource::open_with(Box::new(fake), ROOT) else {
        panic!("a 499 error object opened as a service");
    };
    let ArcgisError::Service { code, message, .. } = &refused else {
        panic!("expected the service's own error: {refused}");
    };
    assert_eq!(*code, Some(499));
    assert_eq!(message, "Token Required");
}

#[test]
fn a_service_listing_nothing_is_a_failure_and_not_an_empty_inventory() {
    let fake = Fake::new().json(
        "",
        json!({
            "serviceDescription": "nothing here",
            "hasVersionedData": false,
            "layers": [],
            "tables": []
        }),
    );
    let Err(empty) = ArcgisSource::open_with(Box::new(fake), ROOT) else {
        panic!("a service listing nothing opened");
    };
    assert!(matches!(empty, ArcgisError::NothingFound), "{empty}");
}
