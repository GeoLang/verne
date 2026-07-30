//! One pass over the geodatabase, tallying what it holds. No verdicts here.

use gdal::Dataset;
use gdal::vector::sql::Dialect;
use gdal::vector::{LayerAccess, field_type_to_name, geometry_type_to_name};

use crate::definition::{self, Definition};
use crate::glue;

/// Definition roots verne inventories in a row of their own, so anything else
/// in the catalogue is an item it cannot interpret.
const INTERPRETED_ROOTS: &[&str] = &[
    "DEWorkspace",
    "DEFeatureDataset",
    "DEFeatureClassInfo",
    "DETableInfo",
    "DERelationshipClassInfo",
];

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
    /// What the Esri definition blob says about it.
    pub definition: Definition,
    /// Whether the layer carries an ISO or FGDC metadata record.
    pub metadata: bool,
}

/// An entry in the geodatabase catalogue that verne has no reader for, kept so
/// the report can name what it could not look inside.
#[derive(Debug)]
pub struct CatalogItem {
    pub name: String,
    /// Root element of the definition, which is how the catalogue says what
    /// kind of item this is.
    pub kind: String,
}

#[derive(Debug, Default)]
pub struct Scan {
    pub tables: Vec<Table>,
    pub domains: Vec<glue::Domain>,
    pub relationships: Vec<glue::Relationship>,
    pub catalog: Vec<CatalogItem>,
}

impl Scan {
    pub fn user_tables(&self) -> impl Iterator<Item = &Table> {
        self.tables
            .iter()
            .filter(|table| table.role == TableRole::User)
    }

    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|table| table.name == name)
    }

    /// Feature datasets, in the order the layers named them.
    pub fn feature_datasets(&self) -> Vec<(String, Vec<String>)> {
        let mut datasets: Vec<(String, Vec<String>)> = Vec::new();
        for table in self.user_tables() {
            let Some(name) = table.definition.feature_dataset() else {
                continue;
            };
            match datasets.iter_mut().find(|(held, _)| held == name) {
                Some((_, members)) => members.push(table.name.clone()),
                None => datasets.push((name.to_string(), vec![table.name.clone()])),
            }
        }
        datasets
    }

    /// Every `table.field` bound to this domain, in the order the tables came.
    pub fn domain_users(&self, domain: &str) -> Vec<String> {
        self.tables
            .iter()
            .flat_map(|table| {
                table
                    .fields
                    .iter()
                    .filter(move |field| field.domain.as_deref() == Some(domain))
                    .map(move |field| format!("{}.{}", table.name, field.name))
            })
            .collect()
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
            definition: Definition::default(),
            metadata: false,
        });
    }
    // the definition and the metadata record come back through special SQL,
    // which needs the layer's name rather than the layer. Only a user table has
    // a definition worth reading: a system table's comes back empty.
    for index in 0..scan.tables.len() {
        if scan.tables[index].role != TableRole::User {
            continue;
        }
        let name = scan.tables[index].name.clone();
        let definition = special_sql(dataset, &format!("GetLayerDefinition {name}"))
            .map(|xml| definition::parse(&xml))
            .unwrap_or_default();
        scan.tables[index].metadata = special_sql(dataset, &format!("GetLayerMetadata {name}"))
            .is_some_and(|record| !record.trim().is_empty());
        scan.tables[index].definition = definition;
    }
    scan.domains = glue::domain_names(dataset)
        .iter()
        .filter_map(|name| glue::domain(dataset, name))
        .collect();
    scan.relationships = glue::relationship_names(dataset)
        .iter()
        .filter_map(|name| glue::relationship(dataset, name))
        .collect();
    // the catalogue holds a row for everything above, domains included, so
    // anything already inventoried under its own name is not an item verne
    // failed to read
    let inventoried: Vec<&str> = scan
        .tables
        .iter()
        .map(|table| table.name.as_str())
        .chain(scan.domains.iter().map(|domain| domain.name.as_str()))
        .chain(
            scan.relationships
                .iter()
                .map(|relationship| relationship.name.as_str()),
        )
        .collect();
    scan.catalog = catalog(dataset, &inventoried);
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

/// One value out of a special SQL statement such as `GetLayerDefinition`.
fn special_sql(dataset: &Dataset, statement: &str) -> Option<String> {
    let mut result = dataset
        .execute_sql(statement, None, Dialect::DEFAULT)
        .ok()??;
    let feature = result.features().next()?;
    feature.field_as_string(0).ok()?
}

/// Catalogue entries verne has no reader for, by the root element of their
/// definition. A geodatabase without a readable catalogue yields none.
fn catalog(dataset: &Dataset, inventoried: &[&str]) -> Vec<CatalogItem> {
    let Ok(mut items) = dataset.layer_by_name("GDB_Items") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for feature in items.features() {
        let Some(xml) = named_field(&feature, "Definition") else {
            continue;
        };
        let Some(kind) = definition::root_element(&xml) else {
            continue;
        };
        if INTERPRETED_ROOTS.contains(&kind.as_str()) {
            continue;
        }
        let name = named_field(&feature, "Name").unwrap_or_default();
        if inventoried.contains(&name.as_str()) {
            continue;
        }
        out.push(CatalogItem { name, kind });
    }
    out
}

fn named_field(feature: &gdal::vector::Feature<'_>, name: &str) -> Option<String> {
    let index = feature.field_index(name).ok()?;
    feature.field_as_string(index).ok()?
}
