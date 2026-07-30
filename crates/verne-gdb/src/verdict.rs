//! Turning the scan into verdicts. Every judgement about what GeoLang can hold
//! is in this file, so it can be argued with in one place.

use verne_core::{Item, ItemKind, Losses, Target, Verdict};

use crate::scan::{Scan, Table, TableRole};

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
