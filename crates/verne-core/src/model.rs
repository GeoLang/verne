use std::fmt;

use serde::{Serialize, Serializer, ser::SerializeSeq, ser::SerializeStruct};

/// Where an inventoried thing would land in GeoLang.
///
/// One variant per destination component, so a verdict names a real home
/// instead of gesturing at "the platform".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// ptolemy, the versioned feature store
    Ptolemy,
    /// jung, symbology and cartographic rendering
    Jung,
    /// nubis, point clouds
    Nubis,
    /// terrano, rasters and terrain
    Terrano,
    /// interiora, indoor and building models
    Interiora,
    /// geogit, edit history and versioning
    Geogit,
    /// geodukt, pipeline-expressible transformation
    Geodukt,
}

impl Target {
    pub fn component(self) -> &'static str {
        match self {
            Target::Ptolemy => "ptolemy",
            Target::Jung => "jung",
            Target::Nubis => "nubis",
            Target::Terrano => "terrano",
            Target::Interiora => "interiora",
            Target::Geogit => "geogit",
            Target::Geodukt => "geodukt",
        }
    }

    pub fn holds(self) -> &'static str {
        match self {
            Target::Ptolemy => "features and attributes",
            Target::Jung => "symbology and cartographic styling",
            Target::Nubis => "point clouds",
            Target::Terrano => "rasters and terrain",
            Target::Interiora => "indoor and building models",
            Target::Geogit => "edit history and versioning",
            Target::Geodukt => "pipeline-expressible transformation",
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.component(), self.holds())
    }
}

impl Serialize for Target {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut out = s.serialize_struct("Target", 2)?;
        out.serialize_field("component", self.component())?;
        out.serialize_field("holds", self.holds())?;
        out.end()
    }
}

/// A non-empty list of things a conversion would drop.
///
/// There is no way to build an empty one, so an `Approximated` verdict that
/// says nothing about what is lost cannot be written down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Losses {
    head: String,
    rest: Vec<String>,
}

impl Losses {
    pub fn one(loss: impl Into<String>) -> Self {
        Losses {
            head: loss.into(),
            rest: Vec::new(),
        }
    }

    pub fn and(mut self, loss: impl Into<String>) -> Self {
        self.rest.push(loss.into());
        self
    }

    /// Append every loss in an iterator, keeping the list non-empty.
    pub fn and_all<S: Into<String>>(mut self, losses: impl IntoIterator<Item = S>) -> Self {
        self.rest.extend(losses.into_iter().map(Into::into));
        self
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.head.as_str()).chain(self.rest.iter().map(String::as_str))
    }

    pub fn count(&self) -> usize {
        1 + self.rest.len()
    }
}

impl fmt::Display for Losses {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let joined: Vec<&str> = self.iter().collect();
        write!(f, "{}", joined.join("; "))
    }
}

impl Serialize for Losses {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.count()))?;
        for loss in self.iter() {
            seq.serialize_element(loss)?;
        }
        seq.end()
    }
}

/// How well GeoLang can hold one inventoried thing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Verdict {
    /// Carried across without loss, and this is where it goes.
    Faithful { target: Target },
    /// Carried across with something left behind, named in `losses`.
    Approximated { target: Target, losses: Losses },
    /// No home in GeoLang, and why.
    Unsupported { reason: String },
}

impl Verdict {
    pub fn faithful(target: Target) -> Self {
        Verdict::Faithful { target }
    }

    pub fn approximated(target: Target, losses: Losses) -> Self {
        Verdict::Approximated { target, losses }
    }

    pub fn unsupported(reason: impl Into<String>) -> Self {
        Verdict::Unsupported {
            reason: reason.into(),
        }
    }

    pub fn outcome(&self) -> Outcome {
        match self {
            Verdict::Faithful { .. } => Outcome::Faithful,
            Verdict::Approximated { .. } => Outcome::Approximated,
            Verdict::Unsupported { .. } => Outcome::Unsupported,
        }
    }

    pub fn target(&self) -> Option<Target> {
        match self {
            Verdict::Faithful { target } | Verdict::Approximated { target, .. } => Some(*target),
            Verdict::Unsupported { .. } => None,
        }
    }

    /// What a conversion would lose: the named losses, or the reason there is
    /// no home at all.
    pub fn shortfall(&self) -> String {
        match self {
            Verdict::Faithful { .. } => String::new(),
            Verdict::Approximated { losses, .. } => losses.to_string(),
            Verdict::Unsupported { reason } => reason.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Faithful,
    Approximated,
    Unsupported,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Outcome::Faithful => "faithful",
            Outcome::Approximated => "approximated",
            Outcome::Unsupported => "unsupported",
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Vendor-neutral category of an inventoried thing.
///
/// These recur across platforms. Add a variant when an adapter meets something
/// none of them describes, not in advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Features with geometry, counted per container.
    FeatureCollection,
    /// Symbols, colours, labels: how features are drawn.
    Styling,
    /// Grouping and nesting of layers or features.
    Hierarchy,
    /// Field definitions, types and domains.
    AttributeSchema,
    /// Times and time ranges on data.
    Temporal,
    /// A pointer at something outside the source.
    ExternalReference,
    /// Names, descriptions and provenance of the source itself.
    Metadata,
    /// A file carried inside the source container.
    EmbeddedResource,
    /// An image registered to the map.
    RasterOverlay,
    /// Display rules driven by the camera or the screen rather than the data.
    ViewDependentDisplay,
    /// A 3D model placed at a location.
    Mesh,
}

impl ItemKind {
    pub fn label(self) -> &'static str {
        match self {
            ItemKind::FeatureCollection => "feature collection",
            ItemKind::Styling => "styling",
            ItemKind::Hierarchy => "hierarchy",
            ItemKind::AttributeSchema => "attribute schema",
            ItemKind::Temporal => "temporal",
            ItemKind::ExternalReference => "external reference",
            ItemKind::Metadata => "metadata",
            ItemKind::EmbeddedResource => "embedded resource",
            ItemKind::RasterOverlay => "raster overlay",
            ItemKind::ViewDependentDisplay => "view-dependent display",
            ItemKind::Mesh => "mesh",
        }
    }
}

impl fmt::Display for ItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One thing found in a source, and what would become of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    /// Where it lives in the source, in the source's own terms.
    pub location: String,
    pub kind: ItemKind,
    /// Counts, types, names: whatever makes the row concrete for this kind.
    pub detail: String,
    pub verdict: Verdict,
}

impl Item {
    pub fn new(
        location: impl Into<String>,
        kind: ItemKind,
        detail: impl Into<String>,
        verdict: Verdict,
    ) -> Self {
        Item {
            location: location.into(),
            kind,
            detail: detail.into(),
            verdict,
        }
    }
}

/// What a source is, before saying what is in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceDescription {
    /// The format or platform, as an operator would name it.
    pub format: String,
    /// Path, URL or connection label the operator pointed verne at.
    pub location: String,
    pub detail: Option<String>,
}

impl SourceDescription {
    pub fn new(format: impl Into<String>, location: impl Into<String>) -> Self {
        SourceDescription {
            format: format.into(),
            location: location.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}
