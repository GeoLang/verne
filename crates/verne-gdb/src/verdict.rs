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
    tables(scan, &mut items);
    domains(scan, &mut items);
    relationships(scan, &mut items);
    system_tables(scan, &mut items);
    items
}

fn tables(scan: &Scan, items: &mut Vec<Item>) {
    for table in scan.user_tables() {
        items.push(Item::new(
            table.name.clone(),
            ItemKind::FeatureCollection,
            detail(table),
            verdict_for(Target::Ptolemy, table_losses(table)),
        ));
    }
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
            "the field aliases ({}) are human labels; ptolemy's dataset_schemas takes free-form JSON so they can be carried, but nothing reads them",
            aliased.join(", ")
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
    for domain in &scan.domains {
        let users = scan.domain_users(&domain.name);
        let verdict = match &domain.kind {
            DomainKind::Glob => Verdict::unsupported(
                "a glob domain constrains a field by pattern, and ptolemy's domains are a coded list or a numeric range; OpenFileGDB refuses to read one, so this came from another driver",
            ),
            _ => verdict_for(Target::Ptolemy, domain_losses(domain, &users)),
        };
        items.push(Item::new(
            ROOT,
            ItemKind::AttributeSchema,
            domain_detail(domain, &users),
            verdict,
        ));
    }
}

fn domain_detail(domain: &Domain, users: &[String]) -> String {
    let mut detail = format!("Domain \"{}\" ({})", domain.name, domain.field_type);
    match &domain.kind {
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
    for relationship in &scan.relationships {
        items.push(Item::new(
            ROOT,
            ItemKind::Relationship,
            relationship_detail(relationship),
            verdict_for(Target::Ptolemy, relationship_losses(relationship)),
        ));
    }
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
