//! What an extraction produces beside the features: the datasets, domains,
//! subtypes and relationship classes to create in ptolemy, and a log of what
//! was taken and what was left behind.
//!
//! Every request-shaped struct here mirrors a ptolemy request body field for
//! field, so loading is a POST of the struct rather than a translation that can
//! drift from the API. Two fields cannot mirror one: a subtype's domain
//! assignments and a relationship's two sides name ptolemy rows by the id of a
//! thing that does not exist until the load is running, so those carry the
//! source's names and the loader does the swap. Both are typed as maps and
//! names rather than as ids, so the swap cannot be forgotten.
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

/// One dataset and everything ptolemy hangs off it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DatasetPlan {
    /// What the source called it.
    pub source_table: String,
    /// The GeoPackage layer holding its features, absent when none was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<String>,
    pub dataset: NewDataset,
    pub domains: Vec<NewDomain>,
    pub subtypes: Vec<NewSubtype>,
}

/// Everything an extraction produced apart from the features themselves.
///
/// The order of the fields is the order a load has to run in: a dataset before
/// the domains and subtypes that reference it, and every dataset before a
/// relationship class, which names two of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    /// The source it came out of, as the report describes it.
    pub source: SourceDescription,
    /// The GeoPackage the features went to, named relative to the sidecar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geopackage: Option<String>,
    pub datasets: Vec<DatasetPlan>,
    pub relationships: Vec<NewRelationship>,
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
