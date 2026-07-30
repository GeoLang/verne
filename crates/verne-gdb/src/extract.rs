//! Extracting a geodatabase into a form ptolemy accepts.
//!
//! The source is still read-only: [`GdbSource::extract`] takes `&self` like
//! everything else on it, reads through the same open dataset the inventory
//! does, and writes only into the directory it is handed.
//!
//! Nothing here decides whether a thing can be carried. That was decided in
//! `verdict.rs`, and the log restates the verdict rather than forming a second
//! opinion, so a report and the extraction beside it cannot disagree about the
//! same row.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use verne_core::{
    DatasetPlan, ExtractionLog, GEOPACKAGE_FILE, Item, ItemKind, NewDataset, NewDomain, NewField,
    NewRelationship, NewSchema, NewSubtype, SIDECAR_FILE, Sidecar, Source,
};

use crate::geopackage;
use crate::glue::{Domain, DomainKind, Relationship};
use crate::scan::{self, Scan, Table};
use crate::source::{GdbError, GdbSource};
use crate::verdict;

/// Where an extraction landed.
#[derive(Debug, Clone)]
pub struct Extraction {
    pub directory: PathBuf,
    pub sidecar_path: PathBuf,
    /// The GeoPackage the features went to, absent when none was written.
    pub geopackage_path: Option<PathBuf>,
    pub sidecar: Sidecar,
}

impl GdbSource {
    /// Read the geodatabase and write it out into `directory`.
    ///
    /// `&self` throughout: the geodatabase is open read-only and nothing here
    /// touches it. The `operator` is recorded in the log, which is what makes
    /// an extraction accountable rather than anonymous.
    pub fn extract(&self, directory: &Path, operator: &str) -> Result<Extraction, GdbError> {
        let scan = scan::scan(&self.dataset);
        let items = verdict::items(&scan);
        if items.is_empty() {
            return Err(GdbError::NothingFound);
        }
        std::fs::create_dir_all(directory).map_err(|source| GdbError::Write {
            path: directory.display().to_string(),
            source,
        })?;

        let mut placed: Vec<(Item, Placed)> = Vec::new();
        let mut conversions: Vec<Conversion> = Vec::new();

        let mut datasets = dataset_plans(&scan, operator, &mut placed, &mut conversions);
        let relationships = relationship_classes(&scan, &datasets, &mut placed);

        let geopackage_path = directory.join(GEOPACKAGE_FILE);
        let tables: Vec<&str> = datasets
            .iter()
            .map(|plan| plan.source_table.as_str())
            .collect();
        let layers =
            geopackage::write(&self.dataset, &geopackage_path, &tables).map_err(|message| {
                GdbError::Convert {
                    path: geopackage_path.display().to_string(),
                    message,
                }
            })?;
        for layer in &layers {
            let Some(plan) = datasets
                .iter_mut()
                .find(|plan| plan.source_table == layer.source_table)
            else {
                continue;
            };
            plan.layer = Some(layer.name.clone());
            conversions.push(geopackage_conversion(&scan, layer));
        }

        let mut log = ExtractionLog::new(operator);
        // walked in report order, so the log reads down the same list the
        // markdown report does
        for item in &items {
            match placed.iter().find(|(held, _)| held == item) {
                Some((_, Placed::At(destination))) => log.carried(item, destination),
                Some((_, Placed::Left(reason))) => log.skipped(item, reason),
                None => log.skipped(item, left_behind(item.kind)),
            }
        }
        for conversion in conversions {
            log.converted(
                conversion.location,
                conversion.kind,
                conversion.detail,
                conversion.destination,
                conversion.losses,
            );
        }

        let sidecar = Sidecar {
            source: self.describe(),
            geopackage: Some(GEOPACKAGE_FILE.to_string()),
            datasets,
            relationships,
            log,
        };
        let sidecar_path = directory.join(SIDECAR_FILE);
        std::fs::write(&sidecar_path, sidecar.to_json() + "\n").map_err(|source| {
            GdbError::Write {
                path: sidecar_path.display().to_string(),
                source,
            }
        })?;

        Ok(Extraction {
            directory: directory.to_path_buf(),
            sidecar_path,
            geopackage_path: Some(geopackage_path),
            sidecar,
        })
    }
}

/// What the GeoPackage step did to one table, beside what the report already
/// says about it. A GeoPackage is a good deal closer to a geodatabase than
/// ptolemy is, so most of this is empty and the losses that stay are the ones
/// worth naming.
fn geopackage_conversion(scan: &Scan, layer: &geopackage::Layer) -> Conversion {
    let mut losses = Vec::new();
    if layer.renamed() {
        losses.push(format!(
            "GDAL could not use {} as a GeoPackage table name and wrote the layer as {}, so the sidecar names the layer and not the source table",
            layer.source_table, layer.name
        ));
    }
    if !layer.dropped_fields.is_empty() {
        losses.push(format!(
            "the field{} {} are in the geodatabase and not in the layer that came out",
            plural(layer.dropped_fields.len()),
            layer.dropped_fields.join(", ")
        ));
    }
    if let Some(table) = scan.table(&layer.source_table) {
        if table.features.is_some_and(|count| count != layer.features) {
            losses.push(format!(
                "the geodatabase counts {} feature{} and the layer holds {}",
                table.features.unwrap_or_default(),
                plural(table.features.unwrap_or_default() as usize),
                layer.features
            ));
        }
        // OBJECTID is the feature id, not a field, so -preserve_fid is what
        // keeps the numbers a relationship class is keyed on
        if !table.fields.iter().any(|field| field.name == "OBJECTID") {
            losses.push(
                "OBJECTID is the geodatabase's feature id rather than a field, so it survives as the GeoPackage's fid and is not a column anything can be joined on by name".to_string(),
            );
        }
    }
    Conversion {
        location: layer.source_table.clone(),
        kind: ItemKind::FeatureCollection,
        detail: format!(
            "{} feature{}",
            layer.features,
            plural(layer.features as usize)
        ),
        destination: format!("{GEOPACKAGE_FILE} layer {}", layer.name),
        losses,
    }
}

/// What became of one row of the report.
enum Placed {
    /// Written out, here.
    At(String),
    /// Not written out, for a reason this extraction has and the report does
    /// not.
    Left(String),
}

/// A loss the extraction itself took, which no verdict covers.
struct Conversion {
    location: String,
    kind: ItemKind,
    detail: String,
    destination: String,
    losses: Vec<String>,
}

/// Why a thing the inventory judged was not written out. A row whose verdict
/// already says there is no home for it keeps the verdict's own reason: this
/// only ever answers for a row that could have been carried and was not.
fn left_behind(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::EmbeddedResource => {
            "attachments are not extracted: the blobs are a slice of work of their own, and verne opens none of them"
        }
        ItemKind::Hierarchy => {
            "ptolemy has no container above a dataset, so there is nothing to create for the grouping itself; the classes it held each became a dataset"
        }
        ItemKind::Metadata => {
            "ptolemy takes a metadata record through a route of its own, which this extraction does not use"
        }
        _ => {
            "this extraction writes datasets, domains, subtypes and relationship classes, and nothing else"
        }
    }
}

// ─── Datasets, domains and subtypes ─────────────────────────────────

fn dataset_plans(
    scan: &Scan,
    operator: &str,
    placed: &mut Vec<(Item, Placed)>,
    conversions: &mut Vec<Conversion>,
) -> Vec<DatasetPlan> {
    let mut plans = Vec::new();
    for table in scan.user_tables() {
        let (geometry_type, dropped) = ptolemy_geometry(table);
        if !dropped.is_empty() {
            conversions.push(Conversion {
                location: table.name.clone(),
                kind: ItemKind::FeatureCollection,
                detail: format!("geometry type recorded as {geometry_type}"),
                destination: format!("dataset {}", table.name),
                losses: dropped,
            });
        }
        let (domains, subtypes) = semantics_of(scan, table, conversions);
        let schema = columns_of(table, conversions);
        placed.push((
            verdict::table_item(table),
            Placed::At(format!("dataset {}", table.name)),
        ));
        if let Some(item) = verdict::subtype_item(table) {
            let destination = if subtypes.is_empty() {
                Placed::Left(
                    "every subtype in the definition carries a code ptolemy cannot hold, so there was nothing left to create".into(),
                )
            } else {
                Placed::At(format!("subtypes of {}", table.name))
            };
            placed.push((item, destination));
        }
        plans.push(DatasetPlan {
            source_table: table.name.clone(),
            layer: None,
            dataset: NewDataset {
                name: table.name.clone(),
                srid: table.srid.unwrap_or(DEFAULT_SRID),
                geometry_type: geometry_type.to_string(),
                created_by: operator.to_string(),
            },
            schema,
            domains,
            subtypes,
        });
    }
    place_domains(scan, &plans, placed, conversions);
    plans
}

/// ptolemy's default when a request names no srid, and the only sane guess for
/// a table that has no spatial reference of its own to report.
const DEFAULT_SRID: i32 = 4326;

/// The domains and subtypes that belong to one table's dataset.
fn semantics_of(
    scan: &Scan,
    table: &Table,
    conversions: &mut Vec<Conversion>,
) -> (Vec<NewDomain>, Vec<NewSubtype>) {
    let mut domains = Vec::new();
    for name in domains_used_by(table) {
        let Some(domain) = scan.domains.iter().find(|held| held.name == name) else {
            continue;
        };
        if let Some(new) = new_domain(domain, &table.name, conversions) {
            domains.push(new);
        }
    }
    (domains, subtypes_of(table, conversions))
}

/// The table's columns as ptolemy's dataset schema holds them.
///
/// This is where a field alias lands. It is the only home the platform has for
/// one, and storing it is the whole of what happens to it: nothing displays it.
fn columns_of(table: &Table, conversions: &mut Vec<Conversion>) -> NewSchema {
    let mut fields = Vec::new();
    let mut retyped = Vec::new();
    for field in &table.fields {
        let (field_type, approximated) = verdict::schema_field_type(&field.kind);
        if let Some(loss) = approximated {
            retyped.push(loss);
        }
        fields.push(NewField {
            name: field.name.clone(),
            field_type: field_type.to_string(),
            required: field.not_null,
            alias: field.alias.clone(),
        });
    }
    if !retyped.is_empty() {
        conversions.push(Conversion {
            location: table.name.clone(),
            kind: ItemKind::AttributeSchema,
            detail: format!(
                "{} column{} declared as the nearest type ptolemy has",
                retyped.len(),
                plural(retyped.len())
            ),
            destination: format!("schema of {}", table.name),
            losses: retyped,
        });
    }
    NewSchema { fields }
}

/// Every domain one table names, whether through a field binding or through a
/// subtype's per-field assignment, in a stable order and without repeats.
fn domains_used_by(table: &Table) -> Vec<String> {
    let bound = table.fields.iter().filter_map(|field| field.domain.clone());
    let assigned = table
        .definition
        .subtypes
        .iter()
        .flat_map(|subtype| subtype.fields.iter())
        .filter_map(|field| field.domain.clone());
    let mut names: Vec<String> = bound.chain(assigned).collect();
    names.sort();
    names.dedup();
    names
}

fn new_domain(
    domain: &Domain,
    dataset: &str,
    conversions: &mut Vec<Conversion>,
) -> Option<NewDomain> {
    let (field_type, retyped) = verdict::domain_field_type(&domain.field_type);
    if let Some(loss) = retyped {
        conversions.push(Conversion {
            location: domain.name.clone(),
            kind: ItemKind::AttributeSchema,
            detail: format!("domain field type recorded as {field_type}"),
            destination: format!("domains of {dataset}"),
            losses: vec![loss],
        });
    }
    match &domain.kind {
        DomainKind::Coded(values) => Some(NewDomain::coded(
            domain.name.clone(),
            field_type,
            values
                .iter()
                .map(|(code, label)| (code.clone(), label.clone())),
        )),
        DomainKind::Range { min, max } => Some(NewDomain::range(
            domain.name.clone(),
            field_type,
            min.map(|bound| bound.value),
            max.map(|bound| bound.value),
        )),
        // the report already calls a glob domain unsupported
        DomainKind::Glob => None,
    }
}

/// Log every domain once, against the datasets it went into. A geodatabase
/// holds one domain for the whole workspace and ptolemy holds one per dataset,
/// so a domain two tables use becomes two rows, and one no table uses has no
/// dataset to live in at all.
fn place_domains(
    scan: &Scan,
    plans: &[DatasetPlan],
    placed: &mut Vec<(Item, Placed)>,
    conversions: &mut Vec<Conversion>,
) {
    for domain in &scan.domains {
        let holders: Vec<&str> = plans
            .iter()
            .filter(|plan| plan.domains.iter().any(|held| held.name == domain.name))
            .map(|plan| plan.dataset.name.as_str())
            .collect();
        let item = verdict::domain_item(scan, domain);
        if holders.is_empty() {
            placed.push((
                item,
                Placed::Left(
                    "ptolemy's domains hang off a dataset, and no field or subtype in this geodatabase is bound to this one, so there is no dataset to put it in".into(),
                ),
            ));
            continue;
        }
        if holders.len() > 1 {
            conversions.push(Conversion {
                location: domain.name.clone(),
                kind: ItemKind::AttributeSchema,
                detail: format!("one domain copied into {} datasets", holders.len()),
                destination: format!("domains of {}", holders.join(", ")),
                losses: vec![format!(
                    "the geodatabase holds one {} for the whole workspace and ptolemy holds one per dataset, so {} separate copies were written and editing one of them no longer changes the others",
                    domain.name,
                    holders.len()
                )],
            });
        }
        placed.push((
            item,
            Placed::At(format!("domains of {}", holders.join(", "))),
        ));
    }
}

fn subtypes_of(table: &Table, conversions: &mut Vec<Conversion>) -> Vec<NewSubtype> {
    let definition = &table.definition;
    let Some(field) = &definition.subtype_field else {
        return Vec::new();
    };
    let mut subtypes = Vec::new();
    let mut untyped = Vec::new();
    for subtype in &definition.subtypes {
        // ptolemy keys a subtype on an integer, so a code that is not one has
        // nothing to be written as
        let Ok(code) = subtype.code.parse::<i32>() else {
            conversions.push(Conversion {
                location: table.name.clone(),
                kind: ItemKind::AttributeSchema,
                detail: format!("subtype \"{}\" with code {}", subtype.name, subtype.code),
                destination: format!("subtypes of {}", table.name),
                losses: vec![format!(
                    "ptolemy's subtype code is an integer and this one is \"{}\", so the subtype was not written",
                    subtype.code
                )],
            });
            continue;
        };
        let mut default_values = serde_json::Map::new();
        let mut domain_assignments = BTreeMap::new();
        for info in &subtype.fields {
            if let Some(default) = &info.default_value {
                default_values.insert(
                    info.name.clone(),
                    serde_json::Value::String(default.clone()),
                );
                untyped.push(format!("{}.{}", subtype.name, info.name));
            }
            if let Some(domain) = &info.domain {
                domain_assignments.insert(info.name.clone(), domain.clone());
            }
        }
        subtypes.push(NewSubtype {
            subtype_field: field.clone(),
            name: subtype.name.clone(),
            code,
            default_values,
            domain_assignments,
        });
    }
    if !untyped.is_empty() {
        conversions.push(Conversion {
            location: table.name.clone(),
            kind: ItemKind::AttributeSchema,
            detail: format!("{} subtype default value{}", untyped.len(), plural(untyped.len())),
            destination: format!("subtypes of {}", table.name),
            losses: vec![format!(
                "the defaults ({}) come out of the definition XML, which declares each one's type in an attribute verne does not read, so they were written as the text they appear as",
                untyped.join(", ")
            )],
        });
    }
    subtypes
}

// ─── Relationship classes ───────────────────────────────────────────

fn relationship_classes(
    scan: &Scan,
    plans: &[DatasetPlan],
    placed: &mut Vec<(Item, Placed)>,
) -> Vec<NewRelationship> {
    let mut classes = Vec::new();
    for relationship in scan
        .relationships
        .iter()
        .filter(|held| held.related_table_type.as_deref() != Some("media"))
    {
        let item = verdict::relationship_item(relationship);
        match new_relationship(relationship, plans) {
            Ok(class) => {
                placed.push((
                    item,
                    Placed::At(format!("relationship class {}", class.name)),
                ));
                classes.push(class);
            }
            Err(reason) => placed.push((item, Placed::Left(reason))),
        }
    }
    classes
}

/// One class, with a many-to-one turned round.
///
/// ptolemy's cardinality is one of three and many-to-one is not among them, so
/// the class is stored the other way about, which moves the key and the labels
/// with it. The report already names this as the loss it is.
fn new_relationship(
    relationship: &Relationship,
    plans: &[DatasetPlan],
) -> Result<NewRelationship, String> {
    let reversed = relationship.cardinality == "many to one";
    let (origin, destination) = if reversed {
        (&relationship.right_table, &relationship.left_table)
    } else {
        (&relationship.left_table, &relationship.right_table)
    };
    let (origin, destination) = (side(origin, plans)?, side(destination, plans)?);
    // the key ptolemy wants is the one on the destination side that holds the
    // origin's key, which is whichever field list belongs to that side
    let keys = if reversed {
        &relationship.left_fields
    } else {
        &relationship.right_fields
    };
    let key = keys.first().ok_or_else(|| {
        format!(
            "{} names no field on its {} side, and ptolemy's class is keyed on one",
            relationship.name,
            if reversed { "left" } else { "right" }
        )
    })?;
    let (forward, backward) = if reversed {
        (&relationship.backward_label, &relationship.forward_label)
    } else {
        (&relationship.forward_label, &relationship.backward_label)
    };
    Ok(NewRelationship {
        name: relationship.name.clone(),
        origin_dataset: origin,
        destination_dataset: destination,
        origin_foreign_key: key.clone(),
        cardinality: cardinality(relationship.cardinality).to_string(),
        forward_label: forward.clone().unwrap_or_default(),
        backward_label: backward.clone().unwrap_or_default(),
    })
}

/// The dataset one side of a class names, refused when nothing was extracted
/// for it: ptolemy takes two dataset ids and there is no id for a table that
/// never became a dataset.
fn side(table: &Option<String>, plans: &[DatasetPlan]) -> Result<String, String> {
    let name = table
        .as_deref()
        .ok_or_else(|| "the class names no table on one side".to_string())?;
    if plans.iter().any(|plan| plan.source_table == name) {
        Ok(name.to_string())
    } else {
        Err(format!(
            "{name} is not one of the tables this extraction turned into a dataset, and a relationship class in ptolemy names two dataset ids"
        ))
    }
}

fn cardinality(gdal: &str) -> &'static str {
    match gdal {
        "one to one" => "one_to_one",
        "many to many" => "many_to_many",
        // a many-to-one was turned round on the way here
        _ => "one_to_many",
    }
}

// ─── Type mapping ───────────────────────────────────────────────────

/// ptolemy's name for a table's geometry, and what calling it that drops.
///
/// Every type is mapped here rather than left to ptolemy, which reads a name it
/// does not know as `point` and says nothing about it.
fn ptolemy_geometry(table: &Table) -> (&'static str, Vec<String>) {
    use gdal::vector::OGRwkbGeometryType as Wkb;

    let flat = crate::glue::flatten(table.geometry_code);
    let mut losses = Vec::new();
    if flat.has_z {
        losses.push(
            "the class carries a Z ordinate and ptolemy's geometry_type names 2D shapes only, so the dataset does not declare it".to_string(),
        );
    }
    if flat.has_m {
        losses.push(
            "the class carries an M ordinate and ptolemy's geometry_type names 2D shapes only, so the dataset does not declare it".to_string(),
        );
    }
    let name = match flat.code {
        Wkb::wkbPoint => "point",
        Wkb::wkbLineString => "linestring",
        Wkb::wkbPolygon => "polygon",
        Wkb::wkbMultiPoint => "multipoint",
        Wkb::wkbMultiLineString => "multilinestring",
        Wkb::wkbMultiPolygon => "multipolygon",
        Wkb::wkbGeometryCollection => "geometrycollection",
        // a table with no geometry still needs a name in the column; anything
        // else is a shape ptolemy has no narrower word for
        _ => {
            if table.geometry.is_some() {
                losses.push(format!(
                    "{} is a shape ptolemy has no name for, so the dataset is declared as holding any geometry",
                    table.geometry.as_deref().unwrap_or("the geometry")
                ));
            }
            "geometry"
        }
    };
    (name, losses)
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plans(names: &[&str]) -> Vec<DatasetPlan> {
        names
            .iter()
            .map(|name| DatasetPlan {
                source_table: (*name).to_string(),
                layer: None,
                dataset: NewDataset {
                    name: (*name).to_string(),
                    srid: DEFAULT_SRID,
                    geometry_type: "point".into(),
                    created_by: "operator".into(),
                },
                schema: NewSchema { fields: Vec::new() },
                domains: Vec::new(),
                subtypes: Vec::new(),
            })
            .collect()
    }

    fn relationship(cardinality: &'static str) -> Relationship {
        Relationship {
            name: "pads_wells".into(),
            cardinality,
            kind: "association",
            related_table_type: Some("features".into()),
            left_table: Some("pads".into()),
            right_table: Some("wells".into()),
            left_fields: vec!["well_id".into()],
            right_fields: vec!["OBJECTID".into()],
            mapping_table: None,
            forward_label: Some("sits on well".into()),
            backward_label: Some("has pads".into()),
        }
    }

    /// OpenFileGDB will not write a many-to-one class, so this is the one path
    /// no fixture can reach: ptolemy has no such cardinality, and the report
    /// already says the class has to be stored the other way about. Storing it
    /// that way moves the key and the labels with it.
    #[test]
    fn a_many_to_one_class_is_turned_round() {
        let plans = plans(&["pads", "wells"]);
        let class = new_relationship(&relationship("many to one"), &plans).expect("turned round");

        assert_eq!(class.origin_dataset, "wells");
        assert_eq!(class.destination_dataset, "pads");
        assert_eq!(class.cardinality, "one_to_many");
        // the key ptolemy wants is the one on whichever table is now the
        // destination, which is pads
        assert_eq!(class.origin_foreign_key, "well_id");
        assert_eq!(class.forward_label, "has pads");
        assert_eq!(class.backward_label, "sits on well");
    }

    #[test]
    fn an_ordinary_class_keeps_its_sides() {
        let plans = plans(&["pads", "wells"]);
        let class = new_relationship(&relationship("one to many"), &plans).expect("kept");

        assert_eq!(class.origin_dataset, "pads");
        assert_eq!(class.destination_dataset, "wells");
        assert_eq!(class.origin_foreign_key, "OBJECTID");
        assert_eq!(class.forward_label, "sits on well");
    }

    /// ptolemy's class names two dataset ids, and a table that never became a
    /// dataset has none.
    #[test]
    fn a_class_naming_a_table_with_no_dataset_is_refused() {
        let plans = plans(&["pads"]);
        let refused = new_relationship(&relationship("one to many"), &plans).expect_err("refused");
        assert!(
            refused.contains("wells is not one of the tables"),
            "{refused}"
        );
    }

    #[test]
    fn a_many_to_many_class_keeps_its_cardinality() {
        assert_eq!(cardinality("many to many"), "many_to_many");
        assert_eq!(cardinality("one to one"), "one_to_one");
    }
}
