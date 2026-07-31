//! Turning a fetched service into verdicts. Every judgement about what
//! GeoLang can hold is in this file, so it can be argued with in one place.
//! The claims about ptolemy's API mirror the gdb adapter's, which were checked
//! against the real routes; what differs here is only what the REST resources
//! expose and withhold.

use verne_core::{Item, ItemKind, Losses, Target, Verdict};

use crate::service::{Domain, DomainKind, Field, Layer, RelationshipEnd, Service};

/// Where anything that belongs to the service rather than to one layer is
/// reported.
const ROOT: &str = "service root";

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn plural_u64(count: u64) -> &'static str {
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

pub fn items(service: &Service) -> Vec<Item> {
    let mut items = Vec::new();
    for layer in &service.layers {
        items.push(layer_item(layer));
        if let Some(item) = subtype_item(layer) {
            items.push(item);
        }
        items.extend(domain_items(layer));
        if layer.has_attachments {
            items.push(attachment_item(layer));
        }
        if let Some(item) = renderer_item(layer) {
            items.push(item);
        }
        if layer.time_aware {
            items.push(time_item(layer));
        }
        if layer.has_metadata {
            items.push(metadata_item(layer));
        }
    }
    items.extend(relationship_items(service));
    items.push(versioning_item(service));
    items
}

// ─── Layers ─────────────────────────────────────────────────────────

/// The row for one layer or table.
pub fn layer_item(layer: &Layer) -> Item {
    Item::new(
        layer.name.clone(),
        ItemKind::FeatureCollection,
        layer_detail(layer),
        verdict_for(Target::Ptolemy, layer_losses(layer)),
    )
}

/// "Polygon" out of "esriGeometryPolygon", which is how an operator would say
/// it; the raw name is one prefix away.
fn geometry_word(kind: &str) -> &str {
    kind.strip_prefix("esriGeometry").unwrap_or(kind)
}

fn layer_detail(layer: &Layer) -> String {
    let mut detail = match (&layer.geometry_type, layer.count) {
        (Some(kind), Some(count)) => format!(
            "{}, {count} feature{}",
            geometry_word(kind),
            plural_u64(count)
        ),
        (Some(kind), None) => geometry_word(kind).to_string(),
        (None, Some(count)) => format!("table, {count} row{}", plural_u64(count)),
        (None, None) => "table".to_string(),
    };
    detail.push_str(&format!(
        ", {} field{}",
        layer.fields.len(),
        plural(layer.fields.len())
    ));
    let named: Vec<String> = layer
        .fields
        .iter()
        .map(|field| match &field.domain {
            Some(domain) => format!("{} -> {}", field.name, domain.name),
            None => field.name.clone(),
        })
        .collect();
    if !named.is_empty() {
        detail.push_str(&format!(" ({})", named.join(", ")));
    }
    detail
}

fn layer_losses(layer: &Layer) -> Vec<String> {
    let mut losses = Vec::new();
    let aliased: Vec<String> = layer
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
    let retyped: Vec<String> = layer
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
    if layer.geometry_type.is_some() {
        losses.extend(reprojection_losses(layer));
    } else {
        losses.push(
            "this is a table with no geometry; ptolemy's insert takes one and its column accepts a null, but a null geometry there is how a deleted version reads, so every row is committed with an empty geometry collection in place of no geometry at all".to_string(),
        );
    }
    losses
}

/// What getting a layer's geometry into the only spatial reference ptolemy
/// stores costs it. The service does the transforming: every query asks for
/// EPSG:4326 in `outSR` and verne does no coordinate arithmetic. The
/// coordinates as stored are fetched in a second pass, paired by object id,
/// and ride on each insert, where the reference can be declared at all.
fn reprojection_losses(layer: &Layer) -> Vec<String> {
    match (layer.wkid, &layer.crs_wkt) {
        (Some(4326), _) => Vec::new(),
        (Some(code), _) if code < 33000 => vec![format!(
            "ptolemy serves geometry as EPSG:4326, so every query asks the service to answer in it and the service transforms this layer out of EPSG:{code} itself; Esri does not document which datum transformation that picks and verne cannot ask for one. the coordinates as the service stores them are fetched in a second pass and ride on each insert as EPSG:{code}, stored beside the working copy and read back exactly by feature"
        )],
        (Some(code), Some(_)) => vec![format!(
            "ptolemy serves geometry as EPSG:4326, so every query asks the service to answer in it and the service transforms this layer out of wkid {code} itself; {code} is Esri's own authority rather than an EPSG code, so the coordinates as stored are fetched in a second pass and ride on each insert with the reference's WKT definition, stored beside the working copy"
        )],
        (Some(code), None) => vec![format!(
            "ptolemy serves geometry as EPSG:4326, so every query asks the service to answer in it and the service transforms this layer out of wkid {code} itself; {code} is Esri's own authority rather than an EPSG code and the layer states no WKT for it, so the original cannot be declared to ptolemy, is not fetched, and lives on only in the service"
        )],
        (None, _) => vec![
            "the layer states no spatial reference verne can read; every query still asks for EPSG:4326 and the service's own knowledge of its source reference decides what that means, so verne cannot say what the coordinates were transformed out of".to_string(),
        ],
    }
}

// ─── Fields ─────────────────────────────────────────────────────────

/// ptolemy's name for a column's own type on a dataset schema, and what
/// calling it that drops. The same six names the gdb adapter maps onto,
/// keyed by the REST API's `esriFieldType*` enumeration.
pub fn schema_field_type(esri: &str) -> (&'static str, Option<String>) {
    match esri {
        "esriFieldTypeString" | "esriFieldTypeGUID" | "esriFieldTypeGlobalID" => ("string", None),
        "esriFieldTypeOID"
        | "esriFieldTypeSmallInteger"
        | "esriFieldTypeInteger"
        | "esriFieldTypeBigInteger" => ("integer", None),
        "esriFieldTypeSingle" | "esriFieldTypeDouble" => ("float", None),
        "esriFieldTypeDate" => (
            "string",
            Some(
                "a Date column, and ptolemy's schema field types are string, integer, float, boolean, array and object with nothing temporal among them, so it is declared a string and each value is rewritten from the service's epoch milliseconds into RFC 3339 text".to_string(),
            ),
        ),
        "esriFieldTypeDateOnly" | "esriFieldTypeTimeOnly" | "esriFieldTypeTimestampOffset" => (
            "string",
            Some(format!(
                "a {} column, and ptolemy has nothing temporal among its field types, so it is declared a string and only the format of the text says what it is",
                esri.strip_prefix("esriFieldType").unwrap_or(esri)
            )),
        ),
        "esriFieldTypeBlob" | "esriFieldTypeRaster" => (
            "string",
            Some(format!(
                "a {} column, and ptolemy has no field type for bytes, so the schema declares a string and no value is written into it; bytes reach ptolemy only as attachments",
                esri.strip_prefix("esriFieldType").unwrap_or(esri)
            )),
        ),
        "esriFieldTypeXML" => (
            "string",
            Some(
                "an XML column, which ptolemy has no narrower type for, so it is declared a string".to_string(),
            ),
        ),
        other => (
            "string",
            Some(format!(
                "a {other} column, which is none of ptolemy's six schema field types, so it is declared a string"
            )),
        ),
    }
}

/// ptolemy's name for the field type a domain applies to. A domain rides on a
/// field, so the field's own type is what answers this.
pub fn domain_field_type(esri: &str) -> (&'static str, Option<String>) {
    match esri {
        "esriFieldTypeString" => ("string", None),
        "esriFieldTypeSmallInteger" | "esriFieldTypeInteger" | "esriFieldTypeBigInteger" => {
            ("integer", None)
        }
        "esriFieldTypeSingle" | "esriFieldTypeDouble" => ("float", None),
        other => (
            "string",
            Some(format!(
                "the domain constrains a {other} field, and ptolemy's domains are documented as constraining a string, an integer or a float, so it was recorded as a string"
            )),
        ),
    }
}

/// Whether a field's values are ever read into a feature's properties. The
/// geometry column is the shape itself, and bytes have nowhere to go.
pub fn carries_values(field: &Field) -> bool {
    !matches!(
        field.kind.as_str(),
        "esriFieldTypeGeometry" | "esriFieldTypeBlob" | "esriFieldTypeRaster"
    )
}

// ─── Domains ────────────────────────────────────────────────────────

/// Every distinct domain a layer uses, through a field or a subtype
/// assignment, with the fields that use it.
pub fn layer_domains(layer: &Layer) -> Vec<(Domain, Vec<String>)> {
    let mut found: Vec<(Domain, Vec<String>)> = Vec::new();
    let mut note = |domain: &Domain, user: String| match found
        .iter_mut()
        .find(|(held, _)| held.name == domain.name)
    {
        Some((_, users)) => users.push(user),
        None => found.push((domain.clone(), vec![user])),
    };
    for field in &layer.fields {
        if let Some(domain) = &field.domain {
            note(domain, format!("{}.{}", layer.name, field.name));
        }
    }
    for subtype in &layer.subtypes {
        for (field, domain) in &subtype.domains {
            note(
                domain,
                format!("{}.{} under subtype {}", layer.name, field, subtype.name),
            );
        }
    }
    found
}

fn domain_items(layer: &Layer) -> Vec<Item> {
    layer_domains(layer)
        .into_iter()
        .map(|(domain, users)| domain_item(layer, &domain, &users))
        .collect()
}

/// The row for one domain on one layer. A hosted domain rides on the layer's
/// fields rather than on a workspace, so a domain two layers share is a row on
/// each, which is also how ptolemy will hold it.
pub fn domain_item(layer: &Layer, domain: &Domain, users: &[String]) -> Item {
    Item::new(
        layer.name.clone(),
        ItemKind::AttributeSchema,
        domain_detail(domain, users),
        verdict_for(Target::Ptolemy, domain_losses(users)),
    )
}

fn domain_detail(domain: &Domain, users: &[String]) -> String {
    let mut detail = format!("Domain \"{}\"", domain.name);
    match &domain.kind {
        DomainKind::Coded(values) if values.is_empty() => detail.push_str(", coded, no values"),
        DomainKind::Coded(values) => {
            let listed: Vec<String> = values
                .iter()
                .map(|(code, label)| format!("{code}={label}"))
                .collect();
            detail.push_str(&format!(", coded: {}", listed.join(", ")));
        }
        DomainKind::Range { min, max } => {
            let end = |bound: &Option<f64>| match bound {
                Some(value) => value.to_string(),
                None => "open".to_string(),
            };
            detail.push_str(&format!(", range: {} to {}", end(min), end(max)));
        }
    }
    if !users.is_empty() {
        detail.push_str(&format!(", used by {}", users.join(", ")));
    }
    detail
}

fn domain_losses(users: &[String]) -> Vec<String> {
    let bound: Vec<&str> = users
        .iter()
        .filter(|user| !user.contains(" under subtype "))
        .map(String::as_str)
        .collect();
    if bound.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "{} {} bound to this domain, and ptolemy binds a domain to a field only through a subtype's domain_assignments, so a binding outside a subtype has nowhere to go but the free-form JSON in dataset_schemas, which nothing reads",
        bound.join(" and "),
        if bound.len() == 1 { "is" } else { "are" }
    )]
}

// ─── Subtypes ───────────────────────────────────────────────────────

/// The one subtype row for a layer, absent when it has none.
pub fn subtype_item(layer: &Layer) -> Option<Item> {
    if layer.subtypes.is_empty() {
        return None;
    }
    let field = layer.subtype_field.as_ref()?;
    let listed: Vec<String> = layer
        .subtypes
        .iter()
        .map(|subtype| format!("{} {}", crate::service::text(&subtype.code), subtype.name))
        .collect();
    let mut detail = format!(
        "{} subtype{} on {}.{}: {}",
        layer.subtypes.len(),
        plural(layer.subtypes.len()),
        layer.name,
        field,
        listed.join(", ")
    );
    if let Some(default) = &layer.default_subtype_code {
        detail.push_str(&format!(", default {}", crate::service::text(default)));
    }

    let mut losses = Vec::new();
    if layer.default_subtype_code.is_some() {
        losses.push(
            "ptolemy's subtypes table has no column saying which code is the default, so a new feature gets no subtype unless something else picks one".to_string(),
        );
    }
    let assigned: Vec<String> = layer
        .subtypes
        .iter()
        .flat_map(|subtype| subtype.domains.iter())
        .map(|(_, domain)| domain.name.clone())
        .collect();
    if !assigned.is_empty() {
        losses.push(format!(
            "the per-subtype domain assignments name domains ({}), and ptolemy's domain_assignments holds the id of a domain row, so the domains have to be loaded first and their names swapped for ids",
            dedup(assigned).join(", ")
        ));
    }
    Some(Item::new(
        layer.name.clone(),
        ItemKind::AttributeSchema,
        detail,
        verdict_for(Target::Ptolemy, losses),
    ))
}

fn dedup(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

// ─── Relationships ──────────────────────────────────────────────────

/// A relationship as both of its ends tell it: the origin layer's entry and
/// the destination layer's, paired by id.
pub struct Pairing<'a> {
    pub origin_layer: &'a Layer,
    pub origin: &'a RelationshipEnd,
    /// Absent when the related table is not among the layers verne was
    /// pointed at: a table outside the service, which the REST model allows,
    /// or outside the one layer the operator's URL scoped verne to. ptolemy
    /// can hold neither.
    pub destination: Option<(&'a Layer, &'a RelationshipEnd)>,
}

/// Every relationship in the service, told once from its origin end.
pub fn pairings(service: &Service) -> Vec<Pairing<'_>> {
    let mut found = Vec::new();
    for layer in &service.layers {
        for end in &layer.relationships {
            if end.role != "esriRelRoleOrigin" {
                continue;
            }
            let destination = service.layer(end.related_table_id).and_then(|related| {
                related
                    .relationships
                    .iter()
                    .find(|held| held.id == end.id && held.role == "esriRelRoleDestination")
                    .map(|held| (related, held))
            });
            found.push(Pairing {
                origin_layer: layer,
                origin: end,
                destination,
            });
        }
    }
    found
}

fn relationship_items(service: &Service) -> Vec<Item> {
    pairings(service)
        .iter()
        .map(|pairing| relationship_item(pairing))
        .collect()
}

/// The row for one relationship.
pub fn relationship_item(pairing: &Pairing<'_>) -> Item {
    Item::new(
        ROOT,
        ItemKind::Relationship,
        relationship_detail(pairing),
        verdict_for(Target::Ptolemy, relationship_losses(pairing)),
    )
}

/// "one to many" out of "esriRelCardinalityOneToMany".
fn cardinality_words(esri: &str) -> &'static str {
    match esri {
        "esriRelCardinalityOneToOne" => "one to one",
        "esriRelCardinalityOneToMany" => "one to many",
        "esriRelCardinalityManyToMany" => "many to many",
        _ => "unstated cardinality",
    }
}

fn relationship_detail(pairing: &Pairing<'_>) -> String {
    let origin = pairing.origin;
    let side = |layer: &Layer, key: &Option<String>| match key {
        Some(field) => format!("{}.{}", layer.name, field),
        None => layer.name.clone(),
    };
    let destination = match &pairing.destination {
        Some((layer, end)) => side(layer, &end.key_field),
        None => format!(
            "table id {}, not among the layers verne was pointed at",
            origin.related_table_id
        ),
    };
    let mut detail = format!(
        "{}: {} -> {}, {}, {}",
        origin.name,
        side(pairing.origin_layer, &origin.key_field),
        destination,
        cardinality_words(&origin.cardinality),
        if origin.composite {
            "composite"
        } else {
            "simple"
        },
    );
    if origin.relationship_table_id.is_some() {
        detail.push_str(", through a mapping table");
    }
    detail
}

fn relationship_losses(pairing: &Pairing<'_>) -> Vec<String> {
    let origin = pairing.origin;
    let mut losses = vec![
        format!(
            "the class keys the origin on {}, and ptolemy's create route takes only the destination key: origin_primary_key stays at its default of 'id', so the origin key cannot be said through the API",
            origin
                .key_field
                .as_deref()
                .unwrap_or("its own key field")
        ),
        "the layer's relationship description carries no forward or backward label and no rules, so ptolemy's labels are created empty and verne cannot say whether the class carries rules at all".to_string(),
    ];
    if pairing.destination.is_none() {
        losses.push(format!(
            "the other side is table id {}, which is not among the layers verne was pointed at, and a relationship class in ptolemy names two dataset ids, so the class cannot be created",
            origin.related_table_id
        ));
    }
    if origin.composite {
        losses.push(
            "this is a composite class, where deleting the origin deletes what hangs off it; ptolemy's relationship_classes has an is_composite column but no route sets it, so the cascade stops at the API".to_string(),
        );
    }
    if origin.cardinality == "esriRelCardinalityManyToMany" {
        losses.push(
            "a many-to-many class relates through a mapping table, and ptolemy relates through relationship_records keyed by feature id, so any attribute on the mapping table has nowhere to go".to_string(),
        );
    }
    losses
}

// ─── Attachments ────────────────────────────────────────────────────

/// The row for one layer's attachments.
pub fn attachment_item(layer: &Layer) -> Item {
    Item::new(
        layer.name.clone(),
        ItemKind::EmbeddedResource,
        format!("attachments on {}", layer.name),
        verdict_for(
            Target::Ptolemy,
            vec![
                "an attachment in ptolemy hangs off a feature on one branch, so it reaches the branch the load created and a second branch of the same dataset shows the feature without it".to_string(),
                "ptolemy takes the bytes as base64 inside a JSON request body, and its request limit is 2 MB, so a blob much over 1.5 MB is refused rather than stored".to_string(),
                "the service's attachment record can carry keywords and camera EXIF beside the bytes; ptolemy's attachment has no field for either, so they are written into its free-form metadata JSON, which nothing in the platform reads".to_string(),
            ],
        ),
    )
}

// ─── The rest of the layer ──────────────────────────────────────────

/// A renderer is how the layer is drawn, and jung is where drawing lives.
/// verne reads the renderer's type and nothing below it, so the row says the
/// symbols stay behind.
fn renderer_item(layer: &Layer) -> Option<Item> {
    let kind = layer.renderer.as_deref()?;
    Some(Item::new(
        layer.name.clone(),
        ItemKind::Styling,
        format!("{kind} renderer"),
        Verdict::approximated(
            Target::Jung,
            Losses::one(
                "verne reads the renderer's type and nothing below it, so the symbols, colours, class breaks and label classes stay on the service and recreating them in jung is by hand",
            ),
        ),
    ))
}

fn time_item(layer: &Layer) -> Item {
    Item::new(
        layer.name.clone(),
        ItemKind::Temporal,
        "time-aware layer".to_string(),
        Verdict::unsupported(
            "the layer declares time settings over its fields; verne carries those fields as ordinary columns and does not map the settings onto anything, so the dataset ptolemy gets is not time-aware",
        ),
    )
}

fn metadata_item(layer: &Layer) -> Item {
    Item::new(
        layer.name.clone(),
        ItemKind::Metadata,
        "metadata record".to_string(),
        Verdict::approximated(
            Target::Ptolemy,
            Losses::one(
                "ptolemy's dataset_metadata holds a description, a source, a licence, an attribution and keywords, so what maps onto those is kept and the rest of the record, its lineage, contacts, extents, dates and the standard it follows, has nowhere to go",
            ),
        ),
    )
}

/// Versioning is a fact about the geodatabase behind the service. A hosted
/// layer has none; an enterprise service can, and verne reads only the
/// version the service answers with.
fn versioning_item(service: &Service) -> Item {
    let verdict = if service.versioned {
        Verdict::unsupported(
            "the service fronts versioned data and verne queries only the default version it answers with; the version tree, its edits and its conflicts are not read, so nothing here reaches geogit's branches",
        )
    } else {
        Verdict::not_applicable(
            "the service reports no versioned data, so there is no version tree to carry",
        )
    };
    Item::new(
        ROOT,
        ItemKind::Temporal,
        "versioning and archiving",
        verdict,
    )
}
