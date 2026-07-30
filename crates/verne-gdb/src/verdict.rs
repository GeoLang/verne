//! Turning the scan into verdicts. Every judgement about what GeoLang can hold
//! is in this file, so it can be argued with in one place.

use verne_core::{Item, ItemKind, Losses, Target, Verdict};

use crate::glue::{Domain, DomainKind, Relationship};
use crate::scan::{Scan, Table, TableRole};

/// Where anything that belongs to the geodatabase rather than to one table is
/// reported.
const ROOT: &str = "geodatabase root";

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// A verdict is faithful only when nothing was found to lose, so a new loss
/// cannot be added without downgrading the verdict that reports it.
fn verdict_for(target: Target, losses: Vec<String>) -> Verdict {
    match losses.split_first() {
        None => Verdict::faithful(target),
        Some((first, rest)) => {
            Verdict::approximated(target, Losses::one(first.clone()).and_all(rest.to_vec()))
        }
    }
}

pub fn items(scan: &Scan) -> Vec<Item> {
    let mut items = Vec::new();
    feature_datasets(scan, &mut items);
    tables(scan, &mut items);
    drawn_classes(scan, &mut items);
    subtypes(scan, &mut items);
    domains(scan, &mut items);
    relationships(scan, &mut items);
    attachments(scan, &mut items);
    orphan_attachments(scan, &mut items);
    layer_metadata(scan, &mut items);
    catalog(scan, &mut items);
    system_tables(scan, &mut items);
    versioning(&mut items);
    items
}

/// A feature dataset groups feature classes and holds them to one spatial
/// reference. ptolemy has no container above a dataset.
fn feature_datasets(scan: &Scan, items: &mut Vec<Item>) {
    for (name, members) in scan.feature_datasets() {
        items.push(Item::new(
            format!("\\{name}"),
            ItemKind::Hierarchy,
            format!(
                "feature dataset holding {} class{}: {}",
                members.len(),
                if members.len() == 1 { "" } else { "es" },
                members.join(", ")
            ),
            Verdict::approximated(
                Target::Ptolemy,
                Losses::one(
                    "ptolemy has no container above a dataset, so the grouping survives as a tag on each dataset rather than as a thing that can be opened, moved or granted as a whole",
                )
                .and(
                    "a feature dataset holds its members to one spatial reference, and nothing in ptolemy enforces that across datasets",
                ),
            ),
        ));
    }
}

/// Annotation and dimension classes: the geometry and fields are reported with
/// the other tables, so this row is about the graphics that come with them.
fn drawn_classes(scan: &Scan, items: &mut Vec<Item>) {
    for table in scan.user_tables() {
        let Some(kind) = table.definition.drawn_feature_type() else {
            continue;
        };
        items.push(Item::new(
            table.name.clone(),
            ItemKind::Styling,
            format!("{kind} graphics"),
            Verdict::unsupported(
                "the text, font, symbol and placement of an annotation or dimension class sit in a class extension that GDAL does not read, so verne cannot see them at all; jung places labels itself from the text and an anchor, so a per-feature graphic placed by hand has nothing to be carried into either",
            ),
        ));
    }
}

/// Esri subtypes go to ptolemy's subtypes table. GDAL does not model them, so
/// they come out of the layer definition XML.
fn subtypes(scan: &Scan, items: &mut Vec<Item>) {
    items.extend(scan.user_tables().filter_map(subtype_item));
}

/// The one subtype row for a table, absent when the table has no subtypes or
/// no field to key them on. Public to the crate so an extraction can ask for
/// the verdict on the thing it just wrote instead of guessing which row it was.
pub fn subtype_item(table: &Table) -> Option<Item> {
    let definition = &table.definition;
    if definition.subtypes.is_empty() {
        return None;
    }
    let field = definition.subtype_field.as_ref()?;
    let listed: Vec<String> = definition
        .subtypes
        .iter()
        .map(|subtype| format!("{} {}", subtype.code, subtype.name))
        .collect();
    let mut detail = format!(
        "{} subtype{} on {}.{}: {}",
        definition.subtypes.len(),
        plural(definition.subtypes.len()),
        table.name,
        field,
        listed.join(", ")
    );
    if let Some(default) = &definition.default_subtype {
        detail.push_str(&format!(", default {default}"));
    }

    let mut losses = Vec::new();
    if definition.default_subtype.is_some() {
        losses.push(
            "ptolemy's subtypes table has no column saying which code is the default, so a new feature gets no subtype unless something else picks one".to_string(),
        );
    }
    let assigned: Vec<String> = definition
        .subtypes
        .iter()
        .flat_map(|subtype| subtype.fields.iter())
        .filter_map(|field| field.domain.clone())
        .collect();
    if !assigned.is_empty() {
        losses.push(format!(
            "the per-subtype domain assignments name domains ({}), and ptolemy's domain_assignments holds the id of a domain row, so the domains have to be loaded first and their names swapped for ids",
            dedup(assigned).join(", ")
        ));
    }
    Some(Item::new(
        table.name.clone(),
        ItemKind::AttributeSchema,
        detail,
        verdict_for(Target::Ptolemy, losses),
    ))
}

/// ptolemy's name for the field type a domain applies to, and what is lost
/// where its three names do not reach.
pub fn domain_field_type(gdal: &str) -> (&'static str, Option<String>) {
    match gdal {
        "String" => ("string", None),
        "Integer" | "Integer64" => ("integer", None),
        "Real" => ("float", None),
        other => (
            "string",
            Some(format!(
                "the domain constrains a {other} field, and ptolemy's domains are documented as constraining a string, an integer or a float, so it was recorded as a string"
            )),
        ),
    }
}

/// ptolemy's name for a column's own type on a dataset schema, and what
/// calling it that drops.
///
/// A different question from [`domain_field_type`] and a different set of
/// names: a domain's column is documented as holding one of three, while a
/// schema field is a six-variant enum ptolemy rejects an unknown name for. Both
/// are here because both are statements about what the platform can say.
pub fn schema_field_type(gdal: &str) -> (&'static str, Option<String>) {
    match gdal {
        "String" => ("string", None),
        "Integer" | "Integer64" => ("integer", None),
        "Real" => ("float", None),
        "Date" | "Time" | "DateTime" => (
            "string",
            Some(format!(
                "a {gdal} column, and ptolemy's schema field types are string, integer, float, boolean, array and object with nothing temporal among them, so it is declared a string and only the format of the text says it is a {gdal}"
            )),
        ),
        "Binary" => (
            "string",
            Some(
                "a Binary column, and ptolemy has no field type for bytes, so the schema declares a string; the bytes themselves are a separate matter, reported with the table".to_string(),
            ),
        ),
        "IntegerList" | "Integer64List" | "RealList" | "StringList" => (
            "array",
            Some(format!(
                "a {gdal} column, and ptolemy's array field type says nothing about what is in the array, so the element type is not declared"
            )),
        ),
        other => (
            "string",
            Some(format!(
                "a {other} column, which is none of ptolemy's six schema field types, so it is declared a string"
            )),
        ),
    }
}

fn dedup(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

/// An attachment relationship and the blob table behind it go to ptolemy's
/// attachments table.
fn attachments(scan: &Scan, items: &mut Vec<Item>) {
    for relationship in scan.relationships.iter().filter(is_media) {
        let table = relationship
            .right_table
            .as_deref()
            .and_then(|name| scan.table(name));
        let mut detail = format!(
            "{}: attachments on {}",
            relationship.name,
            relationship
                .left_table
                .as_deref()
                .unwrap_or("an unnamed table")
        );
        if let Some(table) = table {
            detail.push_str(&format!(
                ", held in {} ({})",
                table.name,
                match table.features {
                    Some(count) => format!("{count} row{}", plural_u64(count)),
                    None => "row count not read".to_string(),
                }
            ));
        }
        items.push(Item::new(
            relationship
                .right_table
                .clone()
                .unwrap_or_else(|| relationship.name.clone()),
            ItemKind::EmbeddedResource,
            detail,
            Verdict::approximated(
                Target::Ptolemy,
                Losses::one(
                    "ptolemy's attachments carry the bytes, the name, the content type and the size, and the row is tied to a feature id and a branch, so the OBJECTID or GlobalID this table relates on has to be swapped for the id of the loaded feature",
                )
                .and(
                    "verne reads the table's shape and its row count, never the blobs themselves, so nothing here is a claim about what the files are",
                ),
            ),
        ));
    }
}

/// A blob table no media relationship points at. Nothing verne found may go
/// unmentioned, so it gets a row saying exactly that.
fn orphan_attachments(scan: &Scan, items: &mut Vec<Item>) {
    let related: Vec<&str> = scan
        .relationships
        .iter()
        .filter(is_media)
        .filter_map(|relationship| relationship.right_table.as_deref())
        .collect();
    for table in scan
        .tables
        .iter()
        .filter(|table| table.role == TableRole::Attachment)
        .filter(|table| !related.contains(&table.name.as_str()))
    {
        items.push(Item::new(
            table.name.clone(),
            ItemKind::EmbeddedResource,
            format!(
                "attachment table with no relationship pointing at it{}",
                match table.features {
                    Some(count) => format!(", {count} row{}", plural_u64(count)),
                    None => String::new(),
                }
            ),
            Verdict::approximated(
                Target::Ptolemy,
                Losses::one(
                    "the blobs can go to ptolemy's attachments, but nothing in the geodatabase says which class they belong to, so the link has to be guessed from the table's name",
                ),
            ),
        ));
    }
}

fn is_media(relationship: &&Relationship) -> bool {
    relationship.related_table_type.as_deref() == Some("media")
}

/// An ISO or FGDC record on a layer, which ptolemy holds as a handful of
/// catalogue fields rather than as a record.
fn layer_metadata(scan: &Scan, items: &mut Vec<Item>) {
    for table in scan.user_tables().filter(|table| table.metadata) {
        items.push(Item::new(
            table.name.clone(),
            ItemKind::Metadata,
            "ISO or FGDC metadata record".to_string(),
            Verdict::approximated(
                Target::Ptolemy,
                Losses::one(
                    "ptolemy's dataset_metadata holds a description, a source, a licence, an attribution and keywords, so what maps onto those is kept and the rest of the record, its lineage, contacts, extents, dates and the standard it follows, has nowhere to go",
                ),
            ),
        ));
    }
}

/// Catalogue items GDAL reads no definition for. Naming them is the whole of
/// what verne can do here.
fn catalog(scan: &Scan, items: &mut Vec<Item>) {
    for item in &scan.catalog {
        let named = if item.name.is_empty() {
            "unnamed".to_string()
        } else {
            item.name.clone()
        };
        if item.kind == "DERasterDataset" {
            items.push(Item::new(
                named.clone(),
                ItemKind::RasterOverlay,
                format!("{named} ({})", item.kind),
                Verdict::approximated(
                    Target::Terrano,
                    Losses::one(
                        "terrano holds it as a GeoTIFF, and verne does not open the raster, so its size, bands, nodata and georeferencing are unverified",
                    )
                    .and(
                        "the pyramids and the raster catalogue the geodatabase keeps around it are the container's own, and a GeoTIFF carries neither",
                    ),
                ),
            ));
            continue;
        }
        items.push(Item::new(
            named.clone(),
            ItemKind::DataModel,
            format!("{named} ({})", item.kind),
            Verdict::unsupported(
                "GDAL's OpenFileGDB driver reads no definition for this kind of item, so verne can name it and nothing else; what it holds cannot be judged from what the driver gives",
            ),
        ));
    }
}

/// Versioning and archiving are enterprise geodatabase features. A file
/// geodatabase cannot have them, so there is nothing to carry or lose.
fn versioning(items: &mut Vec<Item>) {
    items.push(Item::new(
        ROOT,
        ItemKind::Temporal,
        "versioning and archiving",
        Verdict::not_applicable(
            "a file geodatabase has neither: both are enterprise geodatabase features, so nothing here needs geogit's branches or ptolemy's valid time",
        ),
    ));
}

fn tables(scan: &Scan, items: &mut Vec<Item>) {
    items.extend(scan.user_tables().map(table_item));
}

/// The row for one feature class or table.
pub fn table_item(table: &Table) -> Item {
    Item::new(
        table.name.clone(),
        ItemKind::FeatureCollection,
        detail(table),
        verdict_for(Target::Ptolemy, table_losses(table)),
    )
}

fn detail(table: &Table) -> String {
    let mut detail = match (&table.geometry, table.features) {
        (Some(geometry), Some(count)) => {
            format!("{geometry}, {count} feature{}", plural_u64(count))
        }
        (Some(geometry), None) => geometry.clone(),
        (None, Some(count)) => format!("table, {count} row{}", plural_u64(count)),
        (None, None) => "table".to_string(),
    };
    detail.push_str(&format!(
        ", {} field{}",
        table.fields.len(),
        plural(table.fields.len())
    ));
    // the graphics of an annotation class get a row of their own, so this row
    // must not read as though the whole class came across
    if table.definition.drawn_feature_type().is_some() {
        detail.push_str(", graphics reported below");
    }
    // a field bound to a domain is named with it, so the row says which fields
    // the domain rows below are about
    let named: Vec<String> = table
        .fields
        .iter()
        .map(|field| match &field.domain {
            Some(domain) => format!("{} -> {domain}", field.name),
            None => field.name.clone(),
        })
        .collect();
    if !named.is_empty() {
        detail.push_str(&format!(" ({})", named.join(", ")));
    }
    detail
}

fn plural_u64(count: u64) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn table_losses(table: &Table) -> Vec<String> {
    let mut losses = Vec::new();
    let aliased: Vec<String> = table
        .fields
        .iter()
        .filter_map(|field| {
            field
                .alias
                .as_ref()
                .map(|alias| format!("{} \"{alias}\"", field.name))
        })
        .collect();
    if !aliased.is_empty() {
        losses.push(format!(
            "the field aliases ({}) reach ptolemy on the dataset schema and are stored there, and nothing in the platform displays one, so the label a reader knows the column by is kept but never shown",
            aliased.join(", ")
        ));
    }
    let retyped: Vec<String> = table
        .fields
        .iter()
        .filter(|field| schema_field_type(&field.kind).1.is_some())
        .map(|field| format!("{} ({})", field.name, field.kind))
        .collect();
    if !retyped.is_empty() {
        losses.push(format!(
            "the schema cannot name the type of {} exactly: ptolemy's field types are string, integer, float, boolean, array and object, so {} declared as the nearest of those",
            retyped.join(", "),
            if retyped.len() == 1 { "it is" } else { "they are" }
        ));
    }
    let binary: Vec<&str> = table
        .fields
        .iter()
        .filter(|field| field.kind == "Binary")
        .map(|field| field.name.as_str())
        .collect();
    if !binary.is_empty() {
        losses.push(format!(
            "the binary field{} ({}) hold{} bytes, and ptolemy's properties column is JSONB, so the bytes have to go to the attachments table and be linked back by hand",
            plural(binary.len()),
            binary.join(", "),
            if binary.len() == 1 { "s" } else { "" }
        ));
    }
    if table.geometry.is_none() {
        losses.push(
            "this is a table with no geometry; ptolemy's geometry column accepts null but a null geometry there records a deletion, so an attribute-only table needs a convention of its own".to_string(),
        );
    }
    losses
}

/// A domain goes to ptolemy's domains table: coded values as JSON, a range as
/// two double precision bounds. What is left is what those columns do not have.
fn domains(scan: &Scan, items: &mut Vec<Item>) {
    items.extend(scan.domains.iter().map(|domain| domain_item(scan, domain)));
}

/// The row for one domain. The scan is needed as well as the domain: which
/// fields are bound to it is part of both the detail and the loss.
pub fn domain_item(scan: &Scan, domain: &Domain) -> Item {
    let users = scan.domain_users(&domain.name);
    let verdict = match &domain.kind {
        DomainKind::Glob => Verdict::unsupported(
            "a glob domain constrains a field by pattern, and ptolemy's domains are a coded list or a numeric range; OpenFileGDB refuses to read one, so this came from another driver",
        ),
        _ => verdict_for(Target::Ptolemy, domain_losses(domain, &users)),
    };
    Item::new(
        ROOT,
        ItemKind::AttributeSchema,
        domain_detail(domain, &users),
        verdict,
    )
}

fn domain_detail(domain: &Domain, users: &[String]) -> String {
    let mut detail = format!("Domain \"{}\" ({})", domain.name, domain.field_type);
    match &domain.kind {
        DomainKind::Coded(values) if values.is_empty() => {
            // a coded domain that constrains a field to nothing is a real thing
            // to find in a geodatabase, and "coded: " with an empty list after
            // it reads as verne having failed to print rather than as the file
            // saying so
            detail.push_str(", coded, no values");
        }
        DomainKind::Coded(values) => {
            let listed: Vec<String> = values
                .iter()
                .map(|(code, label)| format!("{code}={label}"))
                .collect();
            detail.push_str(&format!(", coded: {}", listed.join(", ")));
        }
        DomainKind::Range { min, max } => {
            let end = |bound: &Option<crate::glue::Bound>| match bound {
                Some(bound) if bound.inclusive => format!("{}", bound.value),
                Some(bound) => format!("{} exclusive", bound.value),
                None => "open".to_string(),
            };
            detail.push_str(&format!(", range: {} to {}", end(min), end(max)));
        }
        DomainKind::Glob => detail.push_str(", glob"),
    }
    if !users.is_empty() {
        detail.push_str(&format!(", used by {}", users.join(", ")));
    }
    detail
}

fn domain_losses(domain: &Domain, users: &[String]) -> Vec<String> {
    let mut losses = Vec::new();
    if !users.is_empty() {
        losses.push(format!(
            "{} {} bound to this domain, and ptolemy binds a domain to a field only through a subtype's domain_assignments, so a binding outside a subtype has nowhere to go but the free-form JSON in dataset_schemas, which nothing reads",
            users.join(" and "),
            if users.len() == 1 { "is" } else { "are" }
        ));
    }
    if let Some(description) = &domain.description {
        losses.push(format!(
            "the description (\"{description}\") has a column in ptolemy's domains table, but the create route does not take one, so it cannot be written through the API"
        ));
    }
    for (policy, what) in [
        (domain.split_policy, "split"),
        (domain.merge_policy, "merge"),
    ] {
        if policy != "default value" {
            losses.push(format!(
                "the {what} policy ({policy}) says what happens to the value when a feature is {}, and ptolemy's domains table has no column for it",
                if what == "split" { "split" } else { "merged" }
            ));
        }
    }
    if let DomainKind::Range { min, max } = &domain.kind {
        let exclusive = [min, max]
            .iter()
            .filter_map(|bound| bound.as_ref())
            .any(|bound| !bound.inclusive);
        if exclusive {
            losses.push(
                "an end of the range is excluded, and ptolemy keeps range_min and range_max with no flag for whether an end is part of the range".to_string(),
            );
        }
        if domain.field_type == "Integer64" {
            losses.push(
                "ptolemy keeps the bounds as double precision, so a 64-bit bound past 2^53 does not come back exact".to_string(),
            );
        }
    }
    losses
}

/// A relationship class goes to ptolemy's relationship_classes. The table has
/// more columns than the create route fills, so some of this stops at the API.
fn relationships(scan: &Scan, items: &mut Vec<Item>) {
    // an attachment relationship is reported with its blob table instead
    items.extend(
        scan.relationships
            .iter()
            .filter(|held| !is_media(held))
            .map(relationship_item),
    );
}

/// The row for one relationship class. Only for a class that is not an
/// attachment: a media class is reported with its blob table.
pub fn relationship_item(relationship: &Relationship) -> Item {
    Item::new(
        ROOT,
        ItemKind::Relationship,
        relationship_detail(relationship),
        verdict_for(Target::Ptolemy, relationship_losses(relationship)),
    )
}

fn relationship_detail(relationship: &Relationship) -> String {
    let side = |table: &Option<String>, fields: &[String]| match (table, fields.first()) {
        (Some(table), Some(field)) => format!("{table}.{field}"),
        (Some(table), None) => table.clone(),
        (None, _) => "unnamed".to_string(),
    };
    let mut detail = format!(
        "{}: {} -> {}, {}, {}",
        relationship.name,
        side(&relationship.left_table, &relationship.left_fields),
        side(&relationship.right_table, &relationship.right_fields),
        relationship.cardinality,
        relationship.kind,
    );
    if let Some(mapping) = &relationship.mapping_table {
        detail.push_str(&format!(", through {mapping}"));
    }
    let labels: Vec<&str> = [&relationship.forward_label, &relationship.backward_label]
        .iter()
        .filter_map(|label| label.as_deref())
        .collect();
    if !labels.is_empty() {
        detail.push_str(&format!(", labels \"{}\"", labels.join("\" / \"")));
    }
    detail
}

fn relationship_losses(relationship: &Relationship) -> Vec<String> {
    let mut losses = vec![
        format!(
            "the class keys the origin on {}, and ptolemy's create route takes only the destination key: origin_primary_key stays at its default of 'id', so the origin key cannot be said through the API",
            relationship
                .left_fields
                .first()
                .map(String::as_str)
                .unwrap_or("its own key field")
        ),
        "GDAL's relationship model has no rules, key type or notification direction, so verne cannot say whether this class carries any of them".to_string(),
    ];
    if relationship.kind == "composite" {
        losses.push(
            "this is a composite class, where deleting the origin deletes what hangs off it; ptolemy's relationship_classes has an is_composite column but no route sets it, so the cascade stops at the API".to_string(),
        );
    }
    if relationship.cardinality == "many to many" {
        losses.push(format!(
            "a many-to-many class relates through {}, and ptolemy relates through relationship_records keyed by feature id, so any attribute on the mapping table has nowhere to go",
            relationship
                .mapping_table
                .as_deref()
                .unwrap_or("an intermediate table")
        ));
    }
    if relationship.cardinality == "many to one" {
        losses.push(
            "ptolemy's cardinality column is documented as one_to_one, one_to_many or many_to_many, so a many-to-one class has to be turned round and stored the other way about".to_string(),
        );
    }
    losses
}

fn system_tables(scan: &Scan, items: &mut Vec<Item>) {
    let system: Vec<&str> = scan
        .tables
        .iter()
        .filter(|table| table.role == TableRole::System)
        .map(|table| table.name.as_str())
        .collect();
    if system.is_empty() {
        return;
    }
    items.push(Item::new(
        "geodatabase root",
        ItemKind::Metadata,
        format!(
            "{} system table{}: {}",
            system.len(),
            plural(system.len()),
            system.join(", ")
        ),
        Verdict::unsupported(
            "the geodatabase's own catalogue of items, relationships and spatial references; it describes the container rather than the data, and GeoLang keeps its own catalogue",
        ),
    ));
}

#[cfg(test)]
mod tests {
    use super::domain_detail;
    use crate::glue::{Domain, DomainKind};

    fn domain(kind: DomainKind) -> Domain {
        Domain {
            name: "Area Event Type".to_string(),
            description: None,
            field_type: "Integer".to_string(),
            kind,
            split_policy: "default value",
            merge_policy: "default value",
        }
    }

    /// A real geodatabase holds coded domains with nothing in them, and the
    /// report has to say that rather than trail off after "coded:".
    #[test]
    fn a_coded_domain_with_no_values_says_so() {
        let detail = domain_detail(&domain(DomainKind::Coded(Vec::new())), &[]);

        assert!(detail.contains("coded, no values"), "{detail}");
        assert!(!detail.ends_with("coded: "), "{detail}");
    }

    #[test]
    fn a_coded_domain_still_lists_the_values_it_has() {
        let values = vec![("53700".to_string(), "Area of Complex Channels".to_string())];
        let detail = domain_detail(&domain(DomainKind::Coded(values)), &[]);

        assert!(
            detail.contains("coded: 53700=Area of Complex Channels"),
            "{detail}"
        );
    }
}
