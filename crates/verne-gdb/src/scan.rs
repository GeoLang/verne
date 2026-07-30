//! One pass over the geodatabase, tallying what it holds. No verdicts here.

use gdal::Dataset;
use gdal::vector::{LayerAccess, field_type_to_name, geometry_type_to_name};

use crate::glue;

/// What a table is for, which decides whether verne reads it as data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableRole {
    /// A feature class or table the operator put there.
    User,
    /// A GDB_* table: the geodatabase's own bookkeeping.
    System,
    /// A `*__ATTACH` table: the blobs behind an attachment relationship.
    Attachment,
}

#[derive(Debug)]
pub struct Field {
    pub name: String,
    /// The Esri alias, absent when the field never had one.
    pub alias: Option<String>,
    pub kind: String,
    pub domain: Option<String>,
}

#[derive(Debug)]
pub struct Table {
    pub name: String,
    pub role: TableRole,
    /// Geometry type as GDAL names it, absent for a table without geometry.
    pub geometry: Option<String>,
    pub features: Option<u64>,
    pub fields: Vec<Field>,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub tables: Vec<Table>,
}

impl Scan {
    pub fn user_tables(&self) -> impl Iterator<Item = &Table> {
        self.tables
            .iter()
            .filter(|table| table.role == TableRole::User)
    }
}

pub fn scan(dataset: &Dataset) -> Scan {
    let mut scan = Scan::default();
    for layer in dataset.layers() {
        let name = layer.name();
        let defn = layer.defn();
        let geometry_type = defn.geometry_type();
        let fields = defn
            .fields()
            .enumerate()
            .map(|(index, field)| Field {
                name: field.name(),
                alias: alias(&field),
                kind: field_type_to_name(field.field_type()),
                domain: glue::field_domain_name(&layer, index),
            })
            .collect();
        scan.tables.push(Table {
            role: role(&name),
            name,
            geometry: match geometry_type {
                gdal::vector::OGRwkbGeometryType::wkbNone => None,
                other => Some(geometry_type_to_name(other)),
            },
            // a count GDAL would have to walk the table for is not worth it
            features: layer.try_feature_count(),
            fields,
        });
    }
    scan
}

/// GDAL returns the field name again when a field has no alias.
fn alias(field: &gdal::vector::Field<'_>) -> Option<String> {
    let alias = field.alternative_name();
    if alias.is_empty() || alias == field.name() {
        None
    } else {
        Some(alias)
    }
}

fn role(name: &str) -> TableRole {
    if name.starts_with("GDB_") {
        TableRole::System
    } else if name.ends_with("__ATTACH") {
        TableRole::Attachment
    } else {
        TableRole::User
    }
}
