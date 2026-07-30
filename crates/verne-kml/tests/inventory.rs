//! Verdicts on KML fixtures. These were checked against the real GeoLang
//! components, so a change here is a claim about what a component can hold.

use verne_core::{Item, ItemKind, Outcome, Source, Verdict};
use verne_kml::KmlSource;

/// Every fixture goes through here, so the invariants hold for all of them.
fn inventory(xml: &str) -> Vec<Item> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("fixture.kml");
    std::fs::write(&path, xml).expect("write fixture");
    let source = KmlSource::open(&path).expect("the fixture opens");
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
    let mut matching = items.iter().filter(|item| {
        item.kind == kind && (item.detail.contains(needle) || item.location.contains(needle))
    });
    let found = matching
        .next()
        .unwrap_or_else(|| panic!("no {kind} row mentioning {needle} in {items:#?}"));
    assert!(
        matching.next().is_none(),
        "more than one {kind} row mentioning {needle}"
    );
    found
}

fn doc(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<kml xmlns="http://www.opengis.net/kml/2.2" xmlns:gx="http://www.google.com/kml/ext/2.2">
  <Document>
    <name>Fixture</name>
{body}
  </Document>
</kml>
"#
    )
}

fn point(name: &str) -> String {
    format!(
        "<Placemark><name>{name}</name><Point><coordinates>1,2</coordinates></Point></Placemark>"
    )
}

#[test]
fn plain_points_are_faithful() {
    let body = format!(
        "<Folder><name>Wells</name>{}{}{}</Folder>",
        point("a"),
        point("b"),
        point("c")
    );
    let items = inventory(&doc(&body));
    let features = only(&items, ItemKind::FeatureCollection);
    assert_eq!(features.verdict.outcome(), Outcome::Faithful);
    assert_eq!(
        features.verdict.target().map(|t| t.component()),
        Some("ptolemy")
    );
    assert!(features.detail.contains("3 placemarks"));
}

#[test]
fn mixed_geometry_types_are_approximated() {
    let body = format!(
        "<Folder><name>Mixed</name>{}<Placemark><name>p</name><Polygon><outerBoundaryIs><LinearRing><coordinates>0,0 1,0 1,1 0,0</coordinates></LinearRing></outerBoundaryIs></Polygon></Placemark></Folder>",
        point("a")
    );
    let items = inventory(&doc(&body));
    let features = only(&items, ItemKind::FeatureCollection);
    assert_eq!(features.verdict.outcome(), Outcome::Approximated);
    assert!(features.verdict.shortfall().contains("geometry_type"));
}

#[test]
fn a_placemark_without_geometry_is_approximated() {
    let body = format!(
        "{}<Placemark><name>attributes only</name></Placemark>",
        point("a")
    );
    let items = inventory(&doc(&body));
    let features = only(&items, ItemKind::FeatureCollection);
    assert_eq!(features.verdict.outcome(), Outcome::Approximated);
    assert!(features.verdict.shortfall().contains("deletion"));
    assert!(features.detail.contains("1 with no geometry"));
}

#[test]
fn extrude_and_altitude_mode_are_approximated() {
    let body = "<Placemark><name>tower</name><extrude>1</extrude><altitudeMode>absolute</altitudeMode><Point><coordinates>1,2,30</coordinates></Point></Placemark>";
    let items = inventory(&doc(body));
    let features = only(&items, ItemKind::FeatureCollection);
    assert_eq!(features.verdict.outcome(), Outcome::Approximated);
    let shortfall = features.verdict.shortfall();
    assert!(shortfall.contains("extrude"), "{shortfall}");
    assert!(shortfall.contains("altitudeMode"), "{shortfall}");
}

#[test]
fn default_altitude_values_lose_nothing() {
    let body = "<Placemark><name>flat</name><extrude>0</extrude><altitudeMode>clampToGround</altitudeMode><Point><coordinates>1,2</coordinates></Point></Placemark>";
    let items = inventory(&doc(body));
    let features = only(&items, ItemKind::FeatureCollection);
    assert_eq!(features.verdict.outcome(), Outcome::Faithful);
}

#[test]
fn a_narrow_schema_field_is_approximated() {
    let body = r#"<Schema name="wells" id="wells"><SimpleField type="short" name="depth"></SimpleField><SimpleField type="string" name="operator"></SimpleField></Schema>"#;
    let items = inventory(&doc(body));
    let schema = only(&items, ItemKind::AttributeSchema);
    assert_eq!(schema.verdict.outcome(), Outcome::Approximated);
    assert!(schema.verdict.shortfall().contains("widths"));
}

#[test]
fn a_wide_schema_is_faithful() {
    let body = r#"<Schema name="wells" id="wells"><SimpleField type="string" name="operator"></SimpleField><SimpleField type="double" name="depth"></SimpleField><SimpleField type="bool" name="active"></SimpleField></Schema>"#;
    let items = inventory(&doc(body));
    let schema = only(&items, ItemKind::AttributeSchema);
    assert_eq!(schema.verdict.outcome(), Outcome::Faithful);
}

#[test]
fn a_ground_overlay_goes_to_terrano_with_losses() {
    let body = r#"<GroundOverlay><name>scan</name><Icon><href>scan.png</href></Icon><rotation>0</rotation><LatLonBox><north>1</north><south>0</south><east>1</east><west>0</west></LatLonBox></GroundOverlay>"#;
    let items = inventory(&doc(body));
    let overlay = only(&items, ItemKind::RasterOverlay);
    assert_eq!(overlay.verdict.outcome(), Outcome::Approximated);
    assert_eq!(
        overlay.verdict.target().map(|t| t.component()),
        Some("terrano")
    );
    let shortfall = overlay.verdict.shortfall();
    assert!(shortfall.contains("single band"), "{shortfall}");
    assert!(!shortfall.contains("rotation terms"), "{shortfall}");
}

#[test]
fn a_rotated_ground_overlay_names_the_rotation() {
    let body = r#"<GroundOverlay><name>scan</name><Icon><href>scan.png</href></Icon><rotation>45</rotation><LatLonBox><north>1</north><south>0</south><east>1</east><west>0</west></LatLonBox></GroundOverlay>"#;
    let items = inventory(&doc(body));
    let overlay = only(&items, ItemKind::RasterOverlay);
    let shortfall = overlay.verdict.shortfall();
    assert!(shortfall.contains("single band"), "{shortfall}");
    assert!(shortfall.contains("rotation terms"), "{shortfall}");
}

#[test]
fn a_region_with_lod_is_approximated_against_jung() {
    let body = r#"<Folder><name>Detail</name><Region><LatLonAltBox><north>1</north><south>0</south><east>1</east><west>0</west></LatLonAltBox><Lod><minLodPixels>128</minLodPixels></Lod></Region></Folder>"#;
    let items = inventory(&doc(body));
    let region = only(&items, ItemKind::ViewDependentDisplay);
    // GeoLang has scale-bounded rules, so this is not unsupported
    assert_eq!(region.verdict.outcome(), Outcome::Approximated);
    assert_eq!(region.verdict.target().map(|t| t.component()), Some("jung"));
    assert!(region.detail.contains("1 Region, 1 with Lod"));
}

#[test]
fn a_network_link_is_unsupported() {
    let body = r#"<NetworkLink><name>live</name><Link><href>https://example.invalid/feed.kml</href><refreshMode>onInterval</refreshMode></Link></NetworkLink>"#;
    let items = inventory(&doc(body));
    let link = only(&items, ItemKind::ExternalReference);
    assert_eq!(link.verdict.outcome(), Outcome::Unsupported);
    assert!(link.detail.contains("live"));
}

#[test]
fn a_screen_overlay_is_unsupported() {
    let body =
        r#"<ScreenOverlay><name>logo</name><Icon><href>logo.png</href></Icon></ScreenOverlay>"#;
    let items = inventory(&doc(body));
    let overlay = only(&items, ItemKind::ViewDependentDisplay);
    assert_eq!(overlay.verdict.outcome(), Outcome::Unsupported);
}

#[test]
fn a_balloon_style_is_reported_apart_from_the_symbology() {
    let body = r#"<Style id="s"><LineStyle><color>ff0000ff</color><width>2</width></LineStyle><BalloonStyle><text><![CDATA[<b>$[name]</b>]]></text></BalloonStyle></Style>"#;
    let items = inventory(&doc(body));
    let styling: Vec<&Item> = items
        .iter()
        .filter(|item| item.kind == ItemKind::Styling)
        .collect();
    assert_eq!(styling.len(), 2, "{styling:#?}");

    let line = only_matching(&items, ItemKind::Styling, "parts: LineStyle");
    assert_ne!(line.verdict.outcome(), Outcome::Unsupported);
    assert_eq!(line.verdict.target().map(|t| t.component()), Some("jung"));

    let balloon = only_matching(&items, ItemKind::Styling, "BalloonStyle");
    assert_eq!(balloon.verdict.outcome(), Outcome::Unsupported);
    assert_eq!(balloon.detail, "BalloonStyle");
}

#[test]
fn a_style_map_is_approximated_against_jung() {
    let body = r#"<StyleMap id="pair"><Pair><key>normal</key><styleUrl>#a</styleUrl></Pair><Pair><key>highlight</key><styleUrl>#b</styleUrl></Pair></StyleMap>"#;
    let items = inventory(&doc(body));
    let map = only_matching(&items, ItemKind::Styling, "states:");
    assert_eq!(map.verdict.outcome(), Outcome::Approximated);
    assert_eq!(map.verdict.target().map(|t| t.component()), Some("jung"));
    assert!(map.detail.contains("normal"));
    assert!(map.detail.contains("highlight"));
}

#[test]
fn a_model_has_no_home_and_a_track_is_approximated() {
    let body = r#"<Placemark><name>shed</name><Model><Location><longitude>1</longitude></Location></Model></Placemark>
    <Placemark><name>run</name><gx:Track><when>2026-01-01T00:00:00Z</when><gx:coord>1 2 0</gx:coord></gx:Track></Placemark>"#;
    let items = inventory(&doc(body));

    let mesh = only(&items, ItemKind::Mesh);
    assert_eq!(mesh.verdict.outcome(), Outcome::Unsupported);
    assert!(mesh.detail.contains("1 Model"));

    let mut tracks = items.iter().filter(|item| item.detail == "1 gx:Track");
    let track = tracks.next().expect("a gx:Track row");
    assert!(tracks.next().is_none(), "more than one gx:Track row");
    assert_eq!(track.kind, ItemKind::FeatureCollection);
    assert_eq!(track.verdict.outcome(), Outcome::Approximated);
    assert_eq!(
        track.verdict.target().map(|t| t.component()),
        Some("ptolemy")
    );
}

#[test]
fn repeated_unruled_elements_aggregate_into_one_row() {
    let look_at =
        "<LookAt><longitude>1</longitude><latitude>2</latitude><range>1000</range></LookAt>";
    let body = look_at.repeat(500);
    let items = inventory(&doc(&body));
    let saved_views: Vec<&Item> = items
        .iter()
        .filter(|item| item.detail.contains("LookAt"))
        .collect();
    assert_eq!(saved_views.len(), 1);
    assert_eq!(saved_views[0].detail, "500 LookAt");
    assert_eq!(saved_views[0].verdict.outcome(), Outcome::Unsupported);
}
