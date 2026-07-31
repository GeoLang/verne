//! Esri JSON geometry to ISO WKB, the encoding ptolemy is committed in.
//!
//! No GDAL and no PROJ in this crate, so nothing here transforms a coordinate:
//! the service is asked to answer in EPSG:4326 and these bytes carry what it
//! sent. ISO type codes say Z and M in the code itself, which is how the gdb
//! adapter writes them and how PostGIS keeps them.
//!
//! A polygon's rings arrive as one flat list. Esri JSON winds an outer ring
//! clockwise and a hole counterclockwise, and says nothing about which outer
//! ring a hole belongs to, so each hole is put in the outer ring that contains
//! its first vertex. A hole no outer ring contains is kept as an outer ring
//! rather than dropped: wrong winding is more often sloppy data than a hole.

/// One vertex. Z and M ride along when the feature set declares them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub x: f64,
    pub y: f64,
    pub z: Option<f64>,
    pub m: Option<f64>,
}

impl Position {
    pub fn xy(x: f64, y: f64) -> Self {
        Position {
            x,
            y,
            z: None,
            m: None,
        }
    }
}

/// A geometry as the query response holds it, already read out of the JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum EsriGeometry {
    Point(Position),
    Multipoint(Vec<Position>),
    /// `paths`: always written as a MultiLineString, matching the dataset's
    /// declared type, so a one-path and a three-path feature agree.
    Polyline(Vec<Vec<Position>>),
    /// `rings`, outer and holes mixed. Always written as a MultiPolygon.
    Polygon(Vec<Vec<Position>>),
}

/// An empty geometry collection, hex WKB, little endian: the same bytes the
/// gdb adapter writes for a row with no shape.
pub const EMPTY_GEOMETRY: &str = "010700000000000000";

const POINT: u32 = 1;
const LINESTRING: u32 = 2;
const POLYGON: u32 = 3;
const MULTIPOINT: u32 = 4;
const MULTILINESTRING: u32 = 5;
const MULTIPOLYGON: u32 = 6;

impl EsriGeometry {
    /// Hex WKB, little endian, ISO type codes. An empty multipoint, polyline
    /// or polygon comes out as the empty geometry collection: ptolemy has no
    /// way to take "no geometry" that is not also how a deletion reads.
    pub fn wkb_hex(&self) -> String {
        let mut out = Vec::new();
        match self {
            EsriGeometry::Point(position) => point(&mut out, *position),
            EsriGeometry::Multipoint(points) if points.is_empty() => return EMPTY_GEOMETRY.into(),
            EsriGeometry::Multipoint(points) => {
                header(&mut out, MULTIPOINT, dimension(points.first()));
                count(&mut out, points.len());
                for held in points {
                    point(&mut out, *held);
                }
            }
            EsriGeometry::Polyline(paths) if paths.is_empty() => return EMPTY_GEOMETRY.into(),
            EsriGeometry::Polyline(paths) => {
                let sample = paths.first().and_then(|path| path.first());
                header(&mut out, MULTILINESTRING, dimension(sample));
                count(&mut out, paths.len());
                for path in paths {
                    header(&mut out, LINESTRING, dimension(sample));
                    ring(&mut out, path);
                }
            }
            EsriGeometry::Polygon(rings) if rings.is_empty() => return EMPTY_GEOMETRY.into(),
            EsriGeometry::Polygon(rings) => {
                let sample = rings.first().and_then(|held| held.first());
                let polygons = assemble(rings);
                header(&mut out, MULTIPOLYGON, dimension(sample));
                count(&mut out, polygons.len());
                for polygon in polygons {
                    header(&mut out, POLYGON, dimension(sample));
                    count(&mut out, polygon.len());
                    for held in polygon {
                        ring(&mut out, held);
                    }
                }
            }
        }
        hex(&out)
    }
}

/// The ISO offset for Z and M, decided by what the first vertex carries: a
/// feature set declares hasZ and hasM once, so every vertex agrees.
fn dimension(sample: Option<&Position>) -> u32 {
    match sample {
        Some(Position {
            z: Some(_),
            m: Some(_),
            ..
        }) => 3000,
        Some(Position { z: Some(_), .. }) => 1000,
        Some(Position { m: Some(_), .. }) => 2000,
        _ => 0,
    }
}

fn point(out: &mut Vec<u8>, position: Position) {
    header(out, POINT, dimension(Some(&position)));
    vertex(out, position);
}

fn header(out: &mut Vec<u8>, base: u32, offset: u32) {
    out.push(1); // little endian
    out.extend_from_slice(&(base + offset).to_le_bytes());
}

fn count(out: &mut Vec<u8>, length: usize) {
    out.extend_from_slice(&(length as u32).to_le_bytes());
}

/// A linestring or ring body: the count, then the vertices. A ring that does
/// not end where it starts is closed here, because WKB readers expect it.
fn ring(out: &mut Vec<u8>, path: &[Position]) {
    let unclosed = path.len() > 2 && path.first() != path.last();
    count(out, path.len() + usize::from(unclosed));
    for held in path {
        vertex(out, *held);
    }
    if unclosed && let Some(first) = path.first() {
        vertex(out, *first);
    }
}

fn vertex(out: &mut Vec<u8>, position: Position) {
    out.extend_from_slice(&position.x.to_le_bytes());
    out.extend_from_slice(&position.y.to_le_bytes());
    if let Some(z) = position.z {
        out.extend_from_slice(&z.to_le_bytes());
    }
    if let Some(m) = position.m {
        out.extend_from_slice(&m.to_le_bytes());
    }
}

/// The flat ring list as polygons: each outer ring with the holes inside it.
fn assemble(rings: &[Vec<Position>]) -> Vec<Vec<&Vec<Position>>> {
    let mut outers: Vec<Vec<&Vec<Position>>> = Vec::new();
    let mut holes: Vec<&Vec<Position>> = Vec::new();
    for held in rings {
        // clockwise in x/y is a negative shoelace sum, and is Esri's outer
        if signed_area(held) <= 0.0 {
            outers.push(vec![held]);
        } else {
            holes.push(held);
        }
    }
    // no outer at all: every ring wound like a hole, so take them as they are
    if outers.is_empty() {
        return holes.into_iter().map(|held| vec![held]).collect();
    }
    for hole in holes {
        let Some(first) = hole.first() else { continue };
        match outers
            .iter_mut()
            .find(|polygon| contains(polygon[0], first))
        {
            Some(polygon) => polygon.push(hole),
            // a hole outside every outer ring is kept as its own polygon
            // rather than dropped
            None => outers.push(vec![hole]),
        }
    }
    outers
}

fn signed_area(ring: &[Position]) -> f64 {
    let mut sum = 0.0;
    for pair in ring.windows(2) {
        sum += (pair[1].x - pair[0].x) * (pair[1].y + pair[0].y);
    }
    if let (Some(last), Some(first)) = (ring.last(), ring.first())
        && last != first
    {
        sum += (first.x - last.x) * (first.y + last.y);
    }
    // the sum above is negative counterclockwise, so flip it to the usual
    // convention: positive counterclockwise
    -sum
}

/// Ray cast on the x axis: odd crossings means inside.
fn contains(ring: &[Position], point: &Position) -> bool {
    let mut inside = false;
    let mut previous = match ring.last() {
        Some(last) => last,
        None => return false,
    };
    for held in ring {
        if (held.y > point.y) != (previous.y > point.y) {
            let cross = (previous.x - held.x) * (point.y - held.y) / (previous.y - held.y) + held.x;
            if point.x < cross {
                inside = !inside;
            }
        }
        previous = held;
    }
    inside
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn positions(coordinates: &[(f64, f64)]) -> Vec<Position> {
        coordinates
            .iter()
            .map(|(x, y)| Position::xy(*x, *y))
            .collect()
    }

    #[test]
    fn a_point_is_iso_wkb() {
        let hex = EsriGeometry::Point(Position::xy(-69.0, 45.0)).wkb_hex();
        // 01 (little endian) 01000000 (point) then two doubles
        assert!(hex.starts_with("0101000000"), "{hex}");
        assert_eq!(hex.len(), 2 * (1 + 4 + 16));
    }

    #[test]
    fn z_and_m_land_in_the_type_code() {
        let z = EsriGeometry::Point(Position {
            x: 0.0,
            y: 0.0,
            z: Some(1.0),
            m: None,
        })
        .wkb_hex();
        assert!(z.starts_with("01e9030000"), "{z}"); // 1001

        let zm = EsriGeometry::Point(Position {
            x: 0.0,
            y: 0.0,
            z: Some(1.0),
            m: Some(2.0),
        })
        .wkb_hex();
        assert!(zm.starts_with("01b90b0000"), "{zm}"); // 3001
    }

    #[test]
    fn a_polyline_is_a_multilinestring() {
        let hex = EsriGeometry::Polyline(vec![
            positions(&[(0.0, 0.0), (1.0, 1.0)]),
            positions(&[(2.0, 2.0), (3.0, 3.0)]),
        ])
        .wkb_hex();
        // 05000000 multilinestring, 02000000 two members
        assert!(hex.starts_with("010500000002000000"), "{hex}");
    }

    #[test]
    fn an_empty_geometry_is_the_empty_collection() {
        assert_eq!(EsriGeometry::Polygon(Vec::new()).wkb_hex(), EMPTY_GEOMETRY);
        assert_eq!(
            EsriGeometry::Multipoint(Vec::new()).wkb_hex(),
            EMPTY_GEOMETRY
        );
    }

    /// Esri winds an outer ring clockwise. A square drawn clockwise plus a
    /// counterclockwise hole inside it must come out as one polygon with two
    /// rings, not two polygons.
    #[test]
    fn a_hole_lands_in_the_outer_ring_that_contains_it() {
        let outer = positions(&[
            (0.0, 0.0),
            (0.0, 10.0),
            (10.0, 10.0),
            (10.0, 0.0),
            (0.0, 0.0),
        ]);
        let hole = positions(&[(2.0, 2.0), (8.0, 2.0), (8.0, 8.0), (2.0, 8.0), (2.0, 2.0)]);
        let hex = EsriGeometry::Polygon(vec![outer, hole]).wkb_hex();
        // 06000000 multipolygon, 01000000 one polygon, then 03000000 polygon
        // header and 02000000 two rings
        assert!(hex.starts_with("0106000000010000000103000000"), "{hex}");
        assert!(hex[28..].starts_with("02000000"), "{hex}");
    }

    /// Two separate clockwise squares are two polygons of one multipolygon.
    #[test]
    fn two_outer_rings_are_two_polygons() {
        let first = positions(&[(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)]);
        let second = positions(&[(5.0, 5.0), (5.0, 6.0), (6.0, 6.0), (6.0, 5.0), (5.0, 5.0)]);
        let hex = EsriGeometry::Polygon(vec![first, second]).wkb_hex();
        assert!(hex.starts_with("010600000002000000"), "{hex}");
    }

    /// An unclosed ring is closed on the way out, because WKB readers expect
    /// the first vertex repeated at the end.
    #[test]
    fn an_unclosed_ring_is_closed() {
        let open = positions(&[(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)]);
        let hex = EsriGeometry::Polygon(vec![open]).wkb_hex();
        // ring vertex count is 5, at bytes: 1+4+4 (multi) + 1+4+4 (polygon) + 4
        let ring_count = &hex[2 * (9 + 9 + 4) - 8..][..8];
        assert_eq!(ring_count, "05000000");
    }
}
