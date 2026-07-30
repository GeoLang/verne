//! Features and attributes into a GeoPackage.
//!
//! The conversion is GDAL's own `GDALVectorTranslate`, the code behind
//! `ogr2ogr`, so the field types, the geometries and the spatial reference are
//! converted by GDAL and not by verne. This is the one module that writes: the
//! source dataset is still only read from, and the destination is a file that
//! did not exist before.
//!
//! The tables to write are named explicitly. Passing none would take the
//! system and `__ATTACH` tables with the rest, and the report has already said
//! those are not data.

use std::ffi::{CStr, CString, c_char, c_int};
use std::path::Path;

use gdal::Dataset;
use gdal::vector::LayerAccess;

/// Layer names GDAL used, in the order the tables were given. A name it had to
/// change to make legal in a GeoPackage would come back different from the one
/// asked for, which is what [`Layer::renamed`] reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layer {
    pub source_table: String,
    pub name: String,
    pub features: u64,
    /// Fields the source had and the layer does not.
    pub dropped_fields: Vec<String>,
}

impl Layer {
    pub fn renamed(&self) -> bool {
        self.name != self.source_table
    }
}

/// Write `tables` into a GeoPackage at `path`, and read back what landed.
///
/// `-preserve_fid` is not optional here: a geodatabase keys its relationship
/// classes on OBJECTID, which OpenFileGDB gives as the feature id and not as a
/// field, so without it the keys a relationship class names would point at
/// numbers GDAL invented.
pub fn write(source: &Dataset, path: &Path, tables: &[&str]) -> Result<Vec<Layer>, String> {
    if tables.is_empty() {
        return Ok(Vec::new());
    }
    // no -skipfailures: a partial GeoPackage that reports itself whole is
    // exactly the failure verne exists to avoid
    let mut argv = vec![arg("-f")?, arg("GPKG")?, arg("-preserve_fid")?];
    for table in tables {
        argv.push(arg(table)?);
    }
    let mut raw: Vec<*mut c_char> = argv.iter().map(|a| a.as_ptr() as *mut c_char).collect();
    raw.push(std::ptr::null_mut());
    let destination = arg(&path.to_string_lossy())?;

    unsafe {
        let options =
            gdal_sys::GDALVectorTranslateOptionsNew(raw.as_mut_ptr(), std::ptr::null_mut());
        if options.is_null() {
            return Err(last_error());
        }
        let mut handle = source.c_dataset();
        let mut usage_error: c_int = 0;
        let written = gdal_sys::GDALVectorTranslate(
            destination.as_ptr(),
            std::ptr::null_mut(),
            1,
            &mut handle,
            options,
            &mut usage_error,
        );
        gdal_sys::GDALVectorTranslateOptionsFree(options);
        if written.is_null() {
            return Err(last_error());
        }
        // the file is not complete until it is closed, and it is read back below
        gdal_sys::GDALClose(written);
    }

    read_back(source, path, tables)
}

/// What the GeoPackage actually holds, against what was asked for. A table
/// with no layer is an error rather than a note: a conversion that lost a whole
/// table has not happened.
fn read_back(source: &Dataset, path: &Path, tables: &[&str]) -> Result<Vec<Layer>, String> {
    let written =
        Dataset::open(path).map_err(|e| format!("cannot reopen {}: {e}", path.display()))?;
    let names: Vec<String> = written.layers().map(|layer| layer.name()).collect();
    let mut layers = Vec::new();
    for (index, table) in tables.iter().enumerate() {
        // GDAL writes the layers in the order it was given them, so the one at
        // this index is this table's however it ended up named
        let name = names
            .get(index)
            .ok_or_else(|| format!("GDAL wrote no layer for {table} in {}", path.display()))?;
        let layer = written
            .layer_by_name(name)
            .map_err(|e| format!("cannot read {name} back out of {}: {e}", path.display()))?;
        layers.push(Layer {
            source_table: (*table).to_string(),
            name: name.clone(),
            features: layer.feature_count(),
            dropped_fields: dropped_fields(source, table, &layer),
        });
    }
    Ok(layers)
}

/// Fields the source table had that the layer does not. GDAL renames a field a
/// GeoPackage cannot hold rather than dropping it, so this is normally empty
/// and a name in it is worth reporting.
fn dropped_fields(source: &Dataset, table: &str, written: &gdal::vector::Layer<'_>) -> Vec<String> {
    let Ok(original) = source.layer_by_name(table) else {
        return Vec::new();
    };
    let kept: Vec<String> = written.defn().fields().map(|field| field.name()).collect();
    original
        .defn()
        .fields()
        .map(|field| field.name())
        .filter(|name| !kept.contains(name))
        .collect()
}

fn arg(text: &str) -> Result<CString, String> {
    CString::new(text).map_err(|_| format!("{text} holds a NUL byte and cannot go to GDAL"))
}

fn last_error() -> String {
    let message = unsafe { CStr::from_ptr(gdal_sys::CPLGetLastErrorMsg()) }
        .to_string_lossy()
        .into_owned();
    if message.is_empty() {
        "GDAL refused the conversion and gave no reason".to_string()
    } else {
        message
    }
}
