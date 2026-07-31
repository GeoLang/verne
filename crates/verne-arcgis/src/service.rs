//! What a feature service says about itself, read once and kept.
//!
//! The raw structs mirror the JSON the REST API documents and stay private;
//! the public model is what the verdicts and the extraction reason over. Esri's
//! docs leave a few shapes loose, so the parsing is deliberately forgiving
//! where they are: a range domain's bounds have been seen as `range: [min,
//! max]` and as `min`/`max`, and a coded value's code is a number on an
//! integer domain and a string on a string one.

use serde::Deserialize;

/// A feature or map service, its layers and tables already fetched.
#[derive(Debug)]
pub struct Service {
    /// The FeatureServer or MapServer root, no trailing slash.
    pub url: String,
    /// What an operator would call the service: "ArcGIS Feature Service" or
    /// "ArcGIS Map Service", decided by the root the URL named.
    pub format: &'static str,
    /// The one layer the operator's URL scoped verne to, when it named one.
    /// `layers` then holds it and nothing else.
    pub scope: Option<i64>,
    pub description: Option<String>,
    /// `hasVersionedData`: only an enterprise geodatabase behind the service
    /// can make this true.
    pub versioned: bool,
    pub layers: Vec<Layer>,
}

impl Service {
    pub fn layer(&self, id: i64) -> Option<&Layer> {
        self.layers.iter().find(|layer| layer.id == id)
    }
}

impl Layer {
    /// Whether the layer holds rows verne can query for. A group layer is
    /// structure and a raster layer is a picture: neither answers `/query`
    /// with features.
    pub fn queryable(&self) -> bool {
        !matches!(
            self.kind.as_deref(),
            Some("Group Layer") | Some("Raster Layer")
        )
    }
}

/// One layer or table of the service, from `{root}/{id}?f=json`.
#[derive(Debug)]
pub struct Layer {
    pub id: i64,
    pub name: String,
    /// The layer's `type`: `Feature Layer`, `Table`, `Group Layer`, `Raster
    /// Layer` and so on. A FeatureServer usually omits it on tables; a
    /// MapServer states it on everything, and it is what says a layer has no
    /// features to ask for.
    pub kind: Option<String>,
    /// The group layer this one sits under, and the members when this is
    /// itself a group: a MapServer's layer tree, flat on a FeatureServer.
    pub parent_layer: Option<String>,
    pub sub_layers: Vec<String>,
    /// `isDataVersioned`: the data behind the layer is versioned in the
    /// enterprise geodatabase serving it.
    pub versioned: bool,
    /// `esriGeometry*`, absent on a table.
    pub geometry_type: Option<String>,
    pub has_z: bool,
    pub has_m: bool,
    /// The reference the layer answers in when no `outSR` is asked for:
    /// `latestWkid` where the service states one, else `wkid`.
    pub wkid: Option<i32>,
    /// The same reference as WKT, which is how a custom or compound one is
    /// stated when no code names it.
    pub crs_wkt: Option<String>,
    pub object_id_field: Option<String>,
    pub max_record_count: Option<u64>,
    pub supports_pagination: bool,
    pub supports_query_attachments: bool,
    pub has_attachments: bool,
    pub has_metadata: bool,
    /// The renderer's `type`, when the layer carries drawing info.
    pub renderer: Option<String>,
    /// Whether the layer declares time awareness.
    pub time_aware: bool,
    pub fields: Vec<Field>,
    pub subtype_field: Option<String>,
    pub default_subtype_code: Option<serde_json::Value>,
    pub subtypes: Vec<Subtype>,
    pub relationships: Vec<RelationshipEnd>,
    /// Filled by a count query, not by the layer resource.
    pub count: Option<u64>,
}

#[derive(Debug)]
pub struct Field {
    pub name: String,
    /// The label the layer's users read the column by, absent when it is the
    /// name again.
    pub alias: Option<String>,
    /// `esriFieldType*`, as the service names it.
    pub kind: String,
    pub nullable: bool,
    pub domain: Option<Domain>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Domain {
    pub name: String,
    pub kind: DomainKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DomainKind {
    /// Code and label pairs. A code arrives as a JSON number on an integer
    /// domain and a string on a string one; both are kept as text, which is
    /// how the sidecar writes coded values.
    Coded(Vec<(String, String)>),
    Range {
        min: Option<f64>,
        max: Option<f64>,
    },
}

#[derive(Debug)]
pub struct Subtype {
    pub code: serde_json::Value,
    pub name: String,
    pub default_values: serde_json::Map<String, serde_json::Value>,
    /// Field to the domain that applies under this subtype. An `inherited`
    /// entry means the field keeps its own domain and is not listed here.
    pub domains: Vec<(String, Domain)>,
}

/// One layer's half of a relationship. The other half is the entry with the
/// same id on the related layer, and pairing the two is the reader's job.
#[derive(Debug)]
pub struct RelationshipEnd {
    pub id: i64,
    pub name: String,
    pub related_table_id: i64,
    /// `esriRelCardinality*`.
    pub cardinality: String,
    /// `esriRelRoleOrigin` or `esriRelRoleDestination`.
    pub role: String,
    pub key_field: Option<String>,
    pub composite: bool,
    /// The mapping table of an attributed or many-to-many class.
    pub relationship_table_id: Option<i64>,
}

// ─── Raw JSON ───────────────────────────────────────────────────────

/// The API writes `"fields": null` where it means none: a group layer's field
/// list is an explicit null, not an absent key, and serde's `default` only
/// covers absence.
fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + serde::Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawService {
    #[serde(default)]
    service_description: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    has_versioned_data: bool,
    #[serde(default)]
    layers: Vec<RawListed>,
    #[serde(default)]
    tables: Vec<RawListed>,
}

#[derive(Deserialize)]
struct RawListed {
    id: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLayer {
    id: i64,
    name: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    parent_layer: Option<RawNamed>,
    #[serde(default, deserialize_with = "null_default")]
    sub_layers: Vec<RawNamed>,
    #[serde(default)]
    is_data_versioned: bool,
    #[serde(default)]
    geometry_type: Option<String>,
    #[serde(default)]
    has_z: bool,
    #[serde(default)]
    has_m: bool,
    #[serde(default)]
    extent: Option<RawExtent>,
    #[serde(default)]
    object_id_field: Option<String>,
    #[serde(default)]
    max_record_count: Option<u64>,
    #[serde(default)]
    advanced_query_capabilities: Option<RawQueryCapabilities>,
    #[serde(default)]
    has_attachments: bool,
    #[serde(default)]
    has_metadata: bool,
    #[serde(default)]
    drawing_info: Option<RawDrawingInfo>,
    #[serde(default)]
    time_info: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "null_default")]
    fields: Vec<RawField>,
    #[serde(default)]
    subtype_field: Option<String>,
    #[serde(default)]
    default_subtype_code: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "null_default")]
    subtypes: Vec<RawSubtype>,
    #[serde(default, deserialize_with = "null_default")]
    relationships: Vec<RawRelationship>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawExtent {
    #[serde(default)]
    spatial_reference: Option<RawSpatialReference>,
}

/// A `{id, name}` pair, which is how a layer names its parent and members.
#[derive(Deserialize)]
struct RawNamed {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSpatialReference {
    #[serde(default)]
    wkid: Option<i32>,
    #[serde(default)]
    latest_wkid: Option<i32>,
    #[serde(default)]
    wkt: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawQueryCapabilities {
    #[serde(default)]
    supports_pagination: bool,
    #[serde(default)]
    supports_query_attachments: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDrawingInfo {
    #[serde(default)]
    renderer: Option<RawRenderer>,
}

#[derive(Deserialize)]
struct RawRenderer {
    #[serde(default, rename = "type")]
    kind: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawField {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default = "nullable_default")]
    nullable: bool,
    #[serde(default)]
    domain: Option<RawDomain>,
}

/// A field that says nothing about nullability is taken as nullable: the
/// safer schema is the one that demands less.
fn nullable_default() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDomain {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default, deserialize_with = "null_default")]
    coded_values: Vec<RawCodedValue>,
    /// The documented shape is `range: [min, max]`; `min`/`max` have been
    /// seen in renderings of the same page, so both are read.
    #[serde(default, deserialize_with = "null_default")]
    range: Vec<serde_json::Value>,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
}

#[derive(Deserialize)]
struct RawCodedValue {
    #[serde(default)]
    name: Option<String>,
    code: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSubtype {
    code: serde_json::Value,
    name: String,
    #[serde(default, deserialize_with = "null_default")]
    default_values: serde_json::Map<String, serde_json::Value>,
    #[serde(default, deserialize_with = "null_default")]
    domains: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRelationship {
    id: i64,
    name: String,
    related_table_id: i64,
    #[serde(default)]
    cardinality: String,
    #[serde(default)]
    role: String,
    #[serde(default)]
    key_field: Option<String>,
    #[serde(default)]
    composite: bool,
    #[serde(default)]
    relationship_table_id: Option<i64>,
}

// ─── Parsing ────────────────────────────────────────────────────────

/// The ids the service lists, layers first, tables after, service order kept.
pub fn parse_service(json: &serde_json::Value) -> Result<(ServiceHead, Vec<i64>), String> {
    let raw: RawService =
        serde_json::from_value(json.clone()).map_err(|error| error.to_string())?;
    let ids = raw
        .layers
        .iter()
        .chain(raw.tables.iter())
        .map(|listed| listed.id)
        .collect();
    Ok((
        ServiceHead {
            description: raw
                .service_description
                .or(raw.description)
                .filter(|text| !text.trim().is_empty()),
            versioned: raw.has_versioned_data,
        },
        ids,
    ))
}

/// The service-level facts, before any layer is fetched.
pub struct ServiceHead {
    pub description: Option<String>,
    pub versioned: bool,
}

pub fn parse_layer(json: &serde_json::Value) -> Result<Layer, String> {
    let raw: RawLayer = serde_json::from_value(json.clone()).map_err(|error| error.to_string())?;
    let capabilities = raw
        .advanced_query_capabilities
        .unwrap_or(RawQueryCapabilities {
            supports_pagination: false,
            supports_query_attachments: false,
        });
    let reference = raw.extent.and_then(|extent| extent.spatial_reference);
    // a MapServer layer often leaves objectIdField empty and declares the
    // object id only as a field, so the field is the fallback
    let object_id_field = raw
        .object_id_field
        .filter(|name| !name.is_empty())
        .or_else(|| {
            raw.fields
                .iter()
                .find(|field| field.kind == "esriFieldTypeOID")
                .map(|field| field.name.clone())
        });
    Ok(Layer {
        id: raw.id,
        kind: raw.kind.filter(|kind| !kind.is_empty()),
        parent_layer: raw.parent_layer.and_then(|held| held.name),
        sub_layers: raw
            .sub_layers
            .into_iter()
            .filter_map(|held| held.name)
            .collect(),
        versioned: raw.is_data_versioned,
        geometry_type: raw.geometry_type.filter(|kind| kind != "esriGeometryNull"),
        has_z: raw.has_z,
        has_m: raw.has_m,
        wkid: reference
            .as_ref()
            .and_then(|held| held.latest_wkid.or(held.wkid)),
        crs_wkt: reference
            .and_then(|held| held.wkt)
            .filter(|wkt| !wkt.trim().is_empty()),
        object_id_field,
        max_record_count: raw.max_record_count,
        supports_pagination: capabilities.supports_pagination,
        supports_query_attachments: capabilities.supports_query_attachments,
        has_attachments: raw.has_attachments,
        has_metadata: raw.has_metadata,
        renderer: raw
            .drawing_info
            .and_then(|info| info.renderer)
            .and_then(|renderer| renderer.kind),
        time_aware: raw.time_info.is_some_and(|info| !info.is_null()),
        fields: raw.fields.into_iter().map(field).collect(),
        subtype_field: raw.subtype_field.filter(|name| !name.is_empty()),
        default_subtype_code: raw.default_subtype_code.filter(|code| !code.is_null()),
        subtypes: raw.subtypes.into_iter().map(subtype).collect(),
        relationships: raw
            .relationships
            .into_iter()
            .map(|held| RelationshipEnd {
                id: held.id,
                name: held.name,
                related_table_id: held.related_table_id,
                cardinality: held.cardinality,
                role: held.role,
                key_field: held.key_field.filter(|name| !name.is_empty()),
                composite: held.composite,
                relationship_table_id: held.relationship_table_id,
            })
            .collect(),
        count: None,
        name: raw.name,
    })
}

fn field(raw: RawField) -> Field {
    let alias = raw
        .alias
        .filter(|alias| !alias.is_empty() && *alias != raw.name);
    Field {
        domain: raw.domain.and_then(domain),
        name: raw.name,
        kind: raw.kind,
        alias,
        nullable: raw.nullable,
    }
}

/// A domain the sidecar can hold: coded or range. An `inherited` marker is
/// not a domain, and a kind the docs do not name is left out rather than
/// guessed at.
fn domain(raw: RawDomain) -> Option<Domain> {
    let kind = match raw.kind.as_str() {
        "codedValue" => DomainKind::Coded(
            raw.coded_values
                .into_iter()
                .map(|value| {
                    let code = text(&value.code);
                    let label = value.name.unwrap_or_else(|| code.clone());
                    (code, label)
                })
                .collect(),
        ),
        "range" => {
            let bound = |index: usize| raw.range.get(index).and_then(serde_json::Value::as_f64);
            DomainKind::Range {
                min: bound(0).or(raw.min),
                max: bound(1).or(raw.max),
            }
        }
        _ => return None,
    };
    Some(Domain {
        name: raw.name.unwrap_or_default(),
        kind,
    })
}

fn subtype(raw: RawSubtype) -> Subtype {
    let domains = raw
        .domains
        .into_iter()
        .filter_map(|(field, value)| {
            let raw: RawDomain = serde_json::from_value(value).ok()?;
            Some((field, domain(raw)?))
        })
        .collect();
    Subtype {
        code: raw.code,
        name: raw.name,
        default_values: raw.default_values,
        domains,
    }
}

/// A JSON scalar as the text the sidecar writes: a string as it stands, a
/// number or bool as it prints.
pub fn text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
