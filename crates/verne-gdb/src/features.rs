//! The features and the attachment blobs, written beside the GeoPackage in the
//! form the loader posts.
//!
//! The loader builds without GDAL and has to stay that way, so it cannot open
//! the GeoPackage that holds the same features. What it gets instead is one
//! file of JSON lines per dataset, each line an insert operation of ptolemy's
//! commit route, and one file per attachment blob. Writing them here is where
//! the field types, the WKB and the identifiers are decided, by GDAL, once.
//!
//! Feature ids are minted here rather than by ptolemy. That is what lets an
//! attachment name the feature it belongs to: the load never reads anything
//! back, so the only ids it can key on are the ones the extraction chose.
//!
//! # The two outputs hold different coordinates
//!
//! ptolemy's commit reads every geometry it is sent as EPSG:4326 and stores no
//! other reference, so what goes into these files is transformed into 4326 by
//! GDAL first. The GeoPackage is not: a GeoPackage holds any reference, and it
//! is the file a reader keeps, so reprojecting it would be a loss taken for
//! nothing. The two differ on purpose and the log says so against every class
//! it is true of.
//!
//! Reading only: the geodatabase is the same read-only dataset the inventory
//! walked, and everything written goes into the extraction directory.

use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::path::Path;

use gdal::Dataset;
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::vector::{Feature, FieldValue, Layer, LayerAccess};
use verne_core::{
    ATTACHMENTS_DIR, FEATURES_DIR, GEOPACKAGE_FILE, MAX_FEATURE_BYTES, NewAttachment, NewFeature,
};

use crate::glue::Relationship;
use crate::scan::{Scan, Table, TableRole};
use crate::source::GdbError;

/// An empty geometry collection, hex WKB, little endian.
///
/// ptolemy's insert takes a geometry and has no way to say there is none: the
/// column accepts a null, and a null there is how a deleted version reads. A
/// row with no shape is written as this instead, which is a geometry that holds
/// nothing rather than an absent one.
const EMPTY_GEOMETRY: &str = "010700000000000000";

/// The blob column of an Esri `__ATTACH` table, and the columns beside it that
/// ptolemy has a field for.
const DATA_COLUMN: &str = "DATA";
const NAME_COLUMN: &str = "ATT_NAME";
const CONTENT_TYPE_COLUMN: &str = "CONTENT_TYPE";

/// The one spatial reference ptolemy stores. Its commit hands the WKB to
/// `ST_GeomFromWKB(..., 4326)` whatever the dataset's srid column says, so a
/// geometry in anything else has to be transformed before it is sent.
const PTOLEMY_SRID: u32 = 4326;

/// What writing one table's features came to.
pub struct FeatureFile {
    pub source_table: String,
    /// Named relative to the extraction directory. Absent when none of this
    /// table's features can go to ptolemy at all, in which case `losses` is
    /// the one reason why and no file was written.
    pub path: Option<String>,
    pub features: usize,
    /// What the write itself dropped, in the words the log will use.
    pub losses: Vec<String>,
}

/// What became of one `__ATTACH` table.
pub struct AttachmentFile {
    /// The `__ATTACH` table the rows came from.
    pub source_table: String,
    pub carried: usize,
    /// Rows that could not be attributed to a feature, with why. Never
    /// attached to something else: an attachment on the wrong feature is worse
    /// than one that did not arrive.
    pub orphans: Vec<String>,
}

/// Everything written beside the GeoPackage.
pub struct Written {
    pub files: Vec<FeatureFile>,
    pub attachments: Vec<NewAttachment>,
    pub attachment_files: Vec<AttachmentFile>,
}

/// Write one file of insert operations per table, then the attachment blobs.
///
/// `tables` is the source tables that became datasets, paired with the dataset
/// name each one is created under, in the order the plans hold them.
pub fn write(
    source: &Dataset,
    scan: &Scan,
    directory: &Path,
    tables: &[(&str, &str)],
    operator: &str,
) -> Result<Written, GdbError> {
    let media = media_relationships(scan, tables);
    let mut files = Vec::new();
    // only the tables an attachment relationship points at need their keys
    // remembered: on a table nothing is attached to, a map of every row's id is
    // a copy of the table for nothing
    let mut keys: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (table, _) in tables {
        let Some(scanned) = scan.table(table) else {
            continue;
        };
        let key_field = media
            .iter()
            .find(|held| held.feature_table == *table)
            .map(|held| held.feature_key.as_str());
        let (file, minted) = write_table(source, scanned, directory, key_field)?;
        if let Some(minted) = minted {
            keys.insert((*table).to_string(), minted);
        }
        files.push(file);
    }

    let names: BTreeMap<&str, &str> = tables.iter().copied().collect();
    let blobs = Blobs {
        source,
        scan,
        directory,
        operator,
    };
    let mut attachments = Vec::new();
    let mut attachment_files = Vec::new();
    for link in &media {
        let dataset = names[link.feature_table.as_str()];
        let empty = BTreeMap::new();
        let minted = keys.get(&link.feature_table).unwrap_or(&empty);
        attachment_files.push(blobs.write(link, dataset, minted, &mut attachments)?);
    }
    Ok(Written {
        files,
        attachments,
        attachment_files,
    })
}

/// An attachment relationship whose two sides verne can act on: the class it
/// hangs attachments off, the blob table, and the columns they relate through.
pub struct Media {
    pub relationship: String,
    pub feature_table: String,
    pub attach_table: String,
    /// The column on the feature side holding the key. `OBJECTID` is the
    /// feature id rather than a column, and is read as such.
    pub feature_key: String,
    /// The column on the blob side holding the feature's key.
    pub attach_key: String,
}

/// The media relationships this extraction can carry, which is those with a
/// blob table, a key on each side, and a feature side that became a dataset.
pub fn media_relationships(scan: &Scan, tables: &[(&str, &str)]) -> Vec<Media> {
    scan.relationships
        .iter()
        .filter(|held| held.related_table_type.as_deref() == Some("media"))
        .filter_map(|held| media(held, tables))
        .collect()
}

fn media(relationship: &Relationship, tables: &[(&str, &str)]) -> Option<Media> {
    let feature_table = relationship.left_table.clone()?;
    if !tables.iter().any(|(table, _)| *table == feature_table) {
        return None;
    }
    Some(Media {
        relationship: relationship.name.clone(),
        feature_table,
        attach_table: relationship.right_table.clone()?,
        feature_key: relationship.left_fields.first()?.clone(),
        attach_key: relationship.right_fields.first()?.clone(),
    })
}

// ─── Features ───────────────────────────────────────────────────────

/// How one table's geometry gets to the reference ptolemy stores.
enum Projection {
    /// Nothing to transform: the layer is already EPSG:4326, or holds no
    /// geometry at all.
    Straight,
    /// Every geometry through this, by GDAL. Named so the log can say what it
    /// came out of.
    Through(Box<CoordTransform>, String),
    /// These features cannot go to ptolemy, and this is why.
    Refused(String),
}

/// What has to happen to a layer's geometry, decided once per table.
///
/// A layer with no spatial reference is refused rather than passed through.
/// ptolemy would read the numbers as degrees, and coordinates whose meaning
/// verne cannot state must not be committed as though it could: on a projected
/// source that is metres or feet read as longitude, which is not a small error
/// but a meaningless one.
fn projection(layer: &Layer<'_>, table: &Table) -> Projection {
    if table.geometry.is_none() {
        return Projection::Straight;
    }
    let Some(mut from) = layer.spatial_ref() else {
        return Projection::Refused(format!(
            "{} names no spatial reference, so there is nothing to transform out of, and ptolemy would read the coordinates as EPSG:4326 degrees whatever they are; none were written and they are in the GeoPackage as they stand",
            table.name
        ));
    };
    let named = from.name().unwrap_or_else(|| "an unnamed reference".into());
    if table.srid == Some(PTOLEMY_SRID as i32) {
        return Projection::Straight;
    }
    let Ok(mut to) = SpatialRef::from_epsg(PTOLEMY_SRID) else {
        return Projection::Refused(format!(
            "GDAL could not build EPSG:{PTOLEMY_SRID} to transform {named} into, so nothing was written"
        ));
    };
    // both ends in x/y order, so what GDAL is handed and what it gives back are
    // easting/northing and longitude/latitude rather than the axis order the
    // authority declares
    from.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    to.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    match CoordTransform::new(&from, &to) {
        Ok(transform) => Projection::Through(Box::new(transform), named),
        Err(error) => Projection::Refused(format!(
            "GDAL knows no transformation from {named} to EPSG:{PTOLEMY_SRID} ({error}), and ptolemy stores geometry as {PTOLEMY_SRID} and nothing else, so none of these features were written; they are in the GeoPackage in their own reference"
        )),
    }
}

/// One table's rows as insert operations, and the keys of the features that
/// came out when the caller asked for them.
fn write_table(
    source: &Dataset,
    table: &Table,
    directory: &Path,
    key_field: Option<&str>,
) -> Result<(FeatureFile, Option<BTreeMap<String, String>>), GdbError> {
    let mut layer = source
        .layer_by_name(&table.name)
        .map_err(|error| GdbError::Features {
            table: table.name.clone(),
            message: error.to_string(),
        })?;
    // the GeoPackage was written from these same layers and left every cursor
    // at the end of its table, so without this the file comes out empty
    layer.reset_feature_reading();

    // decided before a file is opened: a refused table gets no feature file
    // rather than an empty one, so the sidecar names nothing for it
    let (transform, transformed_from) = match projection(&layer, table) {
        Projection::Straight => (None, None),
        Projection::Through(transform, named) => (Some(transform), Some(named)),
        Projection::Refused(reason) => {
            return Ok((
                FeatureFile {
                    source_table: table.name.clone(),
                    path: None,
                    features: 0,
                    losses: vec![reason],
                },
                None,
            ));
        }
    };

    let relative = format!("{FEATURES_DIR}/{}.ndjson", file_name(&table.name));
    let path = directory.join(&relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| GdbError::Write {
            path: parent.display().to_string(),
            source,
        })?;
    }
    let file = std::fs::File::create(&path).map_err(|source| GdbError::Write {
        path: path.display().to_string(),
        source,
    })?;
    let mut out = BufWriter::new(file);

    // the columns that can be read at all, with the ptolemy type each was
    // declared as on the schema, so a value cannot arrive as a type the schema
    // says it is not
    let readable: Vec<(usize, &str, &'static str)> = table
        .fields
        .iter()
        .enumerate()
        .filter(|(_, field)| field.kind != "Binary")
        .map(|(index, field)| {
            (
                index,
                field.name.as_str(),
                crate::verdict::schema_field_type(&field.kind).0,
            )
        })
        .collect();

    // the original rides beside the transformed copy only when its reference
    // has an EPSG code to store it under, since that is how ptolemy names one.
    // a codeless reference keeps its original in the GeoPackage alone, and the
    // losses say so
    let original_srid = if transform.is_some() { table.srid } else { None };

    let mut minted = key_field.map(|_| BTreeMap::new());
    let mut tally = Tally::default();
    for feature in layer.features() {
        tally.read += 1;
        let id = uuid::Uuid::now_v7().to_string();
        let (geometry, native) = match feature.geometry() {
            Some(shape) => match wkb_hex(shape, transform.as_deref()) {
                Ok(hex) => {
                    let native = original_srid
                        .and_then(|srid| wkb_hex(shape, None).ok().map(|original| (original, srid)));
                    (hex, native)
                }
                // a geometry GDAL will not transform or will not export is
                // still a row, and the row is carried with the shape left out
                // rather than dropped
                Err(_) => {
                    tally.unwritable += 1;
                    (EMPTY_GEOMETRY.to_string(), None)
                }
            },
            None => {
                tally.shapeless += 1;
                (EMPTY_GEOMETRY.to_string(), None)
            }
        };
        let mut properties = serde_json::Map::new();
        for (index, name, declared) in &readable {
            if let Some(value) = json_value(&feature, *index, declared) {
                properties.insert((*name).to_string(), value);
            }
        }
        let (native_hex, native_srid) = match native {
            Some((hex, srid)) => (Some(hex), Some(srid)),
            None => (None, None),
        };
        let line = serde_json::to_string(&NewFeature {
            feature_id: id.clone(),
            geometry_wkb_hex: geometry,
            properties,
            native_geometry_wkb_hex: native_hex,
            native_srid,
        })
        .map_err(|error| GdbError::Features {
            table: table.name.clone(),
            message: error.to_string(),
        })?;
        // a feature ptolemy would refuse is not written: it would fail the
        // batch it landed in and take the rest of the table with it
        if line.len() > MAX_FEATURE_BYTES {
            tally.oversized += 1;
            tally.largest = tally.largest.max(line.len());
            continue;
        }
        // only a feature that was written may be attached to, so the key goes
        // in after the size check and not before
        if let (Some(minted), Some(key)) = (minted.as_mut(), key_field)
            && let Some(key) = feature_key(&feature, key)
        {
            minted.insert(key, id);
        }
        writeln!(out, "{line}").map_err(|source| GdbError::Write {
            path: path.display().to_string(),
            source,
        })?;
        tally.written += 1;
    }
    out.flush().map_err(|source| GdbError::Write {
        path: path.display().to_string(),
        source,
    })?;

    Ok((
        FeatureFile {
            source_table: table.name.clone(),
            path: Some(relative),
            features: tally.written,
            losses: feature_losses(table, &tally, transformed_from.as_deref()),
        },
        minted,
    ))
}

/// What one table's rows came to.
#[derive(Default)]
struct Tally {
    read: usize,
    written: usize,
    shapeless: usize,
    unwritable: usize,
    /// Rows whose insert is bigger than ptolemy will take.
    oversized: usize,
    /// The biggest of those, in bytes, so the log says how far over it was.
    largest: usize,
}

/// What reading the rows found, which is the part no verdict can know: the
/// report already says what a binary column, an srid that is not 4326 and a
/// table with no geometry at all cost, because those are true of the table
/// before a single row is read.
fn feature_losses(table: &Table, tally: &Tally, transformed_from: Option<&str>) -> Vec<String> {
    let mut losses = Vec::new();
    // the one place the two outputs are said to differ, against the file that
    // differs from the other
    if let Some(from) = transformed_from {
        losses.push(match table.srid {
            Some(code) => format!(
                "every geometry here was transformed out of {from} into EPSG:{PTOLEMY_SRID} by GDAL, because that is the working reference ptolemy serves. the untransformed original rides on each insert as EPSG:{code} and ptolemy keeps it beside the working copy, and {GEOPACKAGE_FILE} keeps the whole class in {from} as well"
            ),
            None => format!(
                "every geometry here was transformed out of {from} into EPSG:{PTOLEMY_SRID} by GDAL, because that is the working reference ptolemy serves. ptolemy names a reference by EPSG code and none names {from}, so the original could not be sent beside the working copy and {GEOPACKAGE_FILE} is the only file keeping the class in {from}"
            ),
        });
    }
    // a shapeless row in a table that has no geometry column is what the
    // report already calls out; this is about the ones that were meant to have
    // a shape
    if tally.shapeless > 0 && table.geometry.is_some() {
        losses.push(format!(
            "no geometry on {}, and ptolemy's insert has no way to say so that is not also how a deletion reads, so each was written as an empty geometry collection",
            of_rows(tally.shapeless, tally.read)
        ));
    }
    if tally.unwritable > 0 {
        losses.push(format!(
            "GDAL would not export the geometry of {}, which were written with an empty geometry collection in its place",
            of_rows(tally.unwritable, tally.read)
        ));
    }
    if tally.oversized > 0 {
        losses.push(format!(
            "an insert bigger than the {MAX_FEATURE_BYTES} bytes ptolemy takes in a request on {}, {}, so they are in the GeoPackage and not in the feature file and no load will create them",
            of_rows(tally.oversized, tally.read),
            if tally.oversized == 1 {
                format!("which is {} bytes", tally.largest)
            } else {
                format!("the largest {} bytes", tally.largest)
            }
        ));
    }
    losses
}

/// "3 of the 40 rows", as a noun phrase with no verb after it: a loss that
/// reads "1 of 1 rows carry" is a loss nobody trusts the rest of.
fn of_rows(some: usize, all: usize) -> String {
    match (some, all) {
        (_, 1) => "the one row".to_string(),
        (some, all) if some == all => format!("all {all} rows"),
        (some, all) => format!("{some} of the {all} rows"),
    }
}

/// One field as JSON, in the type the schema declared the column as.
///
/// A value that will not fit the declared type is left out rather than sent as
/// something else: ptolemy validates a commit against the schema, and a column
/// arriving as the wrong type would fail the whole batch.
fn json_value(feature: &Feature<'_>, index: usize, declared: &str) -> Option<serde_json::Value> {
    let value = feature.field(index).ok()??;
    let json = match value {
        FieldValue::StringValue(text) => serde_json::Value::String(text),
        FieldValue::IntegerValue(number) => serde_json::Value::from(number),
        FieldValue::Integer64Value(number) => serde_json::Value::from(number),
        FieldValue::RealValue(number) => serde_json::Number::from_f64(number)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        FieldValue::StringListValue(values) => serde_json::Value::from(values),
        FieldValue::IntegerListValue(values) => serde_json::Value::from(values),
        FieldValue::Integer64ListValue(values) => serde_json::Value::from(values),
        FieldValue::RealListValue(values) => serde_json::Value::from(values),
        FieldValue::DateValue(date) => serde_json::Value::String(date.to_string()),
        FieldValue::DateTimeValue(instant) => serde_json::Value::String(instant.to_string()),
    };
    let fits = match declared {
        "string" => json.is_string(),
        "integer" => json.is_i64() || json.is_u64(),
        "float" => json.is_number(),
        "array" => json.is_array(),
        _ => false,
    };
    fits.then_some(json)
}

/// The value a relationship class keys a feature on. `OBJECTID` is the
/// geodatabase's feature id and not a column, so it is read off the feature
/// itself unless the table really does have a column of that name.
fn feature_key(feature: &Feature<'_>, field: &str) -> Option<String> {
    match feature.field_index(field) {
        Ok(index) => feature.field_as_string(index).ok()?,
        Err(_) => feature.fid().map(|fid| fid.to_string()),
    }
}

/// A geometry as hex WKB, in the ISO encoding, which is what says Z and M in
/// the type code rather than in a flag PostGIS would have to guess at.
///
/// With a transform the geometry is cloned and the clone transformed, because
/// the one the feature holds is the source's and this file writes nothing back
/// to the source. georust/gdal wraps neither call, so both are the C API.
fn wkb_hex(
    geometry: &gdal::vector::Geometry,
    transform: Option<&CoordTransform>,
) -> Result<String, ()> {
    let Some(transform) = transform else {
        return unsafe { export_wkb(geometry.c_geometry()) };
    };
    unsafe {
        let clone = gdal_sys::OGR_G_Clone(geometry.c_geometry());
        if clone.is_null() {
            return Err(());
        }
        let moved = gdal_sys::OGR_G_Transform(clone, transform.to_c_hct());
        let out = if moved == gdal_sys::OGRErr::OGRERR_NONE {
            export_wkb(clone)
        } else {
            Err(())
        };
        gdal_sys::OGR_G_DestroyGeometry(clone);
        out
    }
}

/// # Safety
/// `handle` must be a live geometry.
unsafe fn export_wkb(handle: gdal_sys::OGRGeometryH) -> Result<String, ()> {
    unsafe {
        let size = gdal_sys::OGR_G_WkbSizeEx(handle);
        if size == 0 {
            return Err(());
        }
        let mut bytes = vec![0u8; size];
        let result = gdal_sys::OGR_G_ExportToIsoWkb(
            handle,
            gdal_sys::OGRwkbByteOrder::wkbNDR,
            bytes.as_mut_ptr(),
        );
        if result != gdal_sys::OGRErr::OGRERR_NONE {
            return Err(());
        }
        Ok(hex(&bytes))
    }
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

// ─── Attachments ────────────────────────────────────────────────────

/// What one pass over the blob tables needs, so writing one is a method with
/// the link and its keys rather than a function with eight arguments.
struct Blobs<'a> {
    source: &'a Dataset,
    scan: &'a Scan,
    directory: &'a Path,
    operator: &'a str,
}

impl Blobs<'_> {
    /// One `__ATTACH` table's blobs, written as files and named in the sidecar.
    fn write(
        &self,
        link: &Media,
        dataset: &str,
        keys: &BTreeMap<String, String>,
        out: &mut Vec<NewAttachment>,
    ) -> Result<AttachmentFile, GdbError> {
        let (source, scan, directory, operator) =
            (self.source, self.scan, self.directory, self.operator);
        let mut file = AttachmentFile {
            source_table: link.attach_table.clone(),
            carried: 0,
            orphans: Vec::new(),
        };
        let Some(table) = scan.table(&link.attach_table) else {
            file.orphans.push(format!(
                "{} names {} as its blob table and no table of that name was read",
                link.relationship, link.attach_table
            ));
            return Ok(file);
        };
        let mut layer =
            source
                .layer_by_name(&link.attach_table)
                .map_err(|error| GdbError::Features {
                    table: link.attach_table.clone(),
                    message: error.to_string(),
                })?;
        layer.reset_feature_reading();
        let relative_dir = format!("{ATTACHMENTS_DIR}/{}", file_name(&link.attach_table));
        let blob_dir = directory.join(&relative_dir);
        std::fs::create_dir_all(&blob_dir).map_err(|source| GdbError::Write {
            path: blob_dir.display().to_string(),
            source,
        })?;

        let mut unresolved = 0usize;
        let mut empty = 0usize;
        for (row, feature) in layer.features().enumerate() {
            let key = feature
                .field_index(&link.attach_key)
                .ok()
                .and_then(|index| feature.field_as_string(index).ok().flatten());
            let Some(feature_id) = key.as_deref().and_then(|key| keys.get(key)) else {
                unresolved += 1;
                continue;
            };
            let Some(bytes) = binary_field(&feature, DATA_COLUMN) else {
                empty += 1;
                continue;
            };
            let name = string_field(&feature, NAME_COLUMN)
                .unwrap_or_else(|| format!("{}-{row}", link.attach_table));
            let blob = format!("{relative_dir}/{row}-{}", file_name(&name));
            std::fs::write(directory.join(&blob), &bytes).map_err(|source| GdbError::Write {
                path: blob.clone(),
                source,
            })?;
            out.push(NewAttachment {
                dataset: dataset.to_string(),
                feature_id: feature_id.clone(),
                name,
                content_type: string_field(&feature, CONTENT_TYPE_COLUMN),
                file: blob,
                metadata: row_metadata(&feature, table, &link.attach_table),
                created_by: operator.to_string(),
            });
            file.carried += 1;
        }
        if unresolved > 0 {
            file.orphans.push(format!(
                "no feature of {} carries the {} named by {unresolved} row{} of {}, so there is nothing to attach them to and guessing from the name would put them on the wrong feature",
                link.feature_table,
                link.attach_key,
                plural(unresolved),
                link.attach_table
            ));
        }
        if empty > 0 {
            file.orphans.push(format!(
                "no bytes in {DATA_COLUMN} on {empty} row{} of {}, and an attachment with no data is nothing to upload",
                plural(empty),
                link.attach_table
            ));
        }
        Ok(file)
    }
}

/// Every column of the row but the blob, as text.
///
/// ptolemy's attachment holds the bytes, a name, a content type and a size and
/// nothing else, so the row's own identifiers go here or nowhere.
fn row_metadata(feature: &Feature<'_>, table: &Table, source_table: &str) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "source_table".to_string(),
        serde_json::Value::String(source_table.to_string()),
    );
    for (index, field) in table.fields.iter().enumerate() {
        if field.name == DATA_COLUMN || field.kind == "Binary" {
            continue;
        }
        if let Ok(Some(text)) = feature.field_as_string(index) {
            map.insert(field.name.clone(), serde_json::Value::String(text));
        }
    }
    serde_json::Value::Object(map)
}

fn string_field(feature: &Feature<'_>, name: &str) -> Option<String> {
    let index = feature.field_index(name).ok()?;
    let text = feature.field_as_string(index).ok()??;
    if text.is_empty() { None } else { Some(text) }
}

/// A binary column's bytes. georust/gdal has no wrapper for one, and it is the
/// only thing in an attachment table worth reading.
fn binary_field(feature: &Feature<'_>, name: &str) -> Option<Vec<u8>> {
    let index = feature.field_index(name).ok()?;
    unsafe {
        let handle = feature.c_feature();
        if gdal_sys::OGR_F_IsFieldSetAndNotNull(handle, index as i32) == 0 {
            return None;
        }
        let mut length: i32 = 0;
        let bytes = gdal_sys::OGR_F_GetFieldAsBinary(handle, index as i32, &mut length);
        if bytes.is_null() || length <= 0 {
            return None;
        }
        Some(std::slice::from_raw_parts(bytes as *const u8, length as usize).to_vec())
    }
}

// ─── Names ──────────────────────────────────────────────────────────

/// A file name that stands for a table or an attachment without carrying
/// anything a path could act on. Anything outside the safe set becomes an
/// underscore, so two names can collide, which is why every attachment file is
/// also prefixed with its row number.
fn file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        "unnamed".to_string()
    } else {
        cleaned
    }
}

/// `__ATTACH` tables no media relationship points at. The report already has a
/// row for each; this is the extraction saying it will not guess.
pub fn orphan_attachment_tables<'a>(scan: &'a Scan, media: &[Media]) -> Vec<&'a str> {
    scan.tables
        .iter()
        .filter(|table| table.role == TableRole::Attachment)
        .map(|table| table.name.as_str())
        .filter(|name| !media.iter().any(|link| link.attach_table == *name))
        .collect()
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::{EMPTY_GEOMETRY, file_name, hex};

    #[test]
    fn a_table_name_becomes_a_file_name_with_nothing_a_path_can_act_on() {
        assert_eq!(file_name("wells"), "wells");
        assert_eq!(file_name("../etc/passwd"), "_etc_passwd");
        assert_eq!(file_name(".."), "unnamed");
        assert_eq!(file_name("N_1_Props"), "N_1_Props");
    }

    /// The convention for a row with no shape, spelled out: an empty geometry
    /// collection, little endian, no members.
    #[test]
    fn the_empty_geometry_is_a_geometry_collection_with_nothing_in_it() {
        assert_eq!(hex(&[0x01, 0x07, 0, 0, 0, 0, 0, 0, 0]), EMPTY_GEOMETRY);
    }
}
