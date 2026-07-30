//! The GDAL C API that georust/gdal 0.19 does not wrap. Every call here reads;
//! none of them writes.
//!
//! The pointers come from the layer or dataset that owns them and are used
//! before either is dropped, so the borrow the caller holds is what keeps them
//! alive.

use std::ffi::{CStr, CString, c_char};

use gdal::Dataset;
use gdal::vector::LayerAccess;

/// Name of the field domain a field is bound to, if it is bound to one.
pub fn field_domain_name<L: LayerAccess>(layer: &L, index: usize) -> Option<String> {
    unsafe {
        let defn = gdal_sys::OGR_L_GetLayerDefn(layer.c_layer());
        if defn.is_null() {
            return None;
        }
        let field = gdal_sys::OGR_FD_GetFieldDefn(defn, index as i32);
        if field.is_null() {
            return None;
        }
        string(gdal_sys::OGR_Fld_GetDomainName(field))
    }
}

/// A borrowed C string, with null and empty both read as "not set".
///
/// # Safety
/// `ptr` must be null or a valid NUL-terminated string owned by GDAL.
unsafe fn string(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    if text.is_empty() { None } else { Some(text) }
}

/// A GDAL string list (`char **`) copied out. Freeing it, or not, is the
/// caller's business: only the lists GDAL builds fresh are the caller's to
/// free, and the ones hanging off a domain or a relationship are not.
///
/// # Safety
/// `list` must be null or a valid NULL-terminated array of C strings.
unsafe fn string_list(list: *mut *mut c_char) -> Vec<String> {
    let mut out = Vec::new();
    if list.is_null() {
        return out;
    }
    let mut index = 0;
    loop {
        let entry = unsafe { *list.offset(index) };
        if entry.is_null() {
            break;
        }
        out.push(
            unsafe { CStr::from_ptr(entry) }
                .to_string_lossy()
                .into_owned(),
        );
        index += 1;
    }
    out
}

/// A geometry type split into the flat shape and the dimensions hung off it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Flat {
    pub code: gdal_sys::OGRwkbGeometryType::Type,
    pub has_z: bool,
    pub has_m: bool,
}

/// The 2D type behind a geometry type, and whether the original carried Z or M.
/// Pure arithmetic on the type code: no dataset and no pointers.
pub fn flatten(code: gdal_sys::OGRwkbGeometryType::Type) -> Flat {
    unsafe {
        Flat {
            code: gdal_sys::OGR_GT_Flatten(code),
            has_z: gdal_sys::OGR_GT_HasZ(code) != 0,
            has_m: gdal_sys::OGR_GT_HasM(code) != 0,
        }
    }
}

/// How a domain constrains a field.
#[derive(Debug, Clone, PartialEq)]
pub enum DomainKind {
    /// A fixed set of code and label pairs.
    Coded(Vec<(String, String)>),
    /// An interval, with whether each end is part of it. An end that is not
    /// there at all leaves the range open on that side.
    Range {
        min: Option<Bound>,
        max: Option<Bound>,
    },
    /// A glob pattern. OpenFileGDB refuses to read these, so one can only reach
    /// verne through another driver.
    Glob,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bound {
    pub value: f64,
    pub inclusive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Domain {
    pub name: String,
    pub description: Option<String>,
    /// Field type the domain applies to, as GDAL names it.
    pub field_type: String,
    pub kind: DomainKind,
    pub split_policy: &'static str,
    pub merge_policy: &'static str,
}

/// Names of every field domain in the dataset.
pub fn domain_names(dataset: &Dataset) -> Vec<String> {
    unsafe {
        let list =
            gdal_sys::GDALDatasetGetFieldDomainNames(dataset.c_dataset(), std::ptr::null_mut());
        let names = string_list(list);
        gdal_sys::CSLDestroy(list);
        names
    }
}

pub fn domain(dataset: &Dataset, name: &str) -> Option<Domain> {
    let c_name = CString::new(name).ok()?;
    unsafe {
        // GDALDataset::GetFieldDomain hands back a const pointer the dataset
        // owns, so this must not be destroyed: everything is copied out instead
        let handle = gdal_sys::GDALDatasetGetFieldDomain(dataset.c_dataset(), c_name.as_ptr());
        if handle.is_null() {
            return None;
        }
        let field_type = gdal_sys::OGR_FldDomain_GetFieldType(handle);
        let kind = match gdal_sys::OGR_FldDomain_GetDomainType(handle) {
            gdal_sys::OGRFieldDomainType::OFDT_CODED => DomainKind::Coded(coded_values(
                gdal_sys::OGR_CodedFldDomain_GetEnumeration(handle),
            )),
            gdal_sys::OGRFieldDomainType::OFDT_RANGE => DomainKind::Range {
                min: bound(handle, field_type, End::Min),
                max: bound(handle, field_type, End::Max),
            },
            _ => DomainKind::Glob,
        };
        let domain = Domain {
            name: string(gdal_sys::OGR_FldDomain_GetName(handle)).unwrap_or_else(|| name.into()),
            description: string(gdal_sys::OGR_FldDomain_GetDescription(handle)),
            field_type: gdal::vector::field_type_to_name(field_type),
            kind,
            split_policy: split_policy(gdal_sys::OGR_FldDomain_GetSplitPolicy(handle)),
            merge_policy: merge_policy(gdal_sys::OGR_FldDomain_GetMergePolicy(handle)),
        };
        Some(domain)
    }
}

/// # Safety
/// `values` must be null or GDAL's enumeration, which ends at the first pair
/// with a null code.
unsafe fn coded_values(values: *const gdal_sys::OGRCodedValue) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if values.is_null() {
        return out;
    }
    let mut index = 0;
    loop {
        let entry = unsafe { *values.offset(index) };
        if entry.pszCode.is_null() {
            break;
        }
        let code = unsafe { CStr::from_ptr(entry.pszCode) }
            .to_string_lossy()
            .into_owned();
        // a code with no label stands for itself
        let label = unsafe { string(entry.pszValue) }.unwrap_or_else(|| code.clone());
        out.push((code, label));
        index += 1;
    }
    out
}

#[derive(Clone, Copy)]
enum End {
    Min,
    Max,
}

/// One end of a range domain, read through the field type the domain declares.
///
/// # Safety
/// `handle` must be a live range domain.
unsafe fn bound(
    handle: gdal_sys::OGRFieldDomainH,
    field_type: gdal_sys::OGRFieldType::Type,
    end: End,
) -> Option<Bound> {
    let mut inclusive = false;
    let field = unsafe {
        match end {
            End::Min => gdal_sys::OGR_RangeFldDomain_GetMin(handle, &mut inclusive),
            End::Max => gdal_sys::OGR_RangeFldDomain_GetMax(handle, &mut inclusive),
        }
    };
    // an unset or null end means the range is open on that side
    if field.is_null()
        || unsafe { gdal_sys::OGR_RawField_IsUnset(field) } != 0
        || unsafe { gdal_sys::OGR_RawField_IsNull(field) } != 0
    {
        return None;
    }
    let field = unsafe { &*field };
    let value = match field_type {
        gdal_sys::OGRFieldType::OFTInteger => f64::from(unsafe { field.Integer }),
        gdal_sys::OGRFieldType::OFTInteger64 => (unsafe { field.Integer64 }) as f64,
        gdal_sys::OGRFieldType::OFTReal => unsafe { field.Real },
        _ => return None,
    };
    Some(Bound { value, inclusive })
}

fn split_policy(policy: gdal_sys::OGRFieldDomainSplitPolicy::Type) -> &'static str {
    match policy {
        gdal_sys::OGRFieldDomainSplitPolicy::OFDSP_DUPLICATE => "duplicate",
        gdal_sys::OGRFieldDomainSplitPolicy::OFDSP_GEOMETRY_RATIO => "geometry ratio",
        _ => "default value",
    }
}

fn merge_policy(policy: gdal_sys::OGRFieldDomainMergePolicy::Type) -> &'static str {
    match policy {
        gdal_sys::OGRFieldDomainMergePolicy::OFDMP_SUM => "sum",
        gdal_sys::OGRFieldDomainMergePolicy::OFDMP_GEOMETRY_WEIGHTED => "geometry weighted",
        _ => "default value",
    }
}

/// A relationship class, as GDAL models one.
#[derive(Debug, Clone, PartialEq)]
pub struct Relationship {
    pub name: String,
    pub cardinality: &'static str,
    /// `composite`, `association` or `aggregation`.
    pub kind: &'static str,
    /// `features` or `media`; media is how an attachment relationship reads.
    pub related_table_type: Option<String>,
    pub left_table: Option<String>,
    pub right_table: Option<String>,
    pub left_fields: Vec<String>,
    pub right_fields: Vec<String>,
    /// The intermediate table a many-to-many class relates through.
    pub mapping_table: Option<String>,
    pub forward_label: Option<String>,
    pub backward_label: Option<String>,
}

pub fn relationship_names(dataset: &Dataset) -> Vec<String> {
    unsafe {
        let list =
            gdal_sys::GDALDatasetGetRelationshipNames(dataset.c_dataset(), std::ptr::null_mut());
        let names = string_list(list);
        gdal_sys::CSLDestroy(list);
        names
    }
}

pub fn relationship(dataset: &Dataset, name: &str) -> Option<Relationship> {
    let c_name = CString::new(name).ok()?;
    unsafe {
        // as with a domain, the dataset owns this one: no destroy, and the two
        // field lists belong to it as well, so they are copied and not freed
        let handle = gdal_sys::GDALDatasetGetRelationship(dataset.c_dataset(), c_name.as_ptr());
        if handle.is_null() {
            return None;
        }
        let left = gdal_sys::GDALRelationshipGetLeftTableFields(handle);
        let right = gdal_sys::GDALRelationshipGetRightTableFields(handle);
        let relationship = Relationship {
            name: string(gdal_sys::GDALRelationshipGetName(handle)).unwrap_or_else(|| name.into()),
            cardinality: cardinality(gdal_sys::GDALRelationshipGetCardinality(handle)),
            kind: relationship_kind(gdal_sys::GDALRelationshipGetType(handle)),
            related_table_type: string(gdal_sys::GDALRelationshipGetRelatedTableType(handle)),
            left_table: string(gdal_sys::GDALRelationshipGetLeftTableName(handle)),
            right_table: string(gdal_sys::GDALRelationshipGetRightTableName(handle)),
            left_fields: string_list(left),
            right_fields: string_list(right),
            mapping_table: string(gdal_sys::GDALRelationshipGetMappingTableName(handle)),
            forward_label: string(gdal_sys::GDALRelationshipGetForwardPathLabel(handle)),
            backward_label: string(gdal_sys::GDALRelationshipGetBackwardPathLabel(handle)),
        };
        Some(relationship)
    }
}

fn cardinality(value: gdal_sys::GDALRelationshipCardinality::Type) -> &'static str {
    match value {
        gdal_sys::GDALRelationshipCardinality::GRC_ONE_TO_ONE => "one to one",
        gdal_sys::GDALRelationshipCardinality::GRC_MANY_TO_ONE => "many to one",
        gdal_sys::GDALRelationshipCardinality::GRC_MANY_TO_MANY => "many to many",
        _ => "one to many",
    }
}

fn relationship_kind(value: gdal_sys::GDALRelationshipType::Type) -> &'static str {
    match value {
        gdal_sys::GDALRelationshipType::GRT_COMPOSITE => "composite",
        gdal_sys::GDALRelationshipType::GRT_AGGREGATION => "aggregation",
        _ => "association",
    }
}
