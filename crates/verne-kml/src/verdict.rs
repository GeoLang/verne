//! Turning tallies into verdicts. Every judgement about what GeoLang can hold
//! is in this file, so it can be argued with in one place.

use verne_core::{Item, ItemKind, Losses, Target, Verdict};

use crate::ArchiveEntry;
use crate::scan::{Scan, Style, Track};

/// Elements verne recognises but has no per-instance rule for. Reported by
/// count so nothing found in the file goes unmentioned. Every one of them has
/// no home at all: anything GeoLang can hold is inventoried per instance.
struct Rule {
    element: &'static str,
    kind: ItemKind,
    reason: &'static str,
}

const RULES: &[Rule] = &[
    Rule {
        element: "Model",
        kind: ItemKind::Mesh,
        reason: "a COLLADA mesh placed at a point: not a geometry ptolemy stores, and interiora holds building and indoor models rather than arbitrary meshes",
    },
    Rule {
        element: "PhotoOverlay",
        kind: ItemKind::RasterOverlay,
        reason: "an image pinned to a camera frustum; terrano registers rasters to the ground, not to a viewpoint",
    },
    Rule {
        element: "gx:Tour",
        kind: ItemKind::ViewDependentDisplay,
        reason: "a scripted camera flight, which is presentation rather than data",
    },
    Rule {
        element: "LookAt",
        kind: ItemKind::ViewDependentDisplay,
        reason: "a saved viewpoint with tilt, range and heading; GeoLang stores no camera, so a reader opens on the data extent instead",
    },
    Rule {
        element: "Camera",
        kind: ItemKind::ViewDependentDisplay,
        reason: "a saved camera position; GeoLang stores no camera, so a reader opens on the data extent instead",
    },
    Rule {
        element: "NetworkLinkControl",
        kind: ItemKind::ExternalReference,
        reason: "refresh instructions for a live link, which only mean something to the server that sent them",
    },
];

pub fn is_unruled(element: &str) -> bool {
    RULES.iter().any(|rule| rule.element == element)
}

/// SimpleField types narrower than a JSON number. `double`, `bool` and `string`
/// survive a JSONB round trip; these do not.
const NARROW_TYPES: &[&str] = &["int", "uint", "short", "ushort", "float"];

/// Style parts jung draws as they stand: a stroke, a fill, and an icon.
const CARRIED_PARTS: &[&str] = &["LineStyle", "PolyStyle", "IconStyle"];

const PART_LOSSES: &[(&str, &str)] = &[(
    "LabelStyle",
    "label scale multiplies an unstated base size, so text will not come out at the same size",
)];

/// Parts that dress a viewer rather than a map. These have no target at all, so
/// they are reported on their own row instead of as a loss against jung.
const CHROME_PARTS: &[(&str, &str)] = &[
    (
        "BalloonStyle",
        "the balloon template is popup content written in HTML, not symbology, and has nowhere to live",
    ),
    (
        "ListStyle",
        "list item icons and folder open state belong to a layer panel, which is not symbology either",
    ),
];

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// A verdict is faithful only when nothing was found to lose, so a new loss
/// cannot be added without downgrading the verdict that reports it.
fn verdict_for(target: Target, losses: Vec<String>) -> Verdict {
    match losses.split_first() {
        None => Verdict::faithful(target),
        Some((first, rest)) => {
            Verdict::approximated(target, Losses::one(first.clone()).and_all(rest.to_vec()))
        }
    }
}

pub fn items(scan: &Scan, archive: &[ArchiveEntry]) -> Vec<Item> {
    let root = scan.root_path();
    let mut items = Vec::new();

    document_metadata(scan, &root, &mut items);
    hierarchy(scan, &root, &mut items);
    features(scan, &mut items);
    tracks(scan, &mut items);
    schemas(scan, &mut items);
    extended_data(scan, &root, &mut items);
    styles(scan, &mut items);
    temporal(scan, &root, &mut items);
    network_links(scan, &root, &mut items);
    overlays(scan, &root, &mut items);
    regions(scan, &root, &mut items);
    embedded(archive, &mut items);
    unruled(scan, &root, &mut items);

    items
}

fn document_metadata(scan: &Scan, root: &str, items: &mut Vec<Item>) {
    if scan.doc_name.is_none() && !scan.doc_description {
        return;
    }
    let detail = match (&scan.doc_name, scan.doc_description) {
        (Some(name), true) => format!("name \"{name}\", with a description"),
        (Some(name), false) => format!("name \"{name}\""),
        (None, _) => "description only, no name".to_string(),
    };
    let verdict = if scan.doc_description {
        Verdict::approximated(
            Target::Ptolemy,
            Losses::one(
                "the description is HTML written for a balloon popup; it is kept as text and nothing renders it",
            ),
        )
    } else {
        Verdict::faithful(Target::Ptolemy)
    };
    items.push(Item::new(root, ItemKind::Metadata, detail, verdict));
}

fn hierarchy(scan: &Scan, root: &str, items: &mut Vec<Item>) {
    if scan.folders == 0 {
        return;
    }
    let detail = format!(
        "{} folders, nested {} deep",
        scan.folders,
        scan.depth.saturating_sub(1).max(1)
    );
    items.push(Item::new(
        root,
        ItemKind::Hierarchy,
        detail,
        Verdict::approximated(
            Target::Ptolemy,
            Losses::one(
                "the tree survives only as a folder path recorded on each feature, so there is no group to toggle or reorder",
            )
            .and("per-folder visibility and open state are dropped"),
        ),
    ));
}

fn features(scan: &Scan, items: &mut Vec<Item>) {
    for (index, container) in scan.containers.iter().enumerate() {
        if container.placemarks == 0 {
            continue;
        }
        let breakdown: Vec<String> = container
            .geometries
            .iter()
            .map(|(kind, count)| format!("{count} {kind}"))
            .collect();
        let mut detail = format!(
            "{} placemark{}",
            container.placemarks,
            plural(container.placemarks)
        );
        let own_row: Vec<String> = container
            .own_row_geometry
            .iter()
            .map(|(kind, count)| format!("{count} {kind}"))
            .collect();
        if !breakdown.is_empty() {
            detail.push_str(&format!(" ({})", breakdown.join(", ")));
        }
        if !own_row.is_empty() {
            detail.push_str(&format!(", {} reported below", own_row.join(", ")));
        }
        if container.without_geometry > 0 {
            detail.push_str(&format!(
                ", {} with no geometry",
                container.without_geometry
            ));
        }
        // a mixed container is carried now: a ptolemy dataset can declare the
        // geometry type 'geometry', so it no longer has to split by type
        let mut losses = Vec::new();
        if container.without_geometry > 0 {
            losses.push(format!(
                "{} placemark{} carr{} no geometry; ptolemy's geometry column accepts null but a null geometry there records a deletion, so attribute-only placemarks need a convention of their own",
                container.without_geometry,
                plural(container.without_geometry),
                if container.without_geometry == 1 { "ies" } else { "y" }
            ));
        }
        if !container.altitude_hints.is_empty() {
            let hints: Vec<&str> = container
                .altitude_hints
                .iter()
                .map(String::as_str)
                .collect();
            losses.push(format!(
                "{} {} a 3D renderer how to place the geometry; ptolemy keeps the coordinates, including Z, but nothing extrudes, clamps or drapes them",
                hints.join(" and "),
                if hints.len() == 1 { "tells" } else { "tell" }
            ));
        }
        items.push(Item::new(
            scan.path(index),
            ItemKind::FeatureCollection,
            detail,
            verdict_for(Target::Ptolemy, losses),
        ));
    }
}

/// A track has a home now: ptolemy takes a name and an array of timed points,
/// derives the period from them, and stores both on stock PostGIS as JSONB and
/// on MobilityDB as a tgeompoint. What is left is what that row does not carry.
fn tracks(scan: &Scan, items: &mut Vec<Item>) {
    for track in &scan.tracks {
        let location = match track.container {
            Some(index) => scan.path(index),
            None => scan.root_path(),
        };
        items.push(Item::new(
            location,
            ItemKind::FeatureCollection,
            track_detail(track),
            verdict_for(Target::Ptolemy, track_losses(track)),
        ));
    }
}

fn track_detail(track: &Track) -> String {
    let element = if track.multi {
        "gx:MultiTrack"
    } else {
        "gx:Track"
    };
    let name = match &track.name {
        Some(name) => format!(" \"{name}\""),
        None => String::new(),
    };
    let mut detail = format!("{element}{name}, ");
    if track.multi {
        detail.push_str(&format!(
            "{} track{}, ",
            track.segments,
            plural(track.segments)
        ));
    }
    detail.push_str(&format!(
        "{} timed sample{}",
        track.samples,
        plural(track.samples)
    ));
    detail
}

fn track_losses(track: &Track) -> Vec<String> {
    let mut losses = vec![
        "a trajectory holds a name, its timed points and their period, and no feature_id, so the placemark's attributes, style and folder path stay on a separate feature with nothing joining the two".to_string(),
        "only create, list and get run on stock PostGIS: speed, distance, position at a time, simplify and nearest approach are MobilityDB functions, so without that extension the samples are stored and read back as a line, a point count and a period, and nothing computes on them".to_string(),
    ];
    if track.multi {
        losses.push(
            "each gx:Track becomes a trajectory of its own, since ptolemy has no group of trajectories, so the gap KML models between consecutive tracks is only implied by their periods".to_string(),
        );
    }
    if track.altitude {
        losses.push(
            "a gx:coord carries an altitude, and a trajectory point is a longitude, a latitude and a timestamp, so the third value is dropped".to_string(),
        );
    }
    if track.angles {
        losses.push(
            "gx:angles gives each sample a heading, tilt and roll, which a trajectory point has no room for".to_string(),
        );
    }
    if track.array_data {
        losses.push(
            "gx:SimpleArrayData columns are one value per sample, and a trajectory carries no properties of its own".to_string(),
        );
    }
    if track.partial_times {
        losses.push(
            "KML allows a sample time such as 1997, and each point's timestamp is read as a timestamptz, so a partial date widens to an instant and nothing records that the precision was a year".to_string(),
        );
    }
    losses
}

fn schemas(scan: &Scan, items: &mut Vec<Item>) {
    for schema in &scan.schemas {
        let label = schema
            .name
            .clone()
            .or_else(|| schema.id.clone())
            .unwrap_or_else(|| "unnamed".to_string());
        let fields: Vec<String> = schema
            .fields
            .iter()
            .map(|field| format!("{}:{}", field.name, field.kind))
            .collect();
        let detail = format!(
            "Schema \"{}\", {} fields ({})",
            label,
            fields.len(),
            fields.join(", ")
        );
        let mut losses = Vec::new();
        let widths: Vec<&str> = schema
            .fields
            .iter()
            .map(|field| field.kind.as_str())
            .filter(|kind| NARROW_TYPES.contains(kind))
            .collect();
        if !widths.is_empty() {
            losses.push(format!(
                "the declared widths ({}) become plain JSON numbers in ptolemy's properties column, so nothing keeps the width or enforces its range",
                widths.join(", ")
            ));
        }
        if schema.fields.iter().any(|field| field.display_name) {
            losses.push(
                "SimpleField displayName is a human label for a field; ptolemy's dataset_schemas takes free-form JSON so it can be carried, but nothing reads it"
                    .to_string(),
            );
        }
        items.push(Item::new(
            scan.root_path(),
            ItemKind::AttributeSchema,
            detail,
            verdict_for(Target::Ptolemy, losses),
        ));
    }
}

fn extended_data(scan: &Scan, root: &str, items: &mut Vec<Item>) {
    if scan.data_placemarks == 0 && scan.schema_data == 0 {
        return;
    }
    let detail = format!(
        "{} placemark{} carry ExtendedData, {} distinct Data key{}, {} SchemaData block{}",
        scan.data_placemarks,
        plural(scan.data_placemarks),
        scan.data_keys.len(),
        plural(scan.data_keys.len()),
        scan.schema_data,
        plural(scan.schema_data)
    );
    items.push(Item::new(
        root,
        ItemKind::AttributeSchema,
        detail,
        Verdict::faithful(Target::Ptolemy),
    ));
}

fn styles(scan: &Scan, items: &mut Vec<Item>) {
    for style in &scan.styles {
        let location = style_location(scan, style.container, "Style", style.id.as_deref());
        let cartographic: Vec<&str> = style
            .parts
            .iter()
            .map(String::as_str)
            .filter(|part| !CHROME_PARTS.iter().any(|(chrome, _)| chrome == part))
            .collect();
        if !cartographic.is_empty() {
            let mut detail = format!("parts: {}", cartographic.join(", "));
            if let Some(href) = &style.icon_href {
                detail.push_str(&format!("; icon {href}"));
            }
            items.push(Item::new(
                location.clone(),
                ItemKind::Styling,
                detail,
                style_verdict(style, &cartographic),
            ));
        }
        for (part, reason) in CHROME_PARTS {
            if style.parts.iter().any(|held| held == part) {
                items.push(Item::new(
                    location.clone(),
                    ItemKind::Styling,
                    (*part).to_string(),
                    Verdict::unsupported(*reason),
                ));
            }
        }
    }
    for map in &scan.style_maps {
        let location = style_location(scan, map.container, "StyleMap", map.id.as_deref());
        let detail = format!("states: {}", map.keys.join(", "));
        let mut losses = Losses::one(
            "only the normal state becomes a symbol; the highlight state fires on hover, which is an interaction rather than symbology",
        );
        if !map.keys.iter().any(|key| key == "normal") {
            losses = losses.and("there is no normal state, so nothing supplies the default symbol");
        }
        items.push(Item::new(
            location,
            ItemKind::Styling,
            detail,
            Verdict::approximated(Target::Jung, losses),
        ));
    }
}

fn style_location(
    scan: &Scan,
    container: Option<usize>,
    element: &str,
    id: Option<&str>,
) -> String {
    let base = match container {
        Some(index) => scan.path(index),
        None => scan.root_path(),
    };
    match id {
        Some(id) => format!("{base}/{element}#{id}"),
        None => format!("{base}/{element} (inline)"),
    }
}

fn style_verdict(style: &Style, cartographic: &[&str]) -> Verdict {
    let mut losses: Vec<String> = Vec::new();
    for (part, loss) in PART_LOSSES {
        if cartographic.contains(part) {
            losses.push((*loss).to_string());
        }
    }
    if let Some(href) = &style.icon_href
        && (href.starts_with("http://") || href.starts_with("https://"))
    {
        losses.push(format!(
                "the icon is fetched from {href}, so the symbol depends on a server outside the data, and on that image being one the operator may redistribute"
            ));
    }
    // hotSpot and IconStyle heading are carried: jung gained an icon anchor, a
    // pixel offset and a clockwise rotation, so both map straight across.
    if style.random_color {
        losses.push(
            "colorMode random picks a colour per feature at draw time, which no fixed symbol reproduces".to_string(),
        );
    }
    debug_assert!(
        !losses.is_empty() || cartographic.iter().all(|part| CARRIED_PARTS.contains(part)),
        "a style with an unlisted part must produce a loss"
    );
    verdict_for(Target::Jung, losses)
}

fn temporal(scan: &Scan, root: &str, items: &mut Vec<Item>) {
    if scan.timestamps == 0 && scan.timespans == 0 {
        return;
    }
    let detail = format!("{} TimeStamp, {} TimeSpan", scan.timestamps, scan.timespans);
    // a TimeSpan is carried now: a feature version has a half-open valid range
    // and the features endpoint filters on it
    let mut losses = Vec::new();
    if scan.timestamps > 0 {
        losses.push(
            "a TimeStamp is an instant, and a valid range is the nearest thing to it, so an open-ended range reads as valid from that moment onwards rather than at it".to_string(),
        );
    }
    if scan.partial_times {
        losses.push(
            "KML allows partial dates such as 1997 or 1997-07, and a timestamptz cannot record that the precision was a year or a month".to_string(),
        );
    }
    items.push(Item::new(
        root,
        ItemKind::Temporal,
        detail,
        verdict_for(Target::Ptolemy, losses),
    ));
}

fn network_links(scan: &Scan, root: &str, items: &mut Vec<Item>) {
    for link in &scan.network_links {
        let mut detail = format!(
            "{} -> {}",
            link.name.as_deref().unwrap_or("unnamed"),
            link.href.as_deref().unwrap_or("no href")
        );
        if !link.refresh.is_empty() {
            detail.push_str(&format!(" ({})", link.refresh.join(", ")));
        }
        items.push(Item::new(
            root,
            ItemKind::ExternalReference,
            detail,
            Verdict::unsupported(
                "verne does not follow external links by design, and GeoLang has no live remote layer, so whatever this serves is outside the inventory and outside any conversion",
            ),
        ));
    }
}

fn overlays(scan: &Scan, root: &str, items: &mut Vec<Item>) {
    for overlay in &scan.ground_overlays {
        let mut detail = format!(
            "{} -> {}",
            overlay.name.as_deref().unwrap_or("unnamed"),
            overlay.href.as_deref().unwrap_or("no href")
        );
        if overlay.lat_lon_box {
            detail.push_str(", LatLonBox");
        }
        if overlay.quad {
            detail.push_str(", gx:LatLonQuad");
        }
        // colour is carried now: terrano gained a multi-band raster and writes an
        // RGB or RGBA GeoTIFF, and an axis-aligned LatLonBox in EPSG:4326 maps
        // exactly onto its origin and pixel scale, so nothing is resampled.
        let mut losses = Losses::one(
            "verne does not fetch the image, so its size, depth and internal georeferencing are unverified",
        );
        if overlay.quad {
            losses = losses.and(
                "a gx:LatLonQuad is a projective warp between four corners, which a north-up raster cannot express",
            );
        }
        if overlay.rotated {
            losses = losses.and(
                "the box is rotated about its centre, and terrano's GeoTIFF carries an origin and a pixel scale with no rotation terms, so the pixels have to be resampled north-up",
            );
        }
        if overlay.tinted {
            losses = losses.and("the colour tint and alpha are draw-time settings, not content");
        }
        if overlay.draw_order {
            losses = losses.and("drawOrder stacks overlays at draw time, which is a viewer rule");
        }
        items.push(Item::new(
            root,
            ItemKind::RasterOverlay,
            detail,
            Verdict::approximated(Target::Terrano, losses),
        ));
    }
    if scan.screen_overlays > 0 {
        items.push(Item::new(
            root,
            ItemKind::ViewDependentDisplay,
            format!("{} ScreenOverlay", scan.screen_overlays),
            Verdict::unsupported(
                "screen furniture anchored to the viewport, a logo or legend rather than anything on the map; it is neither data nor symbology",
            ),
        ));
    }
}

fn regions(scan: &Scan, root: &str, items: &mut Vec<Item>) {
    if scan.regions == 0 {
        return;
    }
    items.push(Item::new(
        root,
        ItemKind::ViewDependentDisplay,
        format!("{} Region, {} with Lod", scan.regions, scan.lods),
        Verdict::approximated(
            Target::Jung,
            Losses::one(
                "a jung rule and a ptolemy symbology rule both bound visibility, but by zoom level and by scale, whereas Lod switches on how many pixels the region covers on screen, so the crossover has to be picked rather than carried across",
            )
            .and("a Region also gates on its own extent coming into view, which a zoom or scale range cannot express"),
        ),
    ));
}

fn embedded(archive: &[ArchiveEntry], items: &mut Vec<Item>) {
    if archive.is_empty() {
        return;
    }
    let bytes: u64 = archive.iter().map(|entry| entry.bytes).sum();
    let names: Vec<&str> = archive.iter().map(|entry| entry.name.as_str()).collect();
    items.push(Item::new(
        "kmz archive",
        ItemKind::EmbeddedResource,
        format!("{} files, {} bytes: {}", names.len(), bytes, names.join(", ")),
        // an attachment can belong to a dataset now, so a style's icon has a
        // carrier without inventing a feature to hang it on
        Verdict::approximated(
            Target::Ptolemy,
            Losses::one(
                "the archive addresses these by relative path from the KML, so every href has to be rewritten to an attachment URL",
            ),
        ),
    ));
}

fn unruled(scan: &Scan, root: &str, items: &mut Vec<Item>) {
    for (element, count) in &scan.unruled {
        let Some(rule) = RULES.iter().find(|rule| rule.element == element) else {
            continue;
        };
        items.push(Item::new(
            root,
            rule.kind,
            format!("{count} {element}"),
            Verdict::unsupported(rule.reason),
        ));
    }
}
