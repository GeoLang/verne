//! The GDAL C API that georust/gdal 0.19 does not wrap. Every call here reads;
//! none of them writes.
//!
//! The pointers come from the layer or dataset that owns them and are used
//! before either is dropped, so the borrow the caller holds is what keeps them
//! alive.

use std::ffi::{CStr, c_char};

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
