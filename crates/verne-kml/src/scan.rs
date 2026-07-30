//! One pass over the XML, tallying what the document carries. No verdicts here.

use std::collections::{BTreeMap, BTreeSet};

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use crate::KmlError;

const STYLE_PARTS: &[&str] = &[
    "IconStyle",
    "LabelStyle",
    "LineStyle",
    "PolyStyle",
    "BalloonStyle",
    "ListStyle",
];

const GEOMETRIES: &[&str] = &[
    "Point",
    "LineString",
    "Polygon",
    "LinearRing",
    "MultiGeometry",
];

/// Placemark geometry that gets a row of its own. Such a placemark still has
/// geometry, so it must not also be counted as attribute-only, and the altitude
/// settings inside it belong to that row rather than to the container.
const OWN_ROW_GEOMETRY: &[&str] = &["Model", "gx:Track", "gx:MultiTrack"];

/// Elements that open a track. A gx:Track inside a gx:MultiTrack is a segment
/// of it rather than a track of its own.
const TRACK_ELEMENTS: &[&str] = &["gx:Track", "gx:MultiTrack"];

#[derive(Debug)]
pub struct Container {
    pub element: String,
    pub name: Option<String>,
    pub parent: Option<usize>,
    pub placemarks: usize,
    pub geometries: BTreeMap<String, usize>,
    /// Geometry reported on its own row, tallied here so a container holding
    /// only such placemarks does not read as though everything in it is fine.
    pub own_row_geometry: BTreeMap<String, usize>,
    pub without_geometry: usize,
    pub altitude_hints: BTreeSet<String>,
}

#[derive(Debug)]
pub struct Style {
    pub id: Option<String>,
    pub container: Option<usize>,
    pub parts: BTreeSet<String>,
    pub icon_href: Option<String>,
    pub hot_spot: bool,
    pub icon_heading: bool,
    pub random_color: bool,
}

#[derive(Debug)]
pub struct StyleMap {
    pub id: Option<String>,
    pub container: Option<usize>,
    pub keys: Vec<String>,
}

#[derive(Debug)]
pub struct Field {
    pub name: String,
    pub kind: String,
    pub display_name: bool,
}

#[derive(Debug)]
pub struct Schema {
    pub id: Option<String>,
    pub name: Option<String>,
    pub fields: Vec<Field>,
}

/// A gx:Track or gx:MultiTrack: the timed samples of one moving thing.
#[derive(Debug)]
pub struct Track {
    pub container: Option<usize>,
    /// Name of the placemark holding it, which is all a trajectory row carries.
    pub name: Option<String>,
    pub multi: bool,
    /// How many gx:Track it holds, one unless it is a gx:MultiTrack.
    pub segments: usize,
    pub samples: usize,
    /// A gx:coord with a third value that is not zero.
    pub altitude: bool,
    pub angles: bool,
    pub array_data: bool,
    pub partial_times: bool,
}

#[derive(Debug)]
pub struct NetworkLink {
    pub name: Option<String>,
    pub href: Option<String>,
    pub refresh: Vec<String>,
}

#[derive(Debug)]
pub struct GroundOverlay {
    pub name: Option<String>,
    pub href: Option<String>,
    pub lat_lon_box: bool,
    pub quad: bool,
    pub rotated: bool,
    pub tinted: bool,
    pub draw_order: bool,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub root_seen: bool,
    pub doc_name: Option<String>,
    pub doc_description: bool,
    pub containers: Vec<Container>,
    pub folders: usize,
    pub depth: usize,
    pub styles: Vec<Style>,
    pub style_maps: Vec<StyleMap>,
    pub schemas: Vec<Schema>,
    pub data_keys: BTreeSet<String>,
    pub data_placemarks: usize,
    pub schema_data: usize,
    pub timestamps: usize,
    pub timespans: usize,
    pub partial_times: bool,
    pub tracks: Vec<Track>,
    pub network_links: Vec<NetworkLink>,
    pub ground_overlays: Vec<GroundOverlay>,
    pub screen_overlays: usize,
    pub regions: usize,
    pub lods: usize,
    pub unruled: BTreeMap<String, usize>,
}

impl Scan {
    /// Path of a container in the document's own terms, e.g.
    /// `Document[Sites]/Folder[Wells]`.
    pub fn path(&self, mut index: usize) -> String {
        let mut parts = Vec::new();
        loop {
            let container = &self.containers[index];
            parts.push(match &container.name {
                Some(name) => format!("{}[{}]", container.element, name),
                None => container.element.clone(),
            });
            match container.parent {
                Some(parent) => index = parent,
                None => break,
            }
        }
        parts.reverse();
        parts.join("/")
    }

    /// Path of the innermost container, for items that hang off the document
    /// rather than off a folder.
    pub fn root_path(&self) -> String {
        if self.containers.is_empty() {
            "kml".to_string()
        } else {
            self.path(0)
        }
    }
}

#[derive(Default)]
struct Walk {
    scan: Scan,
    elements: Vec<String>,
    open_containers: Vec<usize>,
    in_placemark: bool,
    placemark_has_geometry: bool,
    style: Option<usize>,
    style_map: Option<usize>,
    schema: Option<usize>,
    network_link: Option<usize>,
    overlay: Option<usize>,
    track: Option<usize>,
    /// A placemark names its geometry before or after it, so the name is held
    /// here and handed to the tracks opened inside the placemark at its end.
    placemark_name: Option<String>,
    placemark_tracks: usize,
    text: String,
}

pub fn scan(xml: &str) -> Result<Scan, KmlError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut walk = Walk::default();
    loop {
        match reader.read_event()? {
            Event::Start(e) => walk.start(&e)?,
            Event::End(_) => walk.end(),
            Event::Empty(e) => {
                walk.start(&e)?;
                walk.end();
            }
            Event::Text(e) => walk.text.push_str(&e.decode()?),
            Event::CData(e) => walk.text.push_str(&String::from_utf8_lossy(e.as_ref())),
            Event::Eof => break,
            _ => {}
        }
    }
    if !walk.scan.root_seen {
        return Err(KmlError::NotKml);
    }
    // quick_xml reaches Eof without objecting to elements left open, and a
    // truncated file must not be reported as a source verne read in full.
    if !walk.elements.is_empty() {
        return Err(KmlError::Truncated(walk.elements.join("/")));
    }
    Ok(walk.scan)
}

impl Walk {
    fn parent(&self) -> &str {
        self.elements
            .len()
            .checked_sub(2)
            .map(|i| self.elements[i].as_str())
            .unwrap_or("")
    }

    fn ancestor(&self, name: &str) -> bool {
        self.elements.iter().any(|e| e == name)
    }

    fn inside_own_row_geometry(&self) -> bool {
        OWN_ROW_GEOMETRY.iter().any(|name| self.ancestor(name))
    }

    fn container(&mut self) -> usize {
        match self.open_containers.last() {
            Some(index) => *index,
            None => {
                // a placemark outside any Document or Folder still lives somewhere
                self.push_container("kml".to_string());
                *self.open_containers.last().expect("just pushed")
            }
        }
    }

    fn push_container(&mut self, element: String) {
        let parent = self.open_containers.last().copied();
        self.scan.containers.push(Container {
            element,
            name: None,
            parent,
            placemarks: 0,
            geometries: BTreeMap::new(),
            own_row_geometry: BTreeMap::new(),
            without_geometry: 0,
            altitude_hints: BTreeSet::new(),
        });
        self.open_containers.push(self.scan.containers.len() - 1);
        self.scan.depth = self.scan.depth.max(self.open_containers.len());
    }

    fn start(&mut self, e: &BytesStart<'_>) -> Result<(), KmlError> {
        let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
        let parent = self.elements.last().cloned().unwrap_or_default();
        self.text.clear();

        if crate::verdict::is_unruled(&name) {
            *self.scan.unruled.entry(name.clone()).or_insert(0) += 1;
        }
        if TRACK_ELEMENTS.contains(&name.as_str()) {
            self.open_track(&name);
        }

        match name.as_str() {
            "kml" => self.scan.root_seen = true,
            "Document" | "Folder" => {
                if name == "Folder" {
                    self.scan.folders += 1;
                }
                self.push_container(name.clone());
            }
            "Placemark" => {
                self.in_placemark = true;
                self.placemark_has_geometry = false;
                self.placemark_name = None;
                self.placemark_tracks = self.scan.tracks.len();
                let index = self.container();
                self.scan.containers[index].placemarks += 1;
            }
            "Style" => {
                self.scan.styles.push(Style {
                    id: attr(e, "id")?,
                    container: self.open_containers.last().copied(),
                    parts: BTreeSet::new(),
                    icon_href: None,
                    hot_spot: false,
                    icon_heading: false,
                    random_color: false,
                });
                self.style = Some(self.scan.styles.len() - 1);
            }
            "StyleMap" => {
                self.scan.style_maps.push(StyleMap {
                    id: attr(e, "id")?,
                    container: self.open_containers.last().copied(),
                    keys: Vec::new(),
                });
                self.style_map = Some(self.scan.style_maps.len() - 1);
            }
            "hotSpot" => {
                if let Some(index) = self.style {
                    self.scan.styles[index].hot_spot = true;
                }
            }
            "Schema" => {
                self.scan.schemas.push(Schema {
                    id: attr(e, "id")?,
                    name: attr(e, "name")?,
                    fields: Vec::new(),
                });
                self.schema = Some(self.scan.schemas.len() - 1);
            }
            "SimpleField" => {
                if let Some(index) = self.schema {
                    self.scan.schemas[index].fields.push(Field {
                        name: attr(e, "name")?.unwrap_or_default(),
                        kind: attr(e, "type")?.unwrap_or_default(),
                        display_name: false,
                    });
                }
            }
            // ExtendedData inside a track holds per-sample columns rather than
            // properties of the placemark, so the track row reports it instead.
            "ExtendedData" => {
                if self.in_placemark && self.track.is_none() {
                    self.scan.data_placemarks += 1;
                }
            }
            "Data" => {
                if let Some(key) = attr(e, "name")? {
                    self.scan.data_keys.insert(key);
                }
            }
            "SchemaData" => {
                if self.track.is_none() {
                    self.scan.schema_data += 1;
                }
            }
            "gx:angles" => {
                if let Some(index) = self.track {
                    self.scan.tracks[index].angles = true;
                }
            }
            "gx:SimpleArrayData" => {
                if let Some(index) = self.track {
                    self.scan.tracks[index].array_data = true;
                }
            }
            "TimeStamp" => self.scan.timestamps += 1,
            "TimeSpan" => self.scan.timespans += 1,
            "NetworkLink" => {
                self.scan.network_links.push(NetworkLink {
                    name: None,
                    href: None,
                    refresh: Vec::new(),
                });
                self.network_link = Some(self.scan.network_links.len() - 1);
            }
            "GroundOverlay" => {
                self.scan.ground_overlays.push(GroundOverlay {
                    name: None,
                    href: None,
                    lat_lon_box: false,
                    quad: false,
                    rotated: false,
                    tinted: false,
                    draw_order: false,
                });
                self.overlay = Some(self.scan.ground_overlays.len() - 1);
            }
            "ScreenOverlay" => self.scan.screen_overlays += 1,
            "Region" => self.scan.regions += 1,
            "Lod" => self.scan.lods += 1,
            "LatLonBox" => {
                if let Some(index) = self.overlay {
                    self.scan.ground_overlays[index].lat_lon_box = true;
                }
            }
            "gx:LatLonQuad" => {
                if let Some(index) = self.overlay {
                    self.scan.ground_overlays[index].quad = true;
                }
            }
            part if STYLE_PARTS.contains(&part) => {
                if let Some(index) = self.style {
                    self.scan.styles[index].parts.insert(part.to_string());
                }
            }
            geometry if GEOMETRIES.contains(&geometry) && parent == "Placemark" => {
                self.placemark_has_geometry = true;
                let index = self.container();
                *self.scan.containers[index]
                    .geometries
                    .entry(geometry.to_string())
                    .or_insert(0) += 1;
            }
            own_row if OWN_ROW_GEOMETRY.contains(&own_row) && parent == "Placemark" => {
                self.placemark_has_geometry = true;
                let index = self.container();
                *self.scan.containers[index]
                    .own_row_geometry
                    .entry(own_row.to_string())
                    .or_insert(0) += 1;
            }
            _ => {}
        }
        self.elements.push(name);
        Ok(())
    }

    fn end(&mut self) {
        let Some(name) = self.elements.last().cloned() else {
            return;
        };
        let parent = self.parent().to_string();
        let text = self.text.trim().to_string();
        self.text.clear();

        match name.as_str() {
            "Document" | "Folder" => {
                self.open_containers.pop();
            }
            "Placemark" => {
                self.in_placemark = false;
                let placemark_name = self.placemark_name.take();
                for track in &mut self.scan.tracks[self.placemark_tracks..] {
                    track.name = placemark_name.clone();
                }
                if !self.placemark_has_geometry {
                    let index = self.container();
                    self.scan.containers[index].without_geometry += 1;
                }
            }
            "gx:Track" => {
                // inside a gx:MultiTrack this ends a segment, not the record
                if self
                    .track
                    .is_some_and(|index| !self.scan.tracks[index].multi)
                {
                    self.track = None;
                }
            }
            "gx:MultiTrack" => self.track = None,
            "Style" => self.style = None,
            "StyleMap" => self.style_map = None,
            "Schema" => self.schema = None,
            "NetworkLink" => self.network_link = None,
            "GroundOverlay" => self.overlay = None,
            "name" => self.take_name(&parent, text),
            "description" => {
                if parent == "Document" {
                    self.scan.doc_description = true;
                }
            }
            "href" => self.take_href(text),
            "key" => {
                if let Some(index) = self.style_map {
                    self.scan.style_maps[index].keys.push(text);
                }
            }
            "displayName" => {
                if let Some(index) = self.schema
                    && let Some(field) = self.scan.schemas[index].fields.last_mut()
                {
                    field.display_name = true;
                }
            }
            "colorMode" => {
                if let Some(index) = self.style
                    && text == "random"
                {
                    self.scan.styles[index].random_color = true;
                }
            }
            "heading" => {
                if let Some(index) = self.style
                    && parent == "IconStyle"
                {
                    self.scan.styles[index].icon_heading = true;
                }
            }
            "altitudeMode" | "gx:altitudeMode" => {
                if self.in_placemark && text != "clampToGround" && !self.inside_own_row_geometry() {
                    self.note_altitude_hint(&name);
                }
            }
            "extrude" | "tessellate" => {
                if self.in_placemark && text.trim() != "0" && !self.inside_own_row_geometry() {
                    self.note_altitude_hint(&name);
                }
            }
            "refreshMode" | "viewRefreshMode" => {
                if let Some(index) = self.network_link {
                    self.scan.network_links[index]
                        .refresh
                        .push(format!("{name}={text}"));
                }
            }
            "rotation" => {
                if let Some(index) = self.overlay
                    && text.parse::<f64>().is_ok_and(|degrees| degrees != 0.0)
                {
                    self.scan.ground_overlays[index].rotated = true;
                }
            }
            "drawOrder" => {
                if let Some(index) = self.overlay {
                    self.scan.ground_overlays[index].draw_order = true;
                }
            }
            "color" => {
                if parent == "GroundOverlay"
                    && let Some(index) = self.overlay
                {
                    self.scan.ground_overlays[index].tinted = true;
                }
            }
            "when" if self.track.is_some() => {
                let index = self.track.expect("a track is open");
                self.scan.tracks[index].samples += 1;
                if !text.is_empty() && !text.contains('T') {
                    self.scan.tracks[index].partial_times = true;
                }
            }
            "gx:coord" => {
                if let Some(index) = self.track
                    && text
                        .split_whitespace()
                        .nth(2)
                        .is_some_and(|alt| alt.parse::<f64>().is_ok_and(|metres| metres != 0.0))
                {
                    self.scan.tracks[index].altitude = true;
                }
            }
            "when" | "begin" | "end" if !text.is_empty() && !text.contains('T') => {
                self.scan.partial_times = true;
            }
            _ => {}
        }
        self.elements.pop();
    }

    fn take_name(&mut self, parent: &str, text: String) {
        match parent {
            "Document" | "Folder" => {
                if let Some(index) = self.open_containers.last().copied()
                    && self.scan.containers[index].name.is_none()
                {
                    self.scan.containers[index].name = Some(text.clone());
                }
                if parent == "Document" && self.scan.doc_name.is_none() {
                    self.scan.doc_name = Some(text);
                }
            }
            "NetworkLink" => {
                if let Some(index) = self.network_link {
                    self.scan.network_links[index].name = Some(text);
                }
            }
            "GroundOverlay" => {
                if let Some(index) = self.overlay {
                    self.scan.ground_overlays[index].name = Some(text);
                }
            }
            "Placemark" => self.placemark_name = Some(text),
            _ => {}
        }
    }

    fn open_track(&mut self, element: &str) {
        if let Some(index) = self.track {
            self.scan.tracks[index].segments += 1;
            return;
        }
        let multi = element == "gx:MultiTrack";
        self.scan.tracks.push(Track {
            container: self.open_containers.last().copied(),
            name: None,
            multi,
            segments: usize::from(!multi),
            samples: 0,
            altitude: false,
            angles: false,
            array_data: false,
            partial_times: false,
        });
        self.track = Some(self.scan.tracks.len() - 1);
    }

    fn take_href(&mut self, text: String) {
        if let Some(index) = self.network_link {
            self.scan.network_links[index].href = Some(text);
            return;
        }
        if let Some(index) = self.overlay
            && self.ancestor("Icon")
        {
            self.scan.ground_overlays[index].href = Some(text);
            return;
        }
        if let Some(index) = self.style
            && self.ancestor("Icon")
        {
            self.scan.styles[index].icon_href = Some(text);
        }
    }

    fn note_altitude_hint(&mut self, name: &str) {
        let index = self.container();
        self.scan.containers[index]
            .altitude_hints
            .insert(name.to_string());
    }
}

fn attr(e: &BytesStart<'_>, key: &str) -> Result<Option<String>, KmlError> {
    for attribute in e.attributes() {
        let attribute = attribute?;
        if attribute.key.as_ref() == key.as_bytes() {
            // KML is XML 1.0; the 1.1-only normalisations do not apply
            return Ok(Some(
                attribute
                    .normalized_value(XmlVersion::Implicit1_0)?
                    .to_string(),
            ));
        }
    }
    Ok(None)
}
