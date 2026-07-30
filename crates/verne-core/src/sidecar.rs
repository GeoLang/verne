//! What an extraction produces beside the features: the datasets, domains,
//! subtypes and relationship classes to create in ptolemy, and a log of what
//! was taken and what was left behind.
//!
//! Every request-shaped struct here mirrors a ptolemy request body field for
//! field, so loading is a POST of the struct rather than a translation that can
//! drift from the API. Three fields cannot mirror one. A subtype's domain
//! assignments and a relationship's two sides name ptolemy rows by the id of a
//! thing that does not exist until the load is running, so those carry the
//! source's names and the loader does the swap; both are typed as maps and
//! names rather than as ids, so the swap cannot be forgotten. The third is an
//! attachment's bytes, which ptolemy takes as base64 in the body: keeping a
//! blob in here would make the sidecar unreadable, so the bytes are a file
//! beside it and the loader encodes them.
//!
//! Pure serde: no GDAL, no HTTP, no filesystem.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::model::{Item, ItemKind, SourceDescription, Verdict};

/// The sidecar's name inside an extraction directory. Both names are here
/// rather than with the adapter that writes them: they are the layout of an
/// extraction, and the loader has to find one without knowing what read it.
pub const SIDECAR_FILE: &str = "sidecar.json";

/// The GeoPackage's name inside an extraction directory.
pub const GEOPACKAGE_FILE: &str = "features.gpkg";

/// Where the per-dataset feature files go, one line of JSON per feature.
///
/// The features are in the GeoPackage as well, and this is a second copy of
/// them. The loader builds without GDAL and must stay that way, so it cannot
/// open the GeoPackage; reading one with a SQLite crate instead would mean
/// verne decoding the GeoPackage geometry header and re-deriving each column's
/// JSON type from SQLite's dynamic one, which is GDAL's work done a second time
/// and by hand. The cost of the copy is disk: an extraction directory holds
/// every feature twice.
pub const FEATURES_DIR: &str = "features";

/// Where the attachment blobs go, one file each.
pub const ATTACHMENTS_DIR: &str = "attachments";

/// The most one feature's JSON may be, in bytes.
///
/// ptolemy reads a commit body with axum's JSON extractor and raises no limit
/// of its own, so the 2 MB default stands: a body over it comes back 413 with
/// the body never read. This is that with room for the rest of the batch, and
/// it is one number rather than two because it answers both questions. An
/// extraction will not write a feature bigger than this, since nothing could
/// ever commit it, and a load flushes a batch before it would cross it.
///
/// One real feature hits this: the outermost hydrologic unit boundary of a
/// USGS geodatabase is a single polygon of 2.7 MB.
pub const MAX_FEATURE_BYTES: usize = 1024 * 1024;

/// A dataset to create: `POST /api/v1/datasets`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewDataset {
    pub name: String,
    pub srid: i32,
    /// One of ptolemy's names: `point`, `linestring`, `polygon`, `multipoint`,
    /// `multilinestring`, `multipolygon`, `geometrycollection`, `geometry`.
    /// Anything else is read there as `point` without complaint, so an
    /// extraction maps every type itself instead of letting that default decide.
    pub geometry_type: String,
    /// The operator who ran the extraction. With auth on ptolemy overwrites it
    /// with the token subject; it is here so the sidecar says who made it even
    /// when it is never loaded.
    pub created_by: String,
}

/// One column: ptolemy's `FieldDef`.
///
/// `allowed_values`, `min` and `max` are left out. They all default on the
/// wire, and verne fills none of them: what would go in them is a domain, and
/// domains are carried as domains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewField {
    pub name: String,
    /// One of ptolemy's names: `string`, `integer`, `float`, `boolean`,
    /// `array`, `object`. Unlike a dataset's geometry type, an unknown name is
    /// rejected there rather than defaulted, so a wrong one fails the load
    /// rather than landing quietly.
    pub field_type: String,
    /// Whether a feature must carry a value. Read off the source column's
    /// nullability, so ptolemy demands exactly what the geodatabase did.
    pub required: bool,
    /// The label the source's users read the column by, where it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
}

/// A dataset's schema: `PUT /api/v1/datasets/{id}/schema`.
///
/// `geometry_rules` is left out: it defaults there, and the dataset already
/// says what geometry it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSchema {
    pub fields: Vec<NewField>,
}

impl NewSchema {
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// A coded or range domain: `POST /api/v1/datasets/{id}/domains`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewDomain {
    pub name: String,
    /// `coded_value` or `range`.
    pub domain_type: String,
    /// `string`, `integer` or `float`.
    pub field_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coded_values: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range_max: Option<f64>,
}

impl NewDomain {
    /// Codes and their labels, in the `[{"code": ..., "name": ...}]` shape
    /// ptolemy's schema documents for the column.
    pub fn coded<C: Into<String>, L: Into<String>>(
        name: impl Into<String>,
        field_type: impl Into<String>,
        values: impl IntoIterator<Item = (C, L)>,
    ) -> Self {
        let coded: Vec<serde_json::Value> = values
            .into_iter()
            .map(|(code, label)| serde_json::json!({ "code": code.into(), "name": label.into() }))
            .collect();
        NewDomain {
            name: name.into(),
            domain_type: "coded_value".into(),
            field_type: field_type.into(),
            coded_values: Some(serde_json::Value::Array(coded)),
            range_min: None,
            range_max: None,
        }
    }

    pub fn range(
        name: impl Into<String>,
        field_type: impl Into<String>,
        min: Option<f64>,
        max: Option<f64>,
    ) -> Self {
        NewDomain {
            name: name.into(),
            domain_type: "range".into(),
            field_type: field_type.into(),
            coded_values: None,
            range_min: min,
            range_max: max,
        }
    }
}

/// A subtype: `POST /api/v1/datasets/{id}/subtypes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSubtype {
    pub subtype_field: String,
    pub name: String,
    pub code: i32,
    /// Field to default value, ptolemy's `default_values` unchanged.
    pub default_values: serde_json::Map<String, serde_json::Value>,
    /// Field to domain *name*. ptolemy's `domain_assignments` holds the id of a
    /// domain row, and no domain has one until it is loaded, so the loader
    /// swaps each name for the id that domain came back with.
    pub domain_assignments: BTreeMap<String, String>,
}

/// A relationship class: `POST /api/v1/datasets/{id}/relationships`.
///
/// Both sides are dataset *names* for the same reason a subtype names its
/// domains: the ids do not exist until the datasets are created. The path
/// parameter is ignored by ptolemy, which reads the two sides out of the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewRelationship {
    pub name: String,
    pub origin_dataset: String,
    pub destination_dataset: String,
    /// The field on the destination side holding the origin's key.
    pub origin_foreign_key: String,
    /// `one_to_one`, `one_to_many` or `many_to_many`.
    pub cardinality: String,
    pub forward_label: String,
    pub backward_label: String,
}

/// One feature to insert: one `insert` operation of ptolemy's
/// `POST /api/v1/branches/{id}/commit`, tag included, so a commit body is
/// these lines put in an array and nothing else.
///
/// The id is minted by the extraction rather than left to ptolemy, which is
/// what lets an attachment name the feature it belongs to without the load
/// reading anything back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename = "insert")]
pub struct NewFeature {
    pub feature_id: String,
    /// The geometry as hex WKB. A row from a table with no geometry, or a row
    /// whose geometry is null, carries an empty geometry collection: ptolemy's
    /// insert has no way to say "no geometry" that is not also how it says
    /// "deleted".
    pub geometry_wkb_hex: String,
    pub properties: serde_json::Map<String, serde_json::Value>,
}

/// One attachment: `POST /api/v1/branches/{branch}/features/{feature}/attachments`
/// with the bytes in a file instead of inline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewAttachment {
    /// The dataset the feature belongs to, by the name the sidecar creates it
    /// under. The upload route wants a branch, and the branch is the one the
    /// load made for this dataset.
    pub dataset: String,
    /// The feature it hangs off, as minted in that dataset's feature file.
    pub feature_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    /// The file holding the bytes, named relative to the sidecar. The one
    /// field that is not what ptolemy is sent: the loader reads the file and
    /// base64s it into `data`.
    pub file: String,
    /// Every column of the source row that is not the blob itself, so nothing
    /// the `__ATTACH` table held is dropped without being written down.
    pub metadata: serde_json::Value,
    pub created_by: String,
}

/// One dataset and everything ptolemy hangs off it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetPlan {
    /// What the source called it.
    pub source_table: String,
    /// The GeoPackage layer holding its features, absent when none was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    /// The file of [`NewFeature`] lines to commit, named relative to the
    /// sidecar. Absent when the extraction wrote none. Defaulted on the wire
    /// because a sidecar written before features were loaded at all had none
    /// to name, so reading it as "no features" is the truth about it and not a
    /// silent drop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub features: Option<String>,
    pub dataset: NewDataset,
    /// No serde default: a sidecar written before schemas existed would
    /// otherwise load with an empty one and drop every alias without saying so,
    /// which is the failure this field was added to stop.
    pub schema: NewSchema,
    pub domains: Vec<NewDomain>,
    pub subtypes: Vec<NewSubtype>,
}

/// Everything an extraction produced apart from the features themselves.
///
/// The order of the fields is the order a load has to run in: a dataset before
/// the domains and subtypes that reference it, every dataset before a
/// relationship class, which names two of them, and the attachments last of
/// all, because each one hangs off a feature on a branch and neither exists
/// until its dataset has been created and committed to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    /// The source it came out of, as the report describes it.
    pub source: SourceDescription,
    /// The GeoPackage the features went to, named relative to the sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geopackage: Option<String>,
    pub datasets: Vec<DatasetPlan>,
    pub relationships: Vec<NewRelationship>,
    /// Every attachment that could be attributed to a feature. One that could
    /// not is not in here: it is a skipped entry in the log with the reason.
    /// Defaulted on the wire for the same reason `DatasetPlan::features` is.
    #[serde(default)]
    pub attachments: Vec<NewAttachment>,
    pub log: ExtractionLog,
}

impl Sidecar {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a sidecar holds only serialisable values")
    }

    pub fn from_json(text: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(text)
    }

    /// The plan for a dataset by the name it will have in ptolemy.
    pub fn dataset(&self, name: &str) -> Option<&DatasetPlan> {
        self.datasets.iter().find(|plan| plan.dataset.name == name)
    }
}

/// What an extraction did with one thing.
///
/// For a thing the inventory judged, which variant it gets is decided by the
/// verdict and not by the caller, so the log cannot claim something came across
/// whole that the report called approximated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Written out with nothing left behind.
    Carried,
    /// Written out, minus these.
    CarriedWithLoss { losses: Vec<String> },
    /// Not written out, and why.
    Skipped { reason: String },
}

impl Action {
    /// Carried when there is nothing to lose, and approximated otherwise, so a
    /// loss cannot be added without downgrading the entry that reports it.
    fn of(losses: Vec<String>) -> Self {
        if losses.is_empty() {
            Action::Carried
        } else {
            Action::CarriedWithLoss { losses }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// Where it was in the source, in the source's own terms.
    pub location: String,
    pub kind: ItemKind,
    pub detail: String,
    #[serde(flatten)]
    pub action: Action,
    /// Where it landed: a GeoPackage layer, or a section of the sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<String>,
}

/// Who ran an extraction, when, and what came of everything it met.
///
/// The licence terms this exists for want an extraction to be accountable
/// rather than promised, so the operator is recorded and nothing verne read
/// goes unmentioned: an item the extraction passed over is an entry saying so,
/// never an absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionLog {
    /// The operator who ran it, as they named themselves.
    pub operator: String,
    /// RFC 3339, UTC.
    pub extracted_at: String,
    pub entries: Vec<LogEntry>,
}

impl ExtractionLog {
    pub fn new(operator: impl Into<String>) -> Self {
        ExtractionLog {
            operator: operator.into(),
            extracted_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .expect("the current time formats as RFC 3339"),
            entries: Vec::new(),
        }
    }

    /// A row of the report, written out at `destination`.
    ///
    /// The verdict decides how the entry reads: an approximated item is logged
    /// with the report's own words for what it lost, and an item with no home
    /// at all is logged as skipped however sure the caller was that it wrote
    /// something.
    pub fn carried(&mut self, item: &Item, destination: impl Into<String>) {
        let (action, destination) = match &item.verdict {
            Verdict::Faithful { .. } => (Action::Carried, Some(destination.into())),
            Verdict::Approximated { losses, .. } => (
                Action::CarriedWithLoss {
                    losses: losses.iter().map(str::to_string).collect(),
                },
                Some(destination.into()),
            ),
            Verdict::Unsupported { reason } | Verdict::NotApplicable { reason } => (
                Action::Skipped {
                    reason: reason.clone(),
                },
                None,
            ),
        };
        self.push(item, action, destination);
    }

    /// A row of the report the extraction did not write out. A report that
    /// already said why there is no home for it wins over the caller's reason,
    /// so the two cannot give different accounts of the same row.
    pub fn skipped(&mut self, item: &Item, reason: impl Into<String>) {
        let reason = match &item.verdict {
            Verdict::Unsupported { reason } | Verdict::NotApplicable { reason } => reason.clone(),
            Verdict::Faithful { .. } | Verdict::Approximated { .. } => reason.into(),
        };
        self.push(item, Action::Skipped { reason }, None);
    }

    /// Something the extraction itself did that no verdict covers. The
    /// inventory judges the source against the platform and says nothing about
    /// the file verne writes on the way, so what the GeoPackage step drops is
    /// recorded here.
    pub fn converted(
        &mut self,
        location: impl Into<String>,
        kind: ItemKind,
        detail: impl Into<String>,
        destination: impl Into<String>,
        losses: Vec<String>,
    ) {
        self.entries.push(LogEntry {
            location: location.into(),
            kind,
            detail: detail.into(),
            action: Action::of(losses),
            destination: Some(destination.into()),
        });
    }

    fn push(&mut self, item: &Item, action: Action, destination: Option<String>) {
        self.entries.push(LogEntry {
            location: item.location.clone(),
            kind: item.kind,
            detail: item.detail.clone(),
            action,
            destination,
        });
    }

    pub fn counts(&self) -> LogCounts {
        let mut counts = LogCounts {
            total: self.entries.len(),
            carried: 0,
            approximated: 0,
            skipped: 0,
        };
        for entry in &self.entries {
            match entry.action {
                Action::Carried => counts.carried += 1,
                Action::CarriedWithLoss { .. } => counts.approximated += 1,
                Action::Skipped { .. } => counts.skipped += 1,
            }
        }
        counts
    }

    pub fn to_markdown(&self) -> String {
        let counts = self.counts();
        let mut lines = vec![
            "# Verne extraction log".to_string(),
            String::new(),
            format!("Extracted by {} at {}.", self.operator, self.extracted_at),
            String::new(),
            format!("**{}**", counts.sentence()),
            String::new(),
            "| Location | Kind | Detail | Action | Went to | What was lost or why not |"
                .to_string(),
            "|---|---|---|---|---|---|".to_string(),
        ];
        for entry in &self.entries {
            let (action, note) = match &entry.action {
                Action::Carried => ("carried", String::new()),
                Action::CarriedWithLoss { losses } => ("approximated", losses.join("; ")),
                Action::Skipped { reason } => ("skipped", reason.clone()),
            };
            lines.push(format!(
                "| {} | {} | {} | {} | {} | {} |",
                cell(&entry.location),
                entry.kind,
                cell(&entry.detail),
                action,
                cell(entry.destination.as_deref().unwrap_or("nothing")),
                cell(&note),
            ));
        }
        lines.push(String::new());
        lines.join("\n")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogCounts {
    pub total: usize,
    pub carried: usize,
    pub approximated: usize,
    pub skipped: usize,
}

impl LogCounts {
    pub fn sentence(&self) -> String {
        format!(
            "{} things: {} carried whole, {} approximated, {} skipped.",
            self.total, self.carried, self.approximated, self.skipped
        )
    }
}

fn cell(text: &str) -> String {
    text.replace('|', r"\|").replace('\n', " ")
}
