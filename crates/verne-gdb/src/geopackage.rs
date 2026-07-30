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
//!
//! # One GeoPackage at a time
//!
//! GDAL 3.8's GeoPackage driver calls `spatialite_cleanup_ex` when a dataset
//! closes, which tears down libxml2's process-global table of character
//! encoding handlers. Two threads closing a GeoPackage at the same moment free
//! that table twice and glibc aborts the process with "double free or
//! corruption (fasttop)". This is GDAL's to fix and not verne's: 3.11 does not
//! reach that path at all, so the same code is clean there and the older
//! version, which is what CI has, is the one that breaks.
//!
//! [`serialised`] is therefore the only way to touch a GeoPackage in this
//! crate, and callers reading one back in the same process have to go through
//! it too. Serialising costs nothing an extraction notices: a single extraction
//! writes its GeoPackage in one pass anyway.

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_int};
use std::path::Path;
use std::sync::Mutex;

use gdal::Dataset;
use gdal::vector::LayerAccess;

static GEOPACKAGE: Mutex<()> = Mutex::new(());

thread_local! {
    /// Whether this thread is inside [`serialised`].
    ///
    /// Only the debug assertion below reads it, and that assertion is the whole
    /// guard: GDAL 3.11 tolerates an unguarded close, so a new GeoPackage call
    /// added outside `serialised` would pass every test on a modern GDAL and
    /// abort on CI. This makes it fail here instead, on any version.
    static HOLDING: Cell<bool> = const { Cell::new(false) };
}

/// Run `f` with GDAL's GeoPackage machinery to ourselves.
///
/// Everything that opens, writes or closes a GeoPackage has to be inside one of
/// these, a plain read of a file verne wrote included. See the module comment
/// for what happens otherwise.
///
/// Not reentrant: the lock is a plain mutex, so a `serialised` inside a
/// `serialised` on one thread deadlocks.
///
/// A panic inside `f` poisons the lock and nothing else. What it guards is the
/// absence of a second caller rather than a value a panic could leave half
/// written, so the poison is stepped over instead of propagated.
pub fn serialised<T>(f: impl FnOnce() -> T) -> T {
    let _guard = GEOPACKAGE.lock().unwrap_or_else(|held| held.into_inner());
    HOLDING.with(|holding| holding.set(true));
    let out = f();
    HOLDING.with(|holding| holding.set(false));
    out
}

fn assert_serialised(what: &str) {
    debug_assert!(
        HOLDING.with(Cell::get),
        "{what} touches a GeoPackage outside geopackage::serialised, which is not safe on GDAL 3.8"
    );
}

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
    // the write and the read back both close a GeoPackage, so both are inside
    // the one guard and the file is closed again before it is released
    serialised(|| translate(source, path, tables).and_then(|()| read_back(source, path, tables)))
}

fn translate(source: &Dataset, path: &Path, tables: &[&str]) -> Result<(), String> {
    assert_serialised("translate");
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
    Ok(())
}

/// What the GeoPackage actually holds, against what was asked for. A table
/// with no layer is an error rather than a note: a conversion that lost a whole
/// table has not happened.
fn read_back(source: &Dataset, path: &Path, tables: &[&str]) -> Result<Vec<Layer>, String> {
    assert_serialised("read_back");
    let written =
        Dataset::open(path).map_err(|e| format!("cannot reopen {}: {e}", path.display()))?;
    let names: Vec<String> = written.layers().map(|layer| layer.name()).collect();
    let paired = pair(tables, &names).map_err(|e| format!("{e} in {}", path.display()))?;

    let mut layers = Vec::new();
    for (table, name) in paired {
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

/// Which written layer is which source table.
///
/// By name, never by position. A GeoPackage lists its layers in its own order
/// rather than the order it was handed them — on real data it puts the spatial
/// ones first — so pairing by index hands most tables another table's layer,
/// and every rename, dropped field and feature count then reads against the
/// wrong one. The whole point of this file is a truthful account of what came
/// out, so a wrong pairing is worse than no pairing.
///
/// A name GDAL could not use is the only reason a table is not itself, and
/// attributing one takes a single candidate: guessing among several is what the
/// positional version did.
fn pair<'a>(tables: &[&'a str], names: &'a [String]) -> Result<Vec<(&'a str, &'a String)>, String> {
    let unclaimed: Vec<&String> = names
        .iter()
        .filter(|name| !tables.contains(&name.as_str()))
        .collect();

    tables
        .iter()
        .map(|table| {
            let name = match names.iter().find(|name| name.as_str() == *table) {
                Some(name) => name,
                None => match unclaimed.as_slice() {
                    [only] => *only,
                    [] => return Err(format!("GDAL wrote no layer for {table}")),
                    several => {
                        let list: Vec<&str> = several.iter().map(|n| n.as_str()).collect();
                        return Err(format!(
                            "no layer is named {table}, and {} of them answer to no table ({}), \
                             so which one is {table} cannot be told",
                            several.len(),
                            list.join(", "),
                        ));
                    }
                },
            };
            Ok((*table, name))
        })
        .collect()
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

#[cfg(test)]
mod tests {
    use super::pair;

    fn names(of: &[&str]) -> Vec<String> {
        of.iter().map(|n| n.to_string()).collect()
    }

    /// The bug this replaced. A real geodatabase comes back with its spatial
    /// layers first, so the written order is not the order the tables were
    /// handed over, and pairing by index gave every table another one's layer.
    #[test]
    fn a_reordered_geopackage_still_pairs_every_table_with_itself() {
        let tables = [
            "ExternalCrosswalk",
            "NHDStatus",
            "NHDWaterbody",
            "N_1_Props",
        ];
        let written = names(&[
            "NHDWaterbody",
            "N_1_Props",
            "ExternalCrosswalk",
            "NHDStatus",
        ]);

        let paired = pair(&tables, &written).expect("the same names in another order pair up");

        for (table, name) in paired {
            assert_eq!(table, name.as_str(), "{table} was paired with {name}");
        }
    }

    /// The case the positional version existed for, which still has to work.
    #[test]
    fn the_one_layer_no_table_answers_to_is_the_renamed_one() {
        let tables = ["wells", "1_odd_name"];
        let written = names(&["wells", "_1_odd_name"]);

        let paired = pair(&tables, &written).expect("one rename is attributable");

        assert_eq!(paired[0].1.as_str(), "wells");
        assert_eq!(paired[1].1.as_str(), "_1_odd_name");
    }

    /// Two renames at once cannot be told apart, and guessing is what caused
    /// the mispairing, so this says so instead.
    #[test]
    fn two_renames_at_once_are_refused_rather_than_guessed() {
        let tables = ["1_odd", "2_odd"];
        let written = names(&["_1_odd", "_2_odd"]);

        let error = pair(&tables, &written).expect_err("ambiguous");

        assert!(error.contains("cannot be told"), "{error}");
    }

    #[test]
    fn a_table_with_no_layer_at_all_is_an_error() {
        let tables = ["wells", "pads"];
        let written = names(&["wells"]);

        let error = pair(&tables, &written).expect_err("a lost table");

        assert!(error.contains("no layer for pads"), "{error}");
    }
}
