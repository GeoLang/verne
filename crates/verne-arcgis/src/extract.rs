//! Extracting a feature service into a form ptolemy accepts.
//!
//! The same contract as the gdb extraction, minus the GeoPackage: writing one
//! is GDAL's work and this crate has none, so the feature files and the
//! sidecar are the whole of what lands on disk. Nothing here decides whether
//! a thing can be carried: that was decided in `verdict.rs`, and the log
//! restates the verdict rather than forming a second opinion.
//!
//! The features come down `/query` a page at a time, asked for in EPSG:4326,
//! so the service transforms and verne only encodes. A page is asked for
//! again until the service stops saying `exceededTransferLimit`, which its
//! docs say can outlive the last full page.
//!
//! A delta reads the same route: what a change file changes is which rows are
//! asked for, not how they are read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use verne_core::{
    ATTACHMENTS_DIR, DatasetPlan, DeleteFeature, ExtractionLog, FEATURES_DIR, FeatureOp, Item,
    ItemKind, MAX_FEATURE_BYTES, NewAttachment, NewDataset, NewDomain, NewFeature, NewField,
    NewRelationship, NewSchema, NewSubtype, SIDECAR_FILE, Sidecar, Source, UpdateFeature,
    safe_file_name,
};

use crate::changes::{
    self, AttachmentEdits, ChangeFile, LayerChanges, LayerGen, RecordedGen, RecordedGens,
};
use crate::client::{Fetch, json, json_post};
use crate::geometry::{EMPTY_GEOMETRY, EsriGeometry, Position};
use crate::service::{DomainKind, Layer, text};
use crate::verdict::{self, Pairing};
use crate::{ArcgisError, ArcgisSource};

/// The srid every dataset declares: what its geometry is by the time ptolemy
/// has it, because every query asked the service for it.
const PTOLEMY_SRID: i32 = 4326;

/// The page size when a layer names no `maxRecordCount` of its own.
const DEFAULT_PAGE: u64 = 1000;

/// How many object ids go in one `queryAttachments` request. Bounded by URL
/// length rather than by any documented limit.
const ATTACHMENT_BATCH: usize = 100;

/// Where an extraction landed. No GeoPackage on this path: the sidecar's
/// `geopackage` is absent and the loader already reads that as none.
#[derive(Debug, Clone)]
pub struct Extraction {
    pub directory: PathBuf,
    pub sidecar_path: PathBuf,
    pub sidecar: Sidecar,
}

impl ArcgisSource {
    /// Read the service and write it out into `directory`.
    ///
    /// `&self` throughout: every request is a read, and everything written
    /// goes into the extraction directory. The `operator` is recorded in the
    /// log, which is what makes an extraction accountable rather than
    /// anonymous.
    pub fn extract(&self, directory: &Path, operator: &str) -> Result<Extraction, ArcgisError> {
        self.extract_inner(directory, operator, None)
    }

    /// Read the service again and write only what changed since `previous`,
    /// an earlier extraction of it, as insert, update and delete operations
    /// `verne load` commits onto the datasets the first load created.
    ///
    /// What changed is asked of the service where it can be: a previous
    /// extraction that recorded the generations it read at, against a service
    /// still tracking changes, means `extractChanges` names the object ids
    /// edited since and only those rows are fetched. Otherwise the diff is
    /// verne's own, and the whole current state is fetched and paired with the
    /// previous feature files by object id, with a hash of geometry and
    /// properties deciding changed from unchanged. The report says which of
    /// the two ran and why.
    ///
    /// A delta of a delta rides the `extractChanges` path and only that one.
    /// Both halves of the basis chain there: the generation is the server's own
    /// cursor, and the object id index each delta writes down says which
    /// feature id ptolemy holds every row under, which its feature files cannot
    /// because they hold only the rows it touched. A delta with no index, or a
    /// run that would fall back to the local diff, is refused: the local diff
    /// would read every row the previous delta left alone as vanished.
    pub fn extract_since(
        &self,
        directory: &Path,
        operator: &str,
        previous: &Path,
    ) -> Result<Extraction, ArcgisError> {
        let sidecar_path = previous.join(SIDECAR_FILE);
        let text = std::fs::read_to_string(&sidecar_path).map_err(|source| ArcgisError::Read {
            path: sidecar_path.display().to_string(),
            source,
        })?;
        let sidecar = Sidecar::from_json(&text).map_err(|error| ArcgisError::BadPrevious {
            path: sidecar_path.display().to_string(),
            message: error.to_string(),
        })?;
        // asked before the service is, so a run that cannot be paired does not
        // start a job on the server either
        if sidecar.incremental
            && let Some(reason) = unindexed(previous, &sidecar)
        {
            return Err(ArcgisError::DeltaPrevious {
                path: previous.display().to_string(),
                reason,
            });
        }
        let path = self.delta_path(changes::read(previous)?.as_ref())?;
        if let (true, DeltaPath::LocalDiff(reason)) = (sidecar.incremental, &path) {
            return Err(ArcgisError::DeltaPrevious {
                path: previous.display().to_string(),
                reason: format!(
                    "{reason}, and a local diff would read every row that delta left alone as vanished"
                ),
            });
        }
        self.extract_inner(
            directory,
            operator,
            Some(Previous {
                directory: previous.to_path_buf(),
                sidecar,
                path,
            }),
        )
    }

    /// Whether this delta can ride `extractChanges`, and the change file when
    /// it can.
    ///
    /// All or nothing: one run has one meaning, so a single layer without a
    /// recorded generation or without an object id field puts the whole
    /// service on the local diff rather than leaving a report whose counts
    /// were arrived at two different ways.
    fn delta_path(&self, recorded: Option<&RecordedGens>) -> Result<DeltaPath, ArcgisError> {
        let service = &self.service;
        let local = |reason: String| Ok(DeltaPath::LocalDiff(reason));
        let Some(recorded) = recorded else {
            return local(
                "the previous extraction recorded no server generations, so there is no cursor to send back and the whole service was read again and diffed locally".into(),
            );
        };
        if !service.change_tracking {
            return local(
                "the service no longer states ChangeTracking among its capabilities, so extractChanges has no window to answer for and the whole service was read again and diffed locally".into(),
            );
        }
        let mut gens: Vec<LayerGen> = Vec::new();
        for layer in service.layers.iter().filter(|layer| layer.queryable()) {
            let Some(server_gen) = recorded.of_layer(layer.id) else {
                return local(format!(
                    "the previous extraction recorded no generation for layer {} ({}), and a change file covers the layers it was asked about or none, so the whole service was read again and diffed locally",
                    layer.id, layer.name
                ));
            };
            if layer.object_id_field.is_none() {
                return local(format!(
                    "{} names no object id field, so the object ids a change file holds could not be paired with anything, and the whole service was read again and diffed locally",
                    layer.name
                ));
            }
            gens.push(LayerGen {
                id: layer.id,
                server_gen,
            });
        }
        if gens.is_empty() {
            return local(
                "the service lists no layer that answers a query, so there is nothing to ask extractChanges about".into(),
            );
        }
        let status = match changes::submit(self.fetch.as_ref(), &service.url, &gens) {
            Ok(status) => status,
            // the service would not start the job at all, and the local diff
            // answers the same question the slow way
            Err(error) => {
                return local(format!(
                    "the service refused extractChanges ({error}), so the whole service was read again and diffed locally"
                ));
            }
        };
        // past the submit the job is the service's own, and a job that fails
        // or never finishes is an error rather than a quiet re-read of
        // everything: only the operator can decide to pay for that
        Ok(DeltaPath::Changes(changes::collect(
            self.fetch.as_ref(),
            &status,
        )?))
    }

    fn extract_inner(
        &self,
        directory: &Path,
        operator: &str,
        previous: Option<Previous>,
    ) -> Result<Extraction, ArcgisError> {
        let service = &self.service;
        let items = verdict::items(service);
        std::fs::create_dir_all(directory).map_err(|source| ArcgisError::Write {
            path: directory.display().to_string(),
            source,
        })?;

        let mut placed: Vec<(Item, Placed)> = Vec::new();
        let mut conversions: Vec<Conversion> = Vec::new();

        // group and raster layers get a report row and no dataset: neither
        // answers /query with features
        let planned: Vec<&Layer> = service
            .layers
            .iter()
            .filter(|layer| layer.queryable())
            .collect();
        // one plan per planned layer, in layer order, so the two walk
        // together by index
        let mut datasets = dataset_plans(&planned, operator, &mut placed, &mut conversions);
        // a delta carries feature operations only: the relationship classes
        // were created when the full extraction was loaded, and repeating
        // them here would have the loader create second copies
        let relationships = match &previous {
            None => relationship_classes(service, &planned, &datasets, &mut placed),
            Some(_) => {
                for pairing in verdict::pairings(service) {
                    placed.push((
                        verdict::relationship_item(&pairing),
                        Placed::Left(
                            "the relationship classes were created when the full extraction was loaded, and a delta does not repeat them".into(),
                        ),
                    ));
                }
                Vec::new()
            }
        };

        let mut attachments = Vec::new();
        for (plan_index, layer) in planned.iter().copied().enumerate() {
            // files are named after the dataset, not the layer: dataset names
            // are unique where duplicate layer names were suffixed, and a file
            // per layer name would have the second layer overwrite the first
            let dataset_name = datasets[plan_index].dataset.name.clone();
            let basis = match &previous {
                None => None,
                Some(held) => match delta_basis(held, &dataset_name, layer)? {
                    Ok(basis) => Some(basis),
                    Err(reason) => {
                        // nothing can be diffed for this layer, so no feature
                        // file is written and the plan carries none
                        conversions.push(Conversion {
                            location: layer.name.clone(),
                            kind: ItemKind::FeatureCollection,
                            detail: "no delta computed".into(),
                            destination: None,
                            losses: vec![reason],
                        });
                        if layer.has_attachments {
                            placed.push((
                                verdict::attachment_item(layer),
                                left_of_delta(AttachmentEdits::default()),
                            ));
                        }
                        continue;
                    }
                },
            };
            // what the service said changed on this layer, where it was asked
            let changed = match (&previous, &layer.object_id_field) {
                (Some(held), Some(oid_field)) => match &held.path {
                    DeltaPath::Changes(file) => Some(file.layer(layer.id, oid_field)),
                    DeltaPath::LocalDiff(_) => None,
                },
                _ => None,
            };
            let attachment_edits = changed
                .as_ref()
                .map(|changes| changes.attachments)
                .unwrap_or_default();
            let pass = match (basis, changed) {
                (None, _) => Pass::Full,
                (Some(basis), None) => Pass::Diff(basis),
                (Some(basis), Some(changes)) => Pass::Changed {
                    changes,
                    previous: basis,
                },
            };
            let incremental = !matches!(pass, Pass::Full);
            let (file, minted) = write_layer(
                self.fetch.as_ref(),
                &service.url,
                layer,
                &dataset_name,
                service.gdb_version.as_deref(),
                directory,
                pass,
            )?;
            datasets[plan_index].features = file.path.clone();
            conversions.push(Conversion {
                location: layer.name.clone(),
                kind: ItemKind::FeatureCollection,
                detail: match &file.delta {
                    None => format!("{} feature{}", file.features, plural(file.features)),
                    Some(delta) => format!(
                        "{} inserted, {} updated, {} deleted; {} unchanged",
                        delta.inserted, delta.updated, delta.deleted, delta.unchanged
                    ),
                },
                destination: file.path.clone(),
                losses: file.losses,
            });
            if layer.has_attachments {
                if incremental {
                    // not diffed: what the full extraction attached stands,
                    // and pairing attachment changes would need a second
                    // listing pass this path does not make
                    placed.push((
                        verdict::attachment_item(layer),
                        left_of_delta(attachment_edits),
                    ));
                } else {
                    let carried = self.attachments(
                        layer,
                        &dataset_name,
                        &minted,
                        directory,
                        operator,
                        &mut attachments,
                    )?;
                    place_attachments(layer, carried, &mut placed, &mut conversions);
                }
            }
        }
        if let Some(held) = &previous {
            // which of the two delta paths ran, and why it was that one
            conversions.push(match &held.path {
                DeltaPath::Changes(_) => Conversion {
                    location: verdict::ROOT.into(),
                    kind: ItemKind::Temporal,
                    detail: "delta read from extractChanges".into(),
                    destination: Some("the delta's feature files".into()),
                    losses: Vec::new(),
                },
                DeltaPath::LocalDiff(reason) => Conversion {
                    location: verdict::ROOT.into(),
                    kind: ItemKind::Temporal,
                    detail: "delta found by reading the service again and diffing it against the previous extraction"
                        .into(),
                    destination: Some("the delta's feature files".into()),
                    losses: vec![reason.clone()],
                },
            });
            // a dataset of the previous extraction the service no longer
            // lists: its features stand in ptolemy, and deleting a whole
            // dataset is a decision for an operator, not a diff
            for plan in &held.sidecar.datasets {
                if !datasets
                    .iter()
                    .any(|current| current.dataset.name == plan.dataset.name)
                {
                    conversions.push(Conversion {
                        location: plan.source_table.clone(),
                        kind: ItemKind::FeatureCollection,
                        detail: "in the previous extraction, not in the service now".into(),
                        destination: None,
                        losses: vec![
                            "the service no longer lists this layer, so no delta was computed; its features stand in ptolemy until someone deletes them deliberately".into(),
                        ],
                    });
                }
            }
        }

        let mut log = ExtractionLog::new(operator);
        // walked in report order, so the log reads down the same list the
        // markdown report does
        for item in &items {
            // consumed as matched: two layers with identical names and
            // metadata make equal items, and each must find its own placement
            match placed.iter().position(|(held, _)| held == item) {
                Some(index) => match placed.remove(index).1 {
                    Placed::At(destination) => log.carried(item, destination),
                    Placed::Left(reason) => log.skipped(item, &reason),
                },
                None => log.skipped(item, left_behind(item.kind)),
            }
        }
        for conversion in conversions {
            match conversion.destination {
                Some(destination) => log.converted(
                    conversion.location,
                    conversion.kind,
                    conversion.detail,
                    destination,
                    conversion.losses,
                ),
                None => log.not_converted(
                    conversion.location,
                    conversion.kind,
                    conversion.detail,
                    conversion.losses.join("; "),
                ),
            }
        }

        // the cursor this extraction can be continued from: what the service
        // published at a full read, or what the change file's window ended at
        let gens: BTreeMap<i64, u64> = match &previous {
            None if service.change_tracking => service
                .layer_server_gens
                .iter()
                .map(|held| (held.id, held.server_gen))
                .collect(),
            Some(held) => match &held.path {
                DeltaPath::Changes(file) => file.gens(),
                DeltaPath::LocalDiff(_) => BTreeMap::new(),
            },
            None => BTreeMap::new(),
        };
        let recorded: Vec<RecordedGen> = planned
            .iter()
            .zip(&datasets)
            .filter_map(|(layer, plan)| {
                Some(RecordedGen {
                    dataset: plan.dataset.name.clone(),
                    layer: layer.id,
                    server_gen: *gens.get(&layer.id)?,
                })
            })
            .collect();
        if !recorded.is_empty() {
            changes::write(
                directory,
                &RecordedGens {
                    service: service.url.clone(),
                    layers: recorded,
                },
            )?;
        }

        let sidecar = Sidecar {
            source: self.describe(),
            incremental: previous.is_some(),
            geopackage: None,
            datasets,
            relationships,
            attachments,
            log,
        };
        let sidecar_path = directory.join(SIDECAR_FILE);
        std::fs::write(&sidecar_path, sidecar.to_json() + "\n").map_err(|source| {
            ArcgisError::Write {
                path: sidecar_path.display().to_string(),
                source,
            }
        })?;

        Ok(Extraction {
            directory: directory.to_path_buf(),
            sidecar_path,
            sidecar,
        })
    }
}

/// What became of one row of the report.
enum Placed {
    At(String),
    Left(String),
}

/// A loss the extraction itself took, which no verdict covers.
struct Conversion {
    location: String,
    kind: ItemKind,
    detail: String,
    destination: Option<String>,
    losses: Vec<String>,
}

/// The earlier extraction a delta is measured from, and how.
struct Previous {
    directory: PathBuf,
    sidecar: Sidecar,
    path: DeltaPath,
}

/// Why a delta cannot be the basis of another one, or `None` when it can: it
/// wrote an index of every dataset it lists, which is what says where each row
/// of them landed in ptolemy.
fn unindexed(directory: &Path, sidecar: &Sidecar) -> Option<String> {
    let missing: Vec<&str> = sidecar
        .datasets
        .iter()
        .filter(|plan| !changes::index_path(directory, &plan.dataset.name).exists())
        .map(|plan| plan.dataset.name.as_str())
        .collect();
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "it holds no {} index for {}, so the feature ids the rest of those datasets were loaded under are not written down anywhere",
        changes::OBJECT_IDS_DIR,
        missing.join(", ")
    ))
}

/// How a delta finds what changed, decided once for the whole run before any
/// layer is read.
enum DeltaPath {
    /// The service named what changed, and this is what it said.
    Changes(ChangeFile),
    /// The whole service is read again and paired with the previous
    /// extraction's feature files, for the reason held here.
    LocalDiff(String),
}

/// Where a delta leaves a layer's attachments, and what the service said it
/// missed by leaving them.
fn left_of_delta(edits: AttachmentEdits) -> Placed {
    let mut reason =
        "a delta carries feature operations only; the attachments the full extraction carried stand, and changes to them are not diffed"
            .to_string();
    if edits.any() {
        reason.push_str(&format!(
            "; the service counted {} added, {} updated and {} deleted attachment{} in this window, which a full extraction would pick up",
            edits.added,
            edits.updated,
            edits.deleted,
            plural(edits.added + edits.updated + edits.deleted)
        ));
    }
    Placed::Left(reason)
}

/// Why a thing the inventory judged was not written out, for a row that could
/// have been carried and was not.
fn left_behind(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::EmbeddedResource => {
            "an attachment reaches ptolemy on the feature it belongs to, and this one names no feature this extraction created, so there is nothing to hang it off"
        }
        ItemKind::Styling => {
            "the renderer stays on the service: this extraction writes datasets, domains, subtypes, relationship classes, features and attachments, and jung is not among its outputs"
        }
        ItemKind::Metadata => {
            "ptolemy takes a metadata record through a route of its own, which this extraction does not use"
        }
        ItemKind::Hierarchy => {
            "ptolemy has no container above a dataset, so there is nothing to create for the grouping itself; each member that holds features became its own dataset"
        }
        ItemKind::RasterOverlay => {
            "verne does not fetch or open the raster, so it is named in the report and nothing was extracted"
        }
        _ => {
            "this extraction writes datasets, domains, subtypes, relationship classes, features and attachments, and nothing else"
        }
    }
}

// ─── Datasets, domains and subtypes ─────────────────────────────────

fn dataset_plans(
    planned: &[&Layer],
    operator: &str,
    placed: &mut Vec<(Item, Placed)>,
    conversions: &mut Vec<Conversion>,
) -> Vec<DatasetPlan> {
    let mut plans: Vec<DatasetPlan> = Vec::new();
    for layer in planned.iter().copied() {
        // two layers may share a name across the layer and table lists, and
        // ptolemy datasets are told apart by name
        let mut name = layer.name.clone();
        if plans.iter().any(|plan| plan.dataset.name == name) {
            name = format!("{} {}", layer.name, layer.id);
            conversions.push(Conversion {
                location: layer.name.clone(),
                kind: ItemKind::FeatureCollection,
                detail: format!("dataset created as {name}"),
                destination: Some(format!("dataset {name}")),
                losses: vec![format!(
                    "another layer of this service is already called {}, and ptolemy datasets are told apart by name, so the layer id was appended",
                    layer.name
                )],
            });
        }
        let (geometry_type, dropped) = ptolemy_geometry(layer);
        if !dropped.is_empty() {
            conversions.push(Conversion {
                location: layer.name.clone(),
                kind: ItemKind::FeatureCollection,
                detail: format!("geometry type recorded as {geometry_type}"),
                destination: Some(format!("dataset {name}")),
                losses: dropped,
            });
        }
        placed.push((
            verdict::layer_item(layer),
            Placed::At(format!("dataset {name}")),
        ));
        let domains = domains_of(layer, conversions);
        let subtypes = subtypes_of(layer, conversions);
        if let Some(item) = verdict::subtype_item(layer) {
            let destination = if subtypes.is_empty() {
                Placed::Left(
                    "every subtype on the layer carries a code ptolemy cannot hold, so there was nothing left to create".into(),
                )
            } else {
                Placed::At(format!("subtypes of {name}"))
            };
            placed.push((item, destination));
        }
        for (domain, users) in verdict::layer_domains(layer) {
            placed.push((
                verdict::domain_item(layer, &domain, &users),
                Placed::At(format!("domains of {name}")),
            ));
        }
        plans.push(DatasetPlan {
            source_table: layer.name.clone(),
            layer: None,
            features: None,
            object_id_field: layer.object_id_field.clone(),
            dataset: NewDataset {
                name,
                srid: PTOLEMY_SRID,
                geometry_type: geometry_type.to_string(),
                created_by: operator.to_string(),
            },
            schema: columns_of(layer, conversions),
            domains,
            subtypes,
        });
    }
    plans
}

/// ptolemy's name for a layer's geometry, and what calling it that drops.
///
/// A polyline layer is declared multilinestring and a polygon layer
/// multipolygon, which is also how every feature is encoded, so a one-path
/// feature and a three-path one agree with the dataset that holds them.
fn ptolemy_geometry(layer: &Layer) -> (&'static str, Vec<String>) {
    let mut losses = Vec::new();
    if layer.has_z {
        losses.push(
            "the layer carries a Z ordinate and ptolemy's geometry_type names 2D shapes only, so the dataset does not declare it".to_string(),
        );
    }
    if layer.has_m {
        losses.push(
            "the layer carries an M ordinate and ptolemy's geometry_type names 2D shapes only, so the dataset does not declare it".to_string(),
        );
    }
    let name = match layer.geometry_type.as_deref() {
        None => "geometry",
        Some("esriGeometryPoint") => "point",
        Some("esriGeometryMultipoint") => "multipoint",
        Some("esriGeometryPolyline") => "multilinestring",
        Some("esriGeometryPolygon") => "multipolygon",
        Some(other) => {
            losses.push(format!(
                "{other} is a shape ptolemy has no name for, so the dataset is declared as holding any geometry"
            ));
            "geometry"
        }
    };
    (name, losses)
}

/// The layer's columns as ptolemy's dataset schema holds them. This is where
/// a field alias lands, and storing it is the whole of what happens to it.
fn columns_of(layer: &Layer, conversions: &mut Vec<Conversion>) -> NewSchema {
    let mut fields = Vec::new();
    let mut retyped = Vec::new();
    for field in &layer.fields {
        if field.kind == "esriFieldTypeGeometry" {
            continue;
        }
        let (field_type, approximated) = verdict::schema_field_type(&field.kind);
        if let Some(loss) = approximated {
            retyped.push(loss);
        }
        fields.push(NewField {
            name: field.name.clone(),
            field_type: field_type.to_string(),
            required: !field.nullable,
            alias: field.alias.clone(),
        });
    }
    if !retyped.is_empty() {
        conversions.push(Conversion {
            location: layer.name.clone(),
            kind: ItemKind::AttributeSchema,
            detail: format!(
                "{} column{} declared as the nearest type ptolemy has",
                retyped.len(),
                plural(retyped.len())
            ),
            destination: Some(format!("schema of {}", layer.name)),
            losses: retyped,
        });
    }
    NewSchema { fields }
}

/// Every distinct domain the layer uses, as the sidecar creates them.
fn domains_of(layer: &Layer, conversions: &mut Vec<Conversion>) -> Vec<NewDomain> {
    let mut out: Vec<NewDomain> = Vec::new();
    for (domain, _) in verdict::layer_domains(layer) {
        if out.iter().any(|held| held.name == domain.name) {
            continue;
        }
        // the domain rides on a field, and the field's own type is what
        // ptolemy's domain declares
        let field_kind = layer
            .fields
            .iter()
            .find(|field| {
                field
                    .domain
                    .as_ref()
                    .is_some_and(|held| held.name == domain.name)
            })
            .map(|field| field.kind.as_str())
            .or_else(|| {
                layer
                    .subtypes
                    .iter()
                    .flat_map(|subtype| subtype.domains.iter())
                    .find(|(_, held)| held.name == domain.name)
                    .and_then(|(field, _)| {
                        layer
                            .fields
                            .iter()
                            .find(|held| held.name == *field)
                            .map(|held| held.kind.as_str())
                    })
            })
            .unwrap_or("esriFieldTypeString");
        let (field_type, retyped) = verdict::domain_field_type(field_kind);
        if let Some(loss) = retyped {
            conversions.push(Conversion {
                location: domain.name.clone(),
                kind: ItemKind::AttributeSchema,
                detail: format!("domain field type recorded as {field_type}"),
                destination: Some(format!("domains of {}", layer.name)),
                losses: vec![loss],
            });
        }
        out.push(match &domain.kind {
            DomainKind::Coded(values) => NewDomain::coded(
                domain.name.clone(),
                field_type,
                values
                    .iter()
                    .map(|(code, label)| (code.clone(), label.clone())),
            ),
            DomainKind::Range { min, max } => {
                NewDomain::range(domain.name.clone(), field_type, *min, *max)
            }
        });
    }
    out
}

fn subtypes_of(layer: &Layer, conversions: &mut Vec<Conversion>) -> Vec<NewSubtype> {
    let Some(field) = &layer.subtype_field else {
        return Vec::new();
    };
    let mut subtypes = Vec::new();
    for subtype in &layer.subtypes {
        // ptolemy keys a subtype on an integer, so a code that is not one has
        // nothing to be written as
        let code = subtype
            .code
            .as_i64()
            .and_then(|code| i32::try_from(code).ok());
        let Some(code) = code else {
            conversions.push(Conversion {
                location: layer.name.clone(),
                kind: ItemKind::AttributeSchema,
                detail: format!(
                    "subtype \"{}\" with code {}",
                    subtype.name,
                    text(&subtype.code)
                ),
                destination: Some(format!("subtypes of {}", layer.name)),
                losses: vec![format!(
                    "ptolemy's subtype code is an integer and this one is {}, so the subtype was not written",
                    text(&subtype.code)
                )],
            });
            continue;
        };
        subtypes.push(NewSubtype {
            subtype_field: field.clone(),
            name: subtype.name.clone(),
            code,
            default_values: subtype.default_values.clone(),
            domain_assignments: subtype
                .domains
                .iter()
                .map(|(field, domain)| (field.clone(), domain.name.clone()))
                .collect(),
        });
    }
    subtypes
}

// ─── Relationship classes ───────────────────────────────────────────

fn relationship_classes(
    service: &crate::Service,
    planned: &[&Layer],
    plans: &[DatasetPlan],
    placed: &mut Vec<(Item, Placed)>,
) -> Vec<NewRelationship> {
    // the class names ptolemy datasets, which are the plans' names, not the
    // layers': the two differ where a duplicate layer name was suffixed
    let names: BTreeMap<i64, &str> = planned
        .iter()
        .zip(plans)
        .map(|(layer, plan)| (layer.id, plan.dataset.name.as_str()))
        .collect();
    let mut classes = Vec::new();
    for pairing in verdict::pairings(service) {
        let item = verdict::relationship_item(&pairing);
        match new_relationship(&pairing, &names) {
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

fn new_relationship(
    pairing: &Pairing<'_>,
    names: &BTreeMap<i64, &str>,
) -> Result<NewRelationship, String> {
    let origin = pairing.origin;
    let Some((destination_layer, destination)) = &pairing.destination else {
        return Err(format!(
            "{} relates to table id {}, which is not among the layers verne was pointed at, and a relationship class in ptolemy names two dataset ids",
            origin.name, origin.related_table_id
        ));
    };
    let cardinality = match origin.cardinality.as_str() {
        "esriRelCardinalityOneToOne" => "one_to_one",
        "esriRelCardinalityOneToMany" => "one_to_many",
        "esriRelCardinalityManyToMany" => "many_to_many",
        other => {
            return Err(format!(
                "the service states the cardinality as \"{other}\", which verne does not know, and guessing one would misstate the class"
            ));
        }
    };
    // the key ptolemy wants is the one on the destination side that holds the
    // origin's key, which is what the destination end's keyField names
    let key = destination.key_field.clone().ok_or_else(|| {
        format!(
            "{} names no key field on its destination side, and ptolemy's class is keyed on one",
            origin.name
        )
    })?;
    Ok(NewRelationship {
        name: origin.name.clone(),
        origin_dataset: names
            .get(&pairing.origin_layer.id)
            .unwrap_or(&pairing.origin_layer.name.as_str())
            .to_string(),
        destination_dataset: names
            .get(&destination_layer.id)
            .unwrap_or(&destination_layer.name.as_str())
            .to_string(),
        origin_foreign_key: key,
        cardinality: cardinality.to_string(),
        // the REST layer description carries no labels, and the report says so
        forward_label: String::new(),
        backward_label: String::new(),
    })
}

// ─── Features ───────────────────────────────────────────────────────

/// How one layer's rows are read and what they are compared against.
enum Pass {
    /// Every row of the layer, and every row is an insert.
    Full,
    /// Every row again, paired with the previous extraction by object id.
    Diff(PrevFeatures),
    /// Only the object ids the service said changed, paired the same way.
    Changed {
        changes: LayerChanges,
        previous: PrevFeatures,
    },
}

/// What writing one layer's features came to.
struct FeatureFile {
    path: Option<String>,
    features: usize,
    losses: Vec<String>,
    /// The operation counts when the layer was diffed rather than dumped.
    delta: Option<Delta>,
}

/// What one layer's delta came to. Unchanged features are counted rather
/// than written: the count is the proof the diff saw them.
struct Delta {
    inserted: usize,
    updated: usize,
    deleted: usize,
    unchanged: usize,
}

/// One layer of the previous extraction, keyed for diffing: object id to the
/// feature id that extraction minted and a hash of what it wrote.
struct PrevFeatures {
    map: BTreeMap<String, (String, u64)>,
    /// What reading the previous file already lost, carried into the layer's
    /// loss lines.
    losses: Vec<String>,
}

/// What decides changed from unchanged: the working geometry and the
/// properties, as the feature file holds them. The original is left out
/// because it is derived from the same geometry by the same service. A
/// collision reads a changed feature as unchanged, at one-in-2^64 per pair.
///
/// FNV-1a by hand rather than the standard library's hasher, whose value is
/// documented as not to be relied on across releases: a chained delta compares
/// against a hash an earlier run wrote into its index, so the same feature has
/// to hash the same in another process built by another compiler.
fn feature_hash(
    geometry_wkb_hex: &str,
    properties: &serde_json::Map<String, serde_json::Value>,
) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    eat(geometry_wkb_hex.as_bytes());
    // the properties are a BTreeMap, so one order of keys serialises them
    eat(serde_json::to_string(properties)
        .expect("properties hold only serialisable values")
        .as_bytes());
    hash
}

/// The previous extraction's state of one layer, or the reason no delta can
/// be computed for it, which becomes the log's account. The outer error is a
/// previous feature file that cannot be read at all, which fails the
/// extraction rather than shrugging.
fn delta_basis(
    previous: &Previous,
    dataset_name: &str,
    layer: &Layer,
) -> Result<Result<PrevFeatures, String>, ArcgisError> {
    let Some(plan) = previous.sidecar.dataset(dataset_name) else {
        return Ok(Err(format!(
            "{dataset_name} is not in the previous extraction, so there is nothing to diff against; a full extract and load creates it"
        )));
    };
    let Some(oid_field) = plan.object_id_field.as_deref() else {
        return Ok(Err(
            "the previous extraction recorded no object id field, so its features cannot be paired with the service's and no delta was computed".into(),
        ));
    };
    if layer.object_id_field.is_none() {
        return Ok(Err(
            "the layer names no object id field now, so the current features cannot be paired with the previous extraction's".into(),
        ));
    }
    let mut basis = PrevFeatures {
        map: BTreeMap::new(),
        losses: Vec::new(),
    };
    // a delta's feature files hold only the rows that delta touched, so what
    // pairs a chain is the index it wrote down instead. extract_since refuses
    // a delta that has none, so reaching here means there is one.
    if previous.sidecar.incremental {
        for row in changes::read_index(&previous.directory, dataset_name)? {
            basis.map.insert(row.oid, (row.feature_id, row.hash));
        }
        return Ok(Ok(basis));
    }
    let Some(relative) = &plan.features else {
        // the previous extraction wrote no features, so everything is new
        return Ok(Ok(basis));
    };
    let path = previous.directory.join(relative);
    let file = std::fs::File::open(&path).map_err(|source| ArcgisError::Read {
        path: path.display().to_string(),
        source,
    })?;
    use std::io::BufRead;
    let mut unkeyed = 0usize;
    for (index, line) in std::io::BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|source| ArcgisError::Read {
            path: path.display().to_string(),
            source,
        })?;
        if line.trim().is_empty() {
            continue;
        }
        let feature: NewFeature =
            serde_json::from_str(&line).map_err(|error| ArcgisError::BadPrevious {
                path: path.display().to_string(),
                message: format!("line {}: {error}", index + 1),
            })?;
        let Some(oid) = feature.properties.get(oid_field) else {
            unkeyed += 1;
            continue;
        };
        basis.map.insert(
            text(oid),
            (
                feature.feature_id.clone(),
                feature_hash(&feature.geometry_wkb_hex, &feature.properties),
            ),
        );
    }
    if unkeyed > 0 {
        basis.losses.push(format!(
            "{unkeyed} feature{} of the previous extraction carry no {oid_field} value, so the delta cannot see {}: each old copy stands, and the same row can come back as a fresh insert",
            plural(unkeyed),
            if unkeyed == 1 { "it" } else { "them" }
        ));
    }
    Ok(Ok(basis))
}

/// What one layer's rows came to.
#[derive(Default)]
struct Tally {
    read: usize,
    written: usize,
    shapeless: usize,
    unwritable: usize,
    oversized: usize,
    largest: usize,
    /// The delta's split of the rows; all zero on a full extraction.
    inserted: usize,
    updated: usize,
    deleted: usize,
    unchanged: usize,
    /// Rows a diff could not pair because the object id field held nothing.
    unkeyed: usize,
    /// Vertices whose declared Z or M was missing and written as zero.
    nulled_ordinates: usize,
    /// Rows whose untransformed original did not come back from the second
    /// pass, so their inserts carry the working copy alone.
    no_native: usize,
}

/// How a transformed layer's original reference is said to ptolemy: by EPSG
/// code when one names it, or by the WKT definition when only that does.
enum OriginalRef {
    Code(i32),
    Wkt(String),
}

/// Whether this layer's features get a second, untransformed fetch, and how
/// the original's reference would be declared. None when there is nothing to
/// fetch: a table, a layer already in 4326, or a reference verne cannot state,
/// where a fetched original could not be declared to ptolemy anyway.
fn original_ref(layer: &Layer) -> Option<OriginalRef> {
    layer.geometry_type.as_ref()?;
    match (layer.wkid, &layer.crs_wkt) {
        (Some(4326), _) => None,
        // wkids below 33000 are EPSG codes; from there up they are Esri's own
        // authority, which ptolemy's native_srid cannot name
        (Some(code), _) if code < 33000 => Some(OriginalRef::Code(code)),
        (_, Some(wkt)) => Some(OriginalRef::Wkt(wkt.clone())),
        _ => None,
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPage {
    #[serde(default)]
    features: Vec<RawFeature>,
    #[serde(default)]
    exceeded_transfer_limit: bool,
    #[serde(default)]
    has_z: bool,
    #[serde(default)]
    has_m: bool,
}

#[derive(serde::Deserialize)]
struct RawFeature {
    #[serde(default)]
    attributes: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    geometry: Option<serde_json::Value>,
}

/// One layer's features as commit operations, and the object id of every
/// feature that was written, keyed for the attachments that hang off them.
///
/// On a full pass every row is an insert. On either delta pass the row's
/// object id decides: unknown ids are inserts, known ids are updates when the
/// hash says they changed and a count when it says they did not.
///
/// What counts as a delete is the one thing the two delta passes part over. A
/// local diff sees the whole current state, so whatever is left of the
/// previous extraction at the end vanished from the service. A change pass
/// sees only what it asked for, so a delete is one the service named, and one
/// it named that the previous extraction never held is counted and dropped.
///
/// A change pass also writes the dataset's object id index, which is the basis
/// it was given with every operation it wrote applied to it.
fn write_layer(
    fetch: &dyn Fetch,
    url: &str,
    layer: &Layer,
    dataset: &str,
    gdb_version: Option<&str>,
    directory: &Path,
    pass: Pass,
) -> Result<(FeatureFile, BTreeMap<String, String>), ArcgisError> {
    let relative = format!("{FEATURES_DIR}/{}.ndjson", safe_file_name(dataset));
    let path = directory.join(&relative);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ArcgisError::Write {
            path: parent.display().to_string(),
            source,
        })?;
    }
    let file = std::fs::File::create(&path).map_err(|source| ArcgisError::Write {
        path: path.display().to_string(),
        source,
    })?;
    use std::io::Write;
    let mut out = std::io::BufWriter::new(file);

    // the columns whose values are read, with the ptolemy type each was
    // declared as, so a value cannot arrive as a type the schema says it is not
    let readable: Vec<(&str, &str, &'static str)> = layer
        .fields
        .iter()
        .filter(|field| verdict::carries_values(field))
        .filter(|field| field.kind != "esriFieldTypeGeometry")
        .map(|field| {
            (
                field.name.as_str(),
                field.kind.as_str(),
                verdict::schema_field_type(&field.kind).0,
            )
        })
        .collect();

    let mut minted = BTreeMap::new();
    let mut tally = Tally::default();
    let mut losses = Vec::new();
    let (mut previous, changed) = match pass {
        Pass::Full => (None, None),
        Pass::Diff(previous) => (Some(previous), None),
        Pass::Changed { changes, previous } => (Some(previous), Some(changes)),
    };
    let diffed = previous.is_some();
    if let Some(prev) = &mut previous {
        losses.append(&mut prev.losses);
    }
    let page_size = layer.max_record_count.unwrap_or(DEFAULT_PAGE);
    let route = format!("{url}/{}/query", layer.id);
    // the original is only fetchable when there is an object id to pair the
    // two passes on; without one the pairing would be a guess
    let original = match (original_ref(layer), &layer.object_id_field) {
        (Some(original), Some(_)) => Some(original),
        (Some(_), None) => {
            losses.push(
                "the layer names no object id field, so the untransformed originals could not be paired with the features and none were fetched".to_string(),
            );
            None
        }
        (None, _) => None,
    };
    // the change pass asks for the ids the service named, in batches the size
    // of a page it would have answered anyway
    let batch_size = usize::try_from(page_size).unwrap_or(usize::MAX).max(1);
    let mut batches = changed
        .as_ref()
        .map(|changes| {
            changes
                .touched
                .chunks(batch_size)
                .map(<[String]>::to_vec)
                .collect::<Vec<Vec<String>>>()
        })
        .unwrap_or_default()
        .into_iter();
    let mut paging = Paging::default();
    // the change pass leaves an index behind, which is the basis plus every op
    // it writes: the feature file it writes cannot say where the rows it did
    // not touch landed, and the next delta of the chain has to know
    let mut index = match (&changed, &previous) {
        (Some(_), Some(prev)) => Some(prev.map.clone()),
        _ => None,
    };
    loop {
        let page = match &changed {
            None => match paging.next(fetch, &route, layer, gdb_version, &mut losses)? {
                Some(page) => page,
                None => break,
            },
            Some(_) => match batches.next() {
                Some(batch) => changed_page(fetch, &route, layer, gdb_version, &batch)?,
                None => break,
            },
        };

        // the same rows again, untransformed, asked for by the object ids this
        // page just delivered so an edit mid-extraction cannot skew the pairing
        let natives = match &original {
            Some(_) => native_page(fetch, &route, layer, gdb_version, &page, &mut tally)?,
            None => BTreeMap::new(),
        };

        for feature in &page.features {
            tally.read += 1;
            let geometry = match (&layer.geometry_type, &feature.geometry) {
                (None, _) => EMPTY_GEOMETRY.to_string(),
                (Some(_), None) => {
                    tally.shapeless += 1;
                    EMPTY_GEOMETRY.to_string()
                }
                (Some(kind), Some(value)) => {
                    match esri_geometry(
                        kind,
                        value,
                        layer.has_z || page.has_z,
                        layer.has_m || page.has_m,
                        &mut tally,
                    ) {
                        Some(shape) => shape.wkb_hex(),
                        None => {
                            tally.unwritable += 1;
                            EMPTY_GEOMETRY.to_string()
                        }
                    }
                }
            };
            let mut properties = serde_json::Map::new();
            for (name, esri, declared) in &readable {
                if let Some(value) = json_value(&feature.attributes, name, esri, declared) {
                    properties.insert((*name).to_string(), value);
                }
            }
            let native = original.as_ref().and_then(|_| {
                let oid = layer
                    .object_id_field
                    .as_ref()
                    .and_then(|name| feature.attributes.get(name))?;
                natives.get(&text(oid))
            });
            if original.is_some() && feature.geometry.is_some() && native.is_none() {
                tally.no_native += 1;
            }
            let (native_hex, native_srid, native_crs_wkt) = match (native, &original) {
                (Some(hex), Some(OriginalRef::Code(code))) => {
                    (Some(hex.clone()), Some(*code), None)
                }
                (Some(hex), Some(OriginalRef::Wkt(wkt))) => {
                    (Some(hex.clone()), None, Some(wkt.clone()))
                }
                _ => (None, None, None),
            };
            let insert = |geometry: String, properties| {
                FeatureOp::Insert(NewFeature {
                    feature_id: uuid::Uuid::now_v7().to_string(),
                    geometry_wkb_hex: geometry,
                    properties,
                    native_geometry_wkb_hex: native_hex.clone(),
                    native_srid,
                    native_crs_wkt: native_crs_wkt.clone(),
                })
            };
            // pairing is by object id; a row without one cannot be told apart
            // from any other, so it is counted out
            let oid = layer
                .object_id_field
                .as_ref()
                .and_then(|name| feature.attributes.get(name))
                .map(text);
            // the hash decides changed from unchanged and is also what the
            // index writes down, so it is taken once per row and only where a
            // delta is what is being written
            let hash = diffed.then(|| feature_hash(&geometry, &properties));
            let op = match &mut previous {
                None => insert(geometry, properties),
                Some(prev) => {
                    let Some(oid) = &oid else {
                        tally.unkeyed += 1;
                        continue;
                    };
                    // consumed as matched, so what is left at the end is
                    // exactly what vanished from the service
                    match prev.map.remove(oid) {
                        None => insert(geometry, properties),
                        Some((_, held)) if hash == Some(held) => {
                            tally.unchanged += 1;
                            continue;
                        }
                        Some((feature_id, _)) => FeatureOp::Update(UpdateFeature {
                            feature_id,
                            geometry_wkb_hex: Some(geometry),
                            properties: Some(properties),
                            native_geometry_wkb_hex: native_hex.clone(),
                            native_srid,
                            native_crs_wkt: native_crs_wkt.clone(),
                        }),
                    }
                }
            };
            let line =
                serde_json::to_string(&op).expect("a feature holds only serialisable values");
            // a feature ptolemy would refuse is not written: it would fail the
            // batch it landed in and take the rest of the layer with it
            if line.len() > MAX_FEATURE_BYTES {
                tally.oversized += 1;
                tally.largest = tally.largest.max(line.len());
                continue;
            }
            if layer.has_attachments
                && let FeatureOp::Insert(held) = &op
                && let Some(oid) = &oid
            {
                minted.insert(oid.clone(), held.feature_id.clone());
            }
            writeln!(out, "{line}").map_err(|source| ArcgisError::Write {
                path: path.display().to_string(),
                source,
            })?;
            tally.written += 1;
            if diffed {
                match &op {
                    FeatureOp::Insert(_) => tally.inserted += 1,
                    FeatureOp::Update(_) => tally.updated += 1,
                    FeatureOp::Delete(_) => {}
                }
            }
            // the op was written, so this is what ptolemy holds the row as once
            // the delta is loaded. a row skipped anywhere above never reaches
            // here, and the index keeps saying what it said before
            if let Some(index) = &mut index
                && let (Some(oid), Some(hash)) = (&oid, hash)
            {
                index.insert(oid.clone(), (op_feature_id(&op).to_string(), hash));
            }
        }
    }
    if let Some(mut prev) = previous {
        // on a local diff, whatever the current state never answered for
        // vanished from the service; on a change pass only the ids the service
        // named as deleted are gone, and the rest of the previous extraction
        // was never asked about
        let deletes: Vec<String> = match &changed {
            None => prev.map.keys().cloned().collect(),
            Some(changes) => changes.deleted.clone(),
        };
        let mut unknown = 0usize;
        for oid in deletes {
            let Some((feature_id, _)) = prev.map.remove(&oid) else {
                unknown += 1;
                continue;
            };
            let line = serde_json::to_string(&FeatureOp::Delete(DeleteFeature { feature_id }))
                .expect("a delete holds only serialisable values");
            writeln!(out, "{line}").map_err(|source| ArcgisError::Write {
                path: path.display().to_string(),
                source,
            })?;
            tally.written += 1;
            tally.deleted += 1;
            if let Some(index) = &mut index {
                index.remove(&oid);
            }
        }
        if unknown > 0 {
            losses.push(format!(
                "the service says {unknown} object id{} {} deleted, and the previous extraction holds no feature for {}, so nothing was written to delete",
                plural(unknown),
                if unknown == 1 { "was" } else { "were" },
                if unknown == 1 { "it" } else { "them" }
            ));
        }
    }
    out.flush().map_err(|source| ArcgisError::Write {
        path: path.display().to_string(),
        source,
    })?;
    if let Some(index) = &index {
        changes::write_index(directory, dataset, index)?;
    }

    losses.extend(feature_losses(layer, original.as_ref(), &tally));
    Ok((
        FeatureFile {
            path: Some(relative),
            features: tally.written,
            losses,
            delta: diffed.then_some(Delta {
                inserted: tally.inserted,
                updated: tally.updated,
                deleted: tally.deleted,
                unchanged: tally.unchanged,
            }),
        },
        minted,
    ))
}

/// The feature an operation is of, which is one field under three names.
fn op_feature_id(op: &FeatureOp) -> &str {
    match op {
        FeatureOp::Insert(held) => &held.feature_id,
        FeatureOp::Update(held) => &held.feature_id,
        FeatureOp::Delete(held) => &held.feature_id,
    }
}

/// The parameters every feature query shares: the columns, the geometry as
/// ptolemy wants it, and the named version when the operator gave one.
fn shape_params(layer: &Layer, gdb_version: Option<&str>) -> Vec<(&'static str, String)> {
    let mut params: Vec<(&str, String)> = vec![("outFields", "*".to_string())];
    if let Some(version) = gdb_version {
        params.push(("gdbVersion", version.to_string()));
    }
    if layer.geometry_type.is_some() {
        params.push(("returnGeometry", "true".to_string()));
        params.push(("outSR", PTOLEMY_SRID.to_string()));
        if layer.has_z {
            params.push(("returnZ", "true".to_string()));
        }
        if layer.has_m {
            params.push(("returnM", "true".to_string()));
        }
    } else {
        params.push(("returnGeometry", "false".to_string()));
    }
    params
}

/// Where the full pass has got to: the offset the next page starts at, how
/// many empty pages with the transfer limit still up have gone by, and whether
/// the service has said it has no more.
#[derive(Default)]
struct Paging {
    offset: u64,
    empty: u32,
    done: bool,
}

impl Paging {
    /// The next page of the layer, or `None` when the layer is exhausted. An
    /// empty page with `exceededTransferLimit` still set is stepped over,
    /// which the docs allow, but not forever.
    fn next(
        &mut self,
        fetch: &dyn Fetch,
        route: &str,
        layer: &Layer,
        gdb_version: Option<&str>,
        losses: &mut Vec<String>,
    ) -> Result<Option<RawPage>, ArcgisError> {
        if self.done {
            return Ok(None);
        }
        let page_size = layer.max_record_count.unwrap_or(DEFAULT_PAGE);
        loop {
            let mut params = shape_params(layer, gdb_version);
            params.push(("where", "1=1".to_string()));
            if layer.supports_pagination {
                params.push(("resultOffset", self.offset.to_string()));
                params.push(("resultRecordCount", page_size.to_string()));
                if let Some(oid) = &layer.object_id_field {
                    params.push(("orderByFields", oid.clone()));
                }
            }
            let value = json(fetch, route, &params)?;
            let page: RawPage =
                serde_json::from_value(value).map_err(|error| ArcgisError::BadShape {
                    route: route.to_string(),
                    message: error.to_string(),
                })?;
            if page.features.is_empty() {
                if page.exceeded_transfer_limit && layer.supports_pagination {
                    self.empty += 1;
                    self.offset += page_size;
                    if self.empty >= 3 {
                        losses.push(
                            "the service kept answering empty pages with exceededTransferLimit still set, so the extraction stopped asking; features past that point were not fetched".to_string(),
                        );
                        return Ok(None);
                    }
                    continue;
                }
                return Ok(None);
            }
            self.empty = 0;
            self.offset += page.features.len() as u64;
            if !page.exceeded_transfer_limit {
                self.done = true;
            } else if !layer.supports_pagination {
                losses.push(format!(
                    "the service answers at most {} record{} at a time and does not support pagination, so only the first page was fetched and the rest of the layer is not in the extraction",
                    page.features.len(),
                    plural(page.features.len())
                ));
                self.done = true;
            }
            return Ok(Some(page));
        }
    }
}

/// One batch of the object ids a change file named, read the same way a page
/// is. A POST because a thousand object ids do not fit in a URL, and no
/// `where`: the id list is the whole of what is asked for.
fn changed_page(
    fetch: &dyn Fetch,
    route: &str,
    layer: &Layer,
    gdb_version: Option<&str>,
    oids: &[String],
) -> Result<RawPage, ArcgisError> {
    let mut params = shape_params(layer, gdb_version);
    params.push(("objectIds", oids.join(",")));
    let value = json_post(fetch, route, &params)?;
    serde_json::from_value(value).map_err(|error| ArcgisError::BadShape {
        route: route.to_string(),
        message: error.to_string(),
    })
}

/// One page's rows again, untransformed: a POST of the page's object ids with
/// no `outSR`, so the service answers in the layer's own reference. A POST
/// because a thousand object ids do not fit in a URL. Keyed by object id, and
/// a row that comes back without one, or with a shape verne cannot encode, is
/// simply not in the map: the caller counts the features that end up bare.
fn native_page(
    fetch: &dyn Fetch,
    route: &str,
    layer: &Layer,
    gdb_version: Option<&str>,
    page: &RawPage,
    tally: &mut Tally,
) -> Result<BTreeMap<String, String>, ArcgisError> {
    let Some(oid_field) = layer.object_id_field.as_deref() else {
        return Ok(BTreeMap::new());
    };
    let oids: Vec<String> = page
        .features
        .iter()
        .filter_map(|feature| feature.attributes.get(oid_field))
        .map(text)
        .collect();
    if oids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut params: Vec<(&str, String)> = vec![
        ("objectIds", oids.join(",")),
        ("outFields", oid_field.to_string()),
        ("returnGeometry", "true".to_string()),
    ];
    if let Some(version) = gdb_version {
        params.push(("gdbVersion", version.to_string()));
    }
    if layer.has_z {
        params.push(("returnZ", "true".to_string()));
    }
    if layer.has_m {
        params.push(("returnM", "true".to_string()));
    }
    let value = json_post(fetch, route, &params)?;
    let natives: RawPage =
        serde_json::from_value(value).map_err(|error| ArcgisError::BadShape {
            route: route.to_string(),
            message: error.to_string(),
        })?;
    let mut out = BTreeMap::new();
    for feature in &natives.features {
        let (Some(oid), Some(kind), Some(value)) = (
            feature.attributes.get(oid_field),
            &layer.geometry_type,
            &feature.geometry,
        ) else {
            continue;
        };
        if let Some(shape) = esri_geometry(
            kind,
            value,
            layer.has_z || natives.has_z,
            layer.has_m || natives.has_m,
            tally,
        ) {
            out.insert(text(oid), shape.wkb_hex());
        }
    }
    Ok(out)
}

/// What reading the rows found, which is the part no verdict can know.
fn feature_losses(layer: &Layer, original: Option<&OriginalRef>, tally: &Tally) -> Vec<String> {
    let mut losses = Vec::new();
    if layer.geometry_type.is_some() && layer.wkid != Some(4326) {
        losses.push(match (original, layer.wkid) {
            (Some(OriginalRef::Code(code)), _) => format!(
                "every geometry here was asked for in EPSG:{PTOLEMY_SRID} and the service transformed it out of EPSG:{code} itself; verne does no coordinate arithmetic and cannot say which datum transformation the service chose. the coordinates as the service stores them were fetched in a second pass, paired by object id, and ride on each insert as EPSG:{code}, which ptolemy keeps beside the working copy"
            ),
            (Some(OriginalRef::Wkt(_)), _) => format!(
                "every geometry here was asked for in EPSG:{PTOLEMY_SRID} and the service transformed it itself; no single EPSG code names the layer's reference, so the coordinates as stored were fetched in a second pass and ride on each insert with the reference's WKT definition, which ptolemy keeps beside the working copy"
            ),
            (None, Some(code)) => format!(
                "every geometry here was asked for in EPSG:{PTOLEMY_SRID} and the service transformed it out of wkid {code} itself; {code} is Esri's own authority rather than an EPSG code and the layer states no WKT for it, so the original could not be declared to ptolemy and was not fetched, and the layer's own reference lives on only in the service"
            ),
            (None, None) => format!(
                "every geometry here was asked for in EPSG:{PTOLEMY_SRID} and the layer states no readable reference of its own, so what the service transformed out of is not written down"
            ),
        });
    }
    if tally.no_native > 0 {
        losses.push(format!(
            "the untransformed original of {} did not come back from the second pass, so those inserts carry the working copy alone",
            of_rows(tally.no_native, tally.read)
        ));
    }
    if layer.supports_pagination && layer.object_id_field.is_none() {
        losses.push(
            "the layer names no object id field, so the pages were fetched without a stable order and an edit made during the extraction can repeat or drop a row".to_string(),
        );
    }
    if tally.unkeyed > 0 {
        losses.push(format!(
            "{} carry no value in the object id field, so they could not be paired with the previous extraction and are not in the delta",
            of_rows(tally.unkeyed, tally.read)
        ));
    }
    if tally.shapeless > 0 {
        losses.push(format!(
            "no geometry on {}, and ptolemy's insert has no way to say so that is not also how a deletion reads, so each was written as an empty geometry collection",
            of_rows(tally.shapeless, tally.read)
        ));
    }
    if tally.unwritable > 0 {
        losses.push(format!(
            "the geometry of {} was not in a shape verne could encode, so each was written with an empty geometry collection in its place",
            of_rows(tally.unwritable, tally.read)
        ));
    }
    if tally.nulled_ordinates > 0 {
        losses.push(format!(
            "{} declared Z or M ordinate{} arrived null and {} written as zero, because WKB has no way to leave one out of a vertex",
            tally.nulled_ordinates,
            plural(tally.nulled_ordinates),
            if tally.nulled_ordinates == 1 { "was" } else { "were" }
        ));
    }
    if tally.oversized > 0 {
        losses.push(format!(
            "an insert bigger than the {MAX_FEATURE_BYTES} bytes ptolemy takes in a request on {}, {}, so they are not in the feature file and no load will create them; unlike a file extraction there is no GeoPackage keeping them, only the service itself",
            of_rows(tally.oversized, tally.read),
            if tally.oversized == 1 {
                format!("which is {} bytes", tally.largest)
            } else {
                format!("the largest {} bytes", tally.largest)
            }
        ));
    }
    losses
}

/// "3 of the 40 rows", as a noun phrase with no verb after it.
fn of_rows(some: usize, all: usize) -> String {
    match (some, all) {
        (_, 1) => "the one row".to_string(),
        (some, all) if some == all => format!("all {all} rows"),
        (some, all) => format!("{some} of the {all} rows"),
    }
}

/// One attribute as JSON, in the type the schema declared the column as.
///
/// A value that will not fit the declared type is left out rather than sent
/// as something else: ptolemy validates a commit against the schema. The one
/// rewrite is a Date, which arrives as epoch milliseconds and is written as
/// the RFC 3339 text the schema's string column expects.
fn json_value(
    attributes: &serde_json::Map<String, serde_json::Value>,
    name: &str,
    esri: &str,
    declared: &str,
) -> Option<serde_json::Value> {
    let value = attributes.get(name)?;
    if value.is_null() {
        return None;
    }
    let value = match (esri, value) {
        ("esriFieldTypeDate", serde_json::Value::Number(number)) => {
            let millis = number.as_i64()?;
            let instant =
                time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(millis) * 1_000_000)
                    .ok()?;
            serde_json::Value::String(
                instant
                    .format(&time::format_description::well_known::Rfc3339)
                    .ok()?,
            )
        }
        _ => value.clone(),
    };
    let fits = match declared {
        "string" => value.is_string(),
        "integer" => value.is_i64() || value.is_u64(),
        "float" => value.is_number(),
        _ => false,
    };
    fits.then_some(value)
}

/// A feature's geometry out of the response JSON. `None` is a shape verne
/// could not encode, which the caller counts rather than drops silently.
fn esri_geometry(
    kind: &str,
    value: &serde_json::Value,
    has_z: bool,
    has_m: bool,
    tally: &mut Tally,
) -> Option<EsriGeometry> {
    // a geometry can carry its own flags; the layer's stand when it does not
    let has_z = value
        .get("hasZ")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(has_z);
    let has_m = value
        .get("hasM")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(has_m);
    match kind {
        "esriGeometryPoint" => {
            let x = value.get("x").and_then(serde_json::Value::as_f64)?;
            let y = value.get("y").and_then(serde_json::Value::as_f64)?;
            Some(EsriGeometry::Point(Position {
                x,
                y,
                z: value.get("z").and_then(serde_json::Value::as_f64),
                m: value.get("m").and_then(serde_json::Value::as_f64),
            }))
        }
        "esriGeometryMultipoint" => {
            let points = value.get("points")?.as_array()?;
            let positions = points
                .iter()
                .map(|held| position(held, has_z, has_m, tally))
                .collect::<Option<Vec<Position>>>()?;
            Some(EsriGeometry::Multipoint(positions))
        }
        "esriGeometryPolyline" => Some(EsriGeometry::Polyline(paths(
            value.get("paths")?.as_array()?,
            has_z,
            has_m,
            tally,
        )?)),
        "esriGeometryPolygon" => Some(EsriGeometry::Polygon(paths(
            value.get("rings")?.as_array()?,
            has_z,
            has_m,
            tally,
        )?)),
        _ => None,
    }
}

fn paths(
    raw: &[serde_json::Value],
    has_z: bool,
    has_m: bool,
    tally: &mut Tally,
) -> Option<Vec<Vec<Position>>> {
    raw.iter()
        .map(|path| {
            path.as_array()?
                .iter()
                .map(|held| position(held, has_z, has_m, tally))
                .collect::<Option<Vec<Position>>>()
        })
        .collect()
}

/// One vertex out of its coordinate array. A declared Z or M that is missing
/// or null becomes zero and is counted: WKB has no way to leave an ordinate
/// out of one vertex of a geometry that has it.
fn position(
    value: &serde_json::Value,
    has_z: bool,
    has_m: bool,
    tally: &mut Tally,
) -> Option<Position> {
    let coordinates = value.as_array()?;
    let number = |index: usize| coordinates.get(index).and_then(serde_json::Value::as_f64);
    let ordinate = |index: usize, wanted: bool, tally: &mut Tally| {
        if !wanted {
            return None;
        }
        match number(index) {
            Some(value) => Some(value),
            None => {
                tally.nulled_ordinates += 1;
                Some(0.0)
            }
        }
    };
    let x = number(0)?;
    let y = number(1)?;
    let z = ordinate(2, has_z, tally);
    let m = ordinate(if has_z { 3 } else { 2 }, has_m, tally);
    Some(Position { x, y, z, m })
}

// ─── Attachments ────────────────────────────────────────────────────

/// What one attachment info the service lists says about itself.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAttachmentInfo {
    id: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    keywords: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAttachmentGroup {
    parent_object_id: serde_json::Value,
    #[serde(default)]
    attachment_infos: Vec<RawAttachmentInfo>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawGroups {
    #[serde(default)]
    attachment_groups: Vec<RawAttachmentGroup>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawInfos {
    #[serde(default)]
    attachment_infos: Vec<RawAttachmentInfo>,
}

/// What became of one layer's attachments.
struct AttachmentsCarried {
    carried: usize,
    orphans: Vec<String>,
}

impl ArcgisSource {
    /// One layer's attachments: listed, downloaded, written as files and named
    /// in the sidecar. A blob that cannot be attributed to a feature the
    /// extraction wrote is skipped and said to be skipped.
    fn attachments(
        &self,
        layer: &Layer,
        dataset: &str,
        minted: &BTreeMap<String, String>,
        directory: &Path,
        operator: &str,
        out: &mut Vec<NewAttachment>,
    ) -> Result<AttachmentsCarried, ArcgisError> {
        let mut carried = AttachmentsCarried {
            carried: 0,
            orphans: Vec::new(),
        };
        let relative_dir = format!("{ATTACHMENTS_DIR}/{}", safe_file_name(dataset));
        let blob_dir = directory.join(&relative_dir);
        std::fs::create_dir_all(&blob_dir).map_err(|source| ArcgisError::Write {
            path: blob_dir.display().to_string(),
            source,
        })?;

        let listed = self.list_attachments(layer, minted)?;
        for (row, (oid, info)) in listed.iter().enumerate() {
            let Some(feature_id) = minted.get(oid) else {
                carried.orphans.push(format!(
                    "attachment {} belongs to object id {oid}, which is not a feature this extraction wrote, so there is nothing to attach it to",
                    info.id
                ));
                continue;
            };
            let route = format!(
                "{}/{}/{oid}/attachments/{}",
                self.service.url, layer.id, info.id
            );
            let bytes = match self.fetch.get(&route, &[]) {
                Ok(bytes) => bytes,
                Err(error) => {
                    carried.orphans.push(format!(
                        "the service would not hand over attachment {} ({error}), so it was left where it is",
                        info.id
                    ));
                    continue;
                }
            };
            let name = info
                .name
                .clone()
                .unwrap_or_else(|| format!("{}-{row}", layer.name));
            let blob = format!("{relative_dir}/{row}-{}", safe_file_name(&name));
            std::fs::write(directory.join(&blob), &bytes).map_err(|source| ArcgisError::Write {
                path: blob.clone(),
                source,
            })?;
            let mut metadata = serde_json::Map::new();
            metadata.insert("source_layer".into(), layer.name.clone().into());
            metadata.insert("attachment_id".into(), info.id.into());
            metadata.insert("object_id".into(), oid.clone().into());
            if let Some(size) = info.size {
                metadata.insert("size".into(), size.into());
            }
            if let Some(keywords) = &info.keywords {
                metadata.insert("keywords".into(), keywords.clone().into());
            }
            out.push(NewAttachment {
                dataset: dataset.to_string(),
                feature_id: feature_id.clone(),
                name,
                content_type: info.content_type.clone(),
                file: blob,
                metadata: serde_json::Value::Object(metadata),
                created_by: operator.to_string(),
            });
            carried.carried += 1;
        }
        Ok(carried)
    }

    /// Every attachment of the features that were written, as `(object id,
    /// info)`. Through `queryAttachments` in batches where the layer supports
    /// it, and one listing per feature where it does not.
    fn list_attachments(
        &self,
        layer: &Layer,
        minted: &BTreeMap<String, String>,
    ) -> Result<Vec<(String, RawAttachmentInfo)>, ArcgisError> {
        let mut listed = Vec::new();
        let oids: Vec<&String> = minted.keys().collect();
        if layer.supports_query_attachments {
            let route = format!("{}/{}/queryAttachments", self.service.url, layer.id);
            for batch in oids.chunks(ATTACHMENT_BATCH) {
                let ids = batch
                    .iter()
                    .map(|oid| oid.as_str())
                    .collect::<Vec<&str>>()
                    .join(",");
                let mut params = vec![("objectIds", ids)];
                if let Some(version) = &self.service.gdb_version {
                    params.push(("gdbVersion", version.clone()));
                }
                let value = json(self.fetch.as_ref(), &route, &params)?;
                let groups: RawGroups =
                    serde_json::from_value(value).map_err(|error| ArcgisError::BadShape {
                        route: route.clone(),
                        message: error.to_string(),
                    })?;
                for group in groups.attachment_groups {
                    let oid = text(&group.parent_object_id);
                    for info in group.attachment_infos {
                        listed.push((oid.clone(), info));
                    }
                }
            }
            return Ok(listed);
        }
        for oid in oids {
            let route = format!("{}/{}/{oid}/attachments", self.service.url, layer.id);
            let mut params: Vec<(&str, String)> = Vec::new();
            if let Some(version) = &self.service.gdb_version {
                params.push(("gdbVersion", version.clone()));
            }
            let value = json(self.fetch.as_ref(), &route, &params)?;
            let infos: RawInfos =
                serde_json::from_value(value).map_err(|error| ArcgisError::BadShape {
                    route: route.clone(),
                    message: error.to_string(),
                })?;
            for info in infos.attachment_infos {
                listed.push((oid.clone(), info));
            }
        }
        Ok(listed)
    }
}

/// The log's account of one layer's attachments, decided by what was carried.
fn place_attachments(
    layer: &Layer,
    carried: AttachmentsCarried,
    placed: &mut Vec<(Item, Placed)>,
    conversions: &mut Vec<Conversion>,
) {
    let item = verdict::attachment_item(layer);
    if carried.carried > 0 {
        let destination = format!("attachments of {}", layer.name);
        placed.push((item, Placed::At(destination.clone())));
        if !carried.orphans.is_empty() {
            conversions.push(Conversion {
                location: layer.name.clone(),
                kind: ItemKind::EmbeddedResource,
                detail: format!(
                    "{} attachment{} carried",
                    carried.carried,
                    plural(carried.carried)
                ),
                destination: Some(destination),
                losses: carried.orphans,
            });
        }
    } else {
        placed.push((
            item,
            Placed::Left(if carried.orphans.is_empty() {
                format!(
                    "{} holds no attachments on the features that were written",
                    layer.name
                )
            } else {
                carried.orphans.join("; ")
            }),
        ));
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
