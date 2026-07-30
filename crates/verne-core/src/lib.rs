//! The verne inventory model.
//!
//! Everything an adapter needs is public: an adapter is any crate that depends
//! on `verne-core` and implements [`Source`], in this workspace or out of it.
//! There is no registry and no dynamic loading.

pub mod model;
pub mod report;
pub mod sidecar;

pub use model::{Item, ItemKind, Losses, Outcome, SourceDescription, Target, Verdict};
pub use report::{Counts, Report};
pub use sidecar::{
    Action, DatasetPlan, ExtractionLog, GEOPACKAGE_FILE, LogCounts, LogEntry, NewDataset,
    NewDomain, NewRelationship, NewSubtype, SIDECAR_FILE, Sidecar,
};

/// A thing verne can read.
///
/// The trait has no write, update or delete method, and takes `&self`
/// throughout, so read-only is a property of the type rather than a promise in
/// a doc comment. Adapters keep their own error type: an inventory that cannot
/// be produced must fail loudly, never come back empty.
pub trait Source {
    type Error: std::error::Error;

    /// What this source is, without listing its contents.
    fn describe(&self) -> SourceDescription;

    /// Everything in the source, with a verdict on each.
    fn inventory(&self) -> Result<Vec<Item>, Self::Error>;
}
