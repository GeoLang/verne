//! Push a verne sidecar into a running ptolemy.
//!
//! A thin client, on purpose. Every request body is a struct out of
//! `verne_core::sidecar` serialised as it stands, so there is no place for the
//! sidecar's shape and ptolemy's to drift apart without the compiler noticing.
//! Three bodies are not. A subtype's `domain_assignments` and a relationship
//! class's two sides name ptolemy rows by id, and no id exists until the load
//! is running. An attachment's bytes are a file beside the sidecar, and the
//! upload route wants them base64ed into the body. Those are the only three
//! places a body is built rather than sent, and they are named as such.
//!
//! No GDAL: the sidecar is JSON, the features are JSON lines beside it, and the
//! attachments are plain files. Loading needs nothing that read the source.
//!
//! # Order and authorisation
//!
//! Datasets first, then the domains and subtypes that hang off one, then the
//! relationship classes, which name two datasets and cannot be created before
//! both exist. Each dataset also gets a branch, because a dataset with no
//! branch cannot be committed to and its features have nowhere to go, and the
//! attachments come last: each hangs off a feature on a branch.
//!
//! ptolemy grants the creator of a dataset an admin row on it and enforces a
//! write ladder on every mutating route thereafter, so the loader has to create
//! the datasets itself: a loader pointed at somebody else's dataset would need
//! a grant it has no way to mint.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;

use serde::Serialize;
use verne_core::sidecar::{
    AttachmentOp, DatasetPlan, FeatureOp, MAX_FEATURE_BYTES, NewAttachment, NewRelationship,
    NewSchema, NewSubtype, Sidecar,
};

/// Prefix every ptolemy route shares.
const API: &str = "/api/v1";

/// The branch a load commits its features to. ptolemy creates no branch with a
/// dataset, so this is the first one the dataset has.
const BRANCH: &str = "main";

/// How many features go in one commit. The batch is also flushed when the next
/// feature would take it past [`MAX_FEATURE_BYTES`], which is what stops a
/// table of large polygons building a body ptolemy refuses; on a table of
/// points this is what keeps the number of commits down.
const BATCH_FEATURES: usize = 500;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("{0} is not a URL ptolemy could be at: {1}")]
    BadUrl(String, String),
    #[error("cannot reach ptolemy: {0}")]
    Unreachable(#[from] reqwest::Error),
    #[error("ptolemy refused {method} {route} with {status}: {body}")]
    Refused {
        method: &'static str,
        route: String,
        status: u16,
        body: String,
    },
    #[error("ptolemy answered {route} with {body}, which carries no id")]
    NoId { route: String, body: String },
    #[error(
        "subtype \"{subtype}\" assigns {field} the domain {domain}, which is not one of the domains on {dataset}"
    )]
    UnknownDomain {
        dataset: String,
        subtype: String,
        field: String,
        domain: String,
    },
    #[error(
        "relationship class {class} names the dataset {dataset}, which the sidecar does not create"
    )]
    UnknownDataset { class: String, dataset: String },
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} line {line} is not a feature: {message}")]
    BadFeature {
        path: String,
        line: usize,
        message: String,
    },
    #[error(
        "the attachment \"{name}\" belongs to the dataset {dataset}, which the sidecar does not create"
    )]
    UnknownAttachmentDataset { name: String, dataset: String },
    #[error("ptolemy answered {route} with {body}, which verne could not read")]
    BadAnswer { route: String, body: String },
    #[error(
        "ptolemy holds no dataset named {name}; an incremental load commits onto the datasets a full load created, and nothing here creates one"
    )]
    MissingDataset { name: String },
    #[error("the dataset {name} has no \"main\" branch, so the delta has nowhere to be committed")]
    MissingBranch { name: String },
}

/// What a load created, keyed by the names the sidecar used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    /// Dataset name to the id ptolemy gave it.
    pub datasets: BTreeMap<String, String>,
    /// Dataset name to how many fields its schema carries. A dataset whose
    /// source table had no fields gets no schema and is not in here.
    pub schemas: BTreeMap<String, usize>,
    /// `(dataset, domain)` to the id ptolemy gave that domain. A geodatabase
    /// domain two datasets use is two rows here, as it is two rows there.
    pub domains: BTreeMap<(String, String), String>,
    /// `(dataset, subtype)` to its id.
    pub subtypes: BTreeMap<(String, String), String>,
    /// Relationship class name to its id.
    pub relationships: BTreeMap<String, String>,
    /// Dataset name to the id of the branch its features were committed to.
    pub branches: BTreeMap<String, String>,
    /// Dataset name to how many features were committed, and in how many
    /// commits. A dataset whose table was empty is not in here.
    pub features: BTreeMap<String, Committed>,
    /// Attachment name to its id, one per blob that reached its feature.
    pub attachments: BTreeMap<String, String>,
    /// What the attachment operations came to, which is only interesting on a
    /// delta: a full load's are all uploads and are in `attachments`.
    pub attachment_ops: AttachmentOps,
}

/// What a sidecar's attachment operations came to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttachmentOps {
    pub added: usize,
    pub replaced: usize,
    pub deleted: usize,
    /// The operations whose loaded copy the loader could not put a finger on,
    /// each with the reason and what it did instead. One name matching two
    /// attachments on a feature is refused rather than applied to whichever came
    /// first, and one matching none has nothing to delete. The caller prints
    /// these.
    pub unmatched: Vec<String>,
}

/// What one dataset's features cost to load.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Committed {
    pub features: usize,
    pub commits: usize,
}

impl Loaded {
    pub fn sentence(&self) -> String {
        format!(
            "{} datasets, {} schemas, {} domains, {} subtypes, {} relationship classes, \
             {} features in {} commits, {} attachments.",
            self.datasets.len(),
            self.schemas.len(),
            self.domains.len(),
            self.subtypes.len(),
            self.relationships.len(),
            self.features
                .values()
                .map(|held| held.features)
                .sum::<usize>(),
            self.features
                .values()
                .map(|held| held.commits)
                .sum::<usize>(),
            self.attachments.len()
        )
    }

    /// How many field aliases the load carried, which is the part of a schema
    /// with nowhere else in the platform to go.
    pub fn aliases(sidecar: &Sidecar) -> usize {
        sidecar
            .datasets
            .iter()
            .flat_map(|plan| plan.schema.fields.iter())
            .filter(|field| field.alias.is_some())
            .count()
    }
}

pub struct Loader {
    client: reqwest::blocking::Client,
    /// The base URL with no trailing slash, so a route can be appended as-is.
    base: String,
    token: String,
}

impl Loader {
    /// `base` is the root ptolemy is served at, such as `http://localhost:3000`.
    /// The token is a bearer token with a role that may write.
    pub fn new(base: &str, token: &str) -> Result<Self, LoadError> {
        let base = base.trim_end_matches('/').to_string();
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(LoadError::BadUrl(
                base,
                "expected http:// or https://".into(),
            ));
        }
        let client = reqwest::blocking::Client::builder()
            .build()
            .map_err(LoadError::Unreachable)?;
        Ok(Loader {
            client,
            base,
            token: token.to_string(),
        })
    }

    /// Create everything in the sidecar, in the order ptolemy needs it.
    ///
    /// `directory` is the extraction the sidecar came out of: the feature files
    /// and the attachment blobs are named relative to it.
    ///
    /// There is no rollback: a load that fails part way leaves what it already
    /// created, and the error names the route that refused it. Undoing it is a
    /// matter for whoever holds the admin rows the load minted.
    pub fn load(&self, sidecar: &Sidecar, directory: &Path) -> Result<Loaded, LoadError> {
        if sidecar.incremental {
            return self.load_incremental(sidecar, directory);
        }
        let mut loaded = Loaded::default();
        for plan in &sidecar.datasets {
            let id = self.create_dataset(plan)?;
            loaded
                .datasets
                .insert(plan.dataset.name.clone(), id.clone());
            // the schema is about the dataset itself and waits on nothing, so
            // it goes on before the domains that hang off the same dataset,
            // and before the features, which ptolemy validates against it
            if self.set_schema(&id, &plan.schema)? {
                loaded
                    .schemas
                    .insert(plan.dataset.name.clone(), plan.schema.fields.len());
            }
            self.create_domains(plan, &id, &mut loaded)?;
            let branch = self.create_branch(plan, &id)?;
            loaded
                .branches
                .insert(plan.dataset.name.clone(), branch.clone());
            let committed = self.commit_features(plan, &branch, directory)?;
            if committed.features > 0 {
                loaded.features.insert(plan.dataset.name.clone(), committed);
            }
        }
        // subtypes only after every domain, because a subtype names one by id
        for plan in &sidecar.datasets {
            let id = &loaded.datasets[&plan.dataset.name];
            for subtype in &plan.subtypes {
                let created = self.create_subtype(plan, id, subtype, &loaded)?;
                loaded
                    .subtypes
                    .insert((plan.dataset.name.clone(), subtype.name.clone()), created);
            }
        }
        for class in &sidecar.relationships {
            let id = self.create_relationship(class, &loaded)?;
            loaded.relationships.insert(class.name.clone(), id);
        }
        self.apply_attachments(sidecar, directory, &mut loaded)?;
        Ok(loaded)
    }

    /// Commit a delta onto what a full load created. Nothing is created here:
    /// the datasets, schemas, domains, subtypes and relationship classes were
    /// made by the first load, and making them again would either collide or
    /// fork a second copy of the data. Each dataset is found by the name the
    /// sidecar gives it and the ops go onto its "main" branch, which is the one
    /// [`Self::load`] created.
    ///
    /// The attachments are the exception: a delta carries the ones the source
    /// says were added, replaced or deleted, and those are applied after the
    /// features, because an attachment added in the same window as the feature
    /// it hangs off has nowhere to go until that feature is committed.
    fn load_incremental(&self, sidecar: &Sidecar, directory: &Path) -> Result<Loaded, LoadError> {
        let mut loaded = Loaded::default();
        let held = self.get(&format!("{API}/datasets"))?;
        for plan in &sidecar.datasets {
            let dataset =
                named_id(&held, &plan.dataset.name).ok_or_else(|| LoadError::MissingDataset {
                    name: plan.dataset.name.clone(),
                })?;
            let branches = self.get(&format!("{API}/datasets/{dataset}/branches"))?;
            let branch = named_id(&branches, BRANCH).ok_or_else(|| LoadError::MissingBranch {
                name: plan.dataset.name.clone(),
            })?;
            loaded
                .datasets
                .insert(plan.dataset.name.clone(), dataset.clone());
            loaded
                .branches
                .insert(plan.dataset.name.clone(), branch.clone());
            let committed = self.commit_features(plan, &branch, directory)?;
            if committed.features > 0 {
                loaded.features.insert(plan.dataset.name.clone(), committed);
            }
        }
        self.apply_attachments(sidecar, directory, &mut loaded)?;
        Ok(loaded)
    }

    /// The sidecar's attachment operations, in the order it holds them.
    ///
    /// An add is an upload. ptolemy has no route that changes an attachment, so
    /// a replacement is the old one deleted and the new bytes uploaded, in that
    /// order: the other order would leave two attachments of the same name on
    /// the feature and no way to tell which the next delta means. Both the
    /// replacement and the delete have to find the loaded copy first, and the
    /// only handle either has on it is its name, because ptolemy minted the id
    /// and no extraction ever saw it. Two attachments of one name on one
    /// feature is a match the loader will not pick between, so that operation
    /// is refused and named.
    fn apply_attachments(
        &self,
        sidecar: &Sidecar,
        directory: &Path,
        loaded: &mut Loaded,
    ) -> Result<(), LoadError> {
        for op in &sidecar.attachments {
            match op {
                AttachmentOp::Add(attachment) => {
                    let branch = self.attachment_branch(loaded, op)?;
                    let id = self.upload_attachment(attachment, &branch, directory)?;
                    loaded.attachments.insert(attachment.name.clone(), id);
                    loaded.attachment_ops.added += 1;
                }
                AttachmentOp::Update(attachment) => {
                    let branch = self.attachment_branch(loaded, op)?;
                    match self.held_attachment(&branch, &attachment.feature_id, &attachment.name)? {
                        Held::One(id) => self.delete(&format!("{API}/attachments/{id}"))?,
                        // the bytes still go up: the source says this
                        // attachment is on the feature, and it is
                        Held::None => loaded.attachment_ops.unmatched.push(format!(
                            "no attachment named {} on the feature {}, so the new bytes went up as a first copy rather than replacing one",
                            attachment.name, attachment.feature_id
                        )),
                        Held::Many(count) => {
                            loaded.attachment_ops.unmatched.push(refuse(
                                "replaced",
                                &attachment.name,
                                &attachment.feature_id,
                                count,
                            ));
                            continue;
                        }
                    }
                    let id = self.upload_attachment(attachment, &branch, directory)?;
                    loaded.attachments.insert(attachment.name.clone(), id);
                    loaded.attachment_ops.replaced += 1;
                }
                AttachmentOp::Delete(delete) => {
                    let branch = self.attachment_branch(loaded, op)?;
                    match self.held_attachment(&branch, &delete.feature_id, &delete.name)? {
                        Held::One(id) => {
                            self.delete(&format!("{API}/attachments/{id}"))?;
                            loaded.attachment_ops.deleted += 1;
                        }
                        Held::None => loaded.attachment_ops.unmatched.push(format!(
                            "no attachment named {} on the feature {}, so there was nothing to delete",
                            delete.name, delete.feature_id
                        )),
                        Held::Many(count) => loaded.attachment_ops.unmatched.push(refuse(
                            "deleted",
                            &delete.name,
                            &delete.feature_id,
                            count,
                        )),
                    }
                }
            }
        }
        Ok(())
    }

    /// The branch the operation's dataset was loaded onto.
    fn attachment_branch(&self, loaded: &Loaded, op: &AttachmentOp) -> Result<String, LoadError> {
        loaded.branches.get(op.dataset()).cloned().ok_or_else(|| {
            LoadError::UnknownAttachmentDataset {
                name: op.name().to_string(),
                dataset: op.dataset().to_string(),
            }
        })
    }

    /// Which attachment of `feature` is the one called `name`, out of what
    /// ptolemy lists on it.
    fn held_attachment(&self, branch: &str, feature: &str, name: &str) -> Result<Held, LoadError> {
        let route = format!("{API}/branches/{branch}/features/{feature}/attachments");
        let listed = self.get(&route)?;
        let matching: Vec<String> = listed
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|held| held.get("name").and_then(serde_json::Value::as_str) == Some(name))
            .filter_map(|held| {
                held.get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        Ok(match matching.len() {
            0 => Held::None,
            1 => Held::One(matching[0].clone()),
            count => Held::Many(count),
        })
    }

    fn create_dataset(&self, plan: &DatasetPlan) -> Result<String, LoadError> {
        let route = format!("{API}/datasets");
        // the response is the whole dataset, not just its id
        let body = self.post(&route, &plan.dataset)?;
        id_of(&route, &body)
    }

    /// The dataset's columns, aliases included. Answers whether one was sent: a
    /// table with no fields has no schema to set, and an empty one would only
    /// record that ptolemy should validate nothing.
    fn set_schema(&self, dataset_id: &str, schema: &NewSchema) -> Result<bool, LoadError> {
        if schema.is_empty() {
            return Ok(false);
        }
        self.put(&format!("{API}/datasets/{dataset_id}/schema"), schema)?;
        Ok(true)
    }

    /// The dataset's first branch. ptolemy creates none with a dataset, and a
    /// dataset with no branch has nowhere to hold a feature.
    fn create_branch(&self, plan: &DatasetPlan, dataset_id: &str) -> Result<String, LoadError> {
        let route = format!("{API}/datasets/{dataset_id}/branches");
        let body = self.post(
            &route,
            &BranchBody {
                name: BRANCH,
                created_by: &plan.dataset.created_by,
            },
        )?;
        id_of(&route, &body)
    }

    /// The dataset's features, in batches. Read a line at a time rather than
    /// held in memory: a real geodatabase table is bigger than the process
    /// should be, and the file is written one insert operation per line so
    /// that reading it that way is possible.
    fn commit_features(
        &self,
        plan: &DatasetPlan,
        branch_id: &str,
        directory: &Path,
    ) -> Result<Committed, LoadError> {
        let Some(relative) = &plan.features else {
            return Ok(Committed::default());
        };
        let path = directory.join(relative);
        let file = std::fs::File::open(&path).map_err(|source| LoadError::Read {
            path: path.display().to_string(),
            source,
        })?;

        let mut committed = Committed::default();
        let mut batch: Vec<FeatureOp> = Vec::new();
        let mut bytes = 0usize;
        for (index, line) in std::io::BufReader::new(file).lines().enumerate() {
            let line = line.map_err(|source| LoadError::Read {
                path: path.display().to_string(),
                source,
            })?;
            if line.trim().is_empty() {
                continue;
            }
            let feature: FeatureOp =
                serde_json::from_str(&line).map_err(|error| LoadError::BadFeature {
                    path: relative.clone(),
                    line: index + 1,
                    message: error.to_string(),
                })?;
            // flushed before the feature goes in, not after: a batch that is
            // already at the limit and then takes one more line is the body
            // ptolemy refuses
            if !batch.is_empty()
                && (batch.len() >= BATCH_FEATURES || bytes + line.len() > MAX_FEATURE_BYTES)
            {
                self.commit(plan, branch_id, &batch)?;
                committed.features += batch.len();
                committed.commits += 1;
                batch.clear();
                bytes = 0;
            }
            bytes += line.len();
            batch.push(feature);
        }
        if !batch.is_empty() {
            self.commit(plan, branch_id, &batch)?;
            committed.features += batch.len();
            committed.commits += 1;
        }
        Ok(committed)
    }

    fn commit(
        &self,
        plan: &DatasetPlan,
        branch_id: &str,
        features: &[FeatureOp],
    ) -> Result<(), LoadError> {
        self.post(
            &format!("{API}/branches/{branch_id}/commit"),
            &CommitBody {
                message: &format!(
                    "verne: {} operation{} of {}",
                    features.len(),
                    if features.len() == 1 { "" } else { "s" },
                    plan.source_table
                ),
                author: &plan.dataset.created_by,
                operations: features,
            },
        )
        .map(drop)
    }

    /// One blob, base64 into the upload route. The only body built out of a
    /// file rather than out of the sidecar: the bytes are too big to keep in
    /// a document meant to be read.
    fn upload_attachment(
        &self,
        attachment: &NewAttachment,
        branch: &str,
        directory: &Path,
    ) -> Result<String, LoadError> {
        let path = directory.join(&attachment.file);
        let bytes = std::fs::read(&path).map_err(|source| LoadError::Read {
            path: path.display().to_string(),
            source,
        })?;
        let route = format!(
            "{API}/branches/{branch}/features/{}/attachments",
            attachment.feature_id
        );
        let body = self.post(
            &route,
            &UploadBody {
                name: &attachment.name,
                content_type: attachment.content_type.as_deref(),
                data: base64(&bytes),
                metadata: &attachment.metadata,
                created_by: &attachment.created_by,
            },
        )?;
        id_of(&route, &body)
    }

    fn create_domains(
        &self,
        plan: &DatasetPlan,
        dataset_id: &str,
        loaded: &mut Loaded,
    ) -> Result<(), LoadError> {
        for domain in &plan.domains {
            let route = format!("{API}/datasets/{dataset_id}/domains");
            let body = self.post(&route, domain)?;
            loaded.domains.insert(
                (plan.dataset.name.clone(), domain.name.clone()),
                id_of(&route, &body)?,
            );
        }
        Ok(())
    }

    fn create_subtype(
        &self,
        plan: &DatasetPlan,
        dataset_id: &str,
        subtype: &NewSubtype,
        loaded: &Loaded,
    ) -> Result<String, LoadError> {
        let mut assignments = serde_json::Map::new();
        for (field, domain) in &subtype.domain_assignments {
            let key = (plan.dataset.name.clone(), domain.clone());
            let id = loaded
                .domains
                .get(&key)
                .ok_or_else(|| LoadError::UnknownDomain {
                    dataset: plan.dataset.name.clone(),
                    subtype: subtype.name.clone(),
                    field: field.clone(),
                    domain: domain.clone(),
                })?;
            assignments.insert(field.clone(), serde_json::Value::String(id.clone()));
        }
        let route = format!("{API}/datasets/{dataset_id}/subtypes");
        let body = self.post(
            &route,
            &SubtypeBody {
                subtype_field: &subtype.subtype_field,
                name: &subtype.name,
                code: subtype.code,
                default_values: &subtype.default_values,
                domain_assignments: assignments,
            },
        )?;
        id_of(&route, &body)
    }

    fn create_relationship(
        &self,
        class: &NewRelationship,
        loaded: &Loaded,
    ) -> Result<String, LoadError> {
        let origin = dataset_id(loaded, class, &class.origin_dataset)?;
        let destination = dataset_id(loaded, class, &class.destination_dataset)?;
        // the class names both sides in the body; the dataset in the path is
        // ignored, and the origin is the one the caller is answerable for
        let route = format!("{API}/datasets/{origin}/relationships");
        let body = self.post(
            &route,
            &RelationshipBody {
                name: &class.name,
                origin_dataset_id: origin,
                destination_dataset_id: destination,
                origin_foreign_key: &class.origin_foreign_key,
                cardinality: &class.cardinality,
                forward_label: &class.forward_label,
                backward_label: &class.backward_label,
            },
        )?;
        id_of(&route, &body)
    }

    /// A DELETE, which only an attachment operation makes: ptolemy has no route
    /// that changes an attachment, so replacing one is deleting it and
    /// uploading the new bytes.
    fn delete(&self, route: &str) -> Result<(), LoadError> {
        let url = format!("{}{route}", self.base);
        let response = self.client.delete(url).bearer_auth(&self.token).send()?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        Err(LoadError::Refused {
            method: "DELETE",
            route: route.to_string(),
            status: status.as_u16(),
            body: response.text()?,
        })
    }

    fn post<T: Serialize>(&self, route: &str, body: &T) -> Result<serde_json::Value, LoadError> {
        let text = self.send(Method::Post, route, body)?;
        serde_json::from_str(&text).map_err(|_| LoadError::NoId {
            route: route.to_string(),
            body: text,
        })
    }

    /// A PUT answers with a status and nothing else, so unlike [`Self::post`]
    /// there is no body to read an id out of.
    fn put<T: Serialize>(&self, route: &str, body: &T) -> Result<(), LoadError> {
        self.send(Method::Put, route, body).map(drop)
    }

    /// A read, which only the incremental path makes: a full load creates
    /// everything itself and never has to ask what is already there.
    fn get(&self, route: &str) -> Result<serde_json::Value, LoadError> {
        let url = format!("{}{route}", self.base);
        let response = self.client.get(url).bearer_auth(&self.token).send()?;
        let status = response.status();
        let text = response.text()?;
        if !status.is_success() {
            return Err(LoadError::Refused {
                method: "GET",
                route: route.to_string(),
                status: status.as_u16(),
                body: text,
            });
        }
        serde_json::from_str(&text).map_err(|_| LoadError::BadAnswer {
            route: route.to_string(),
            body: text,
        })
    }

    fn send<T: Serialize>(
        &self,
        method: Method,
        route: &str,
        body: &T,
    ) -> Result<String, LoadError> {
        let url = format!("{}{route}", self.base);
        let request = match method {
            Method::Post => self.client.post(url),
            Method::Put => self.client.put(url),
        };
        let response = request.bearer_auth(&self.token).json(body).send()?;
        let status = response.status();
        let text = response.text()?;
        if status.is_success() {
            return Ok(text);
        }
        Err(LoadError::Refused {
            method: method.name(),
            route: route.to_string(),
            status: status.as_u16(),
            body: text,
        })
    }
}

/// Which loaded attachment an operation is about, out of the ones ptolemy lists
/// on the feature under that name.
enum Held {
    None,
    One(String),
    Many(usize),
}

/// Why an operation the loader would have had to guess at was not applied.
fn refuse(verb: &str, name: &str, feature: &str, count: usize) -> String {
    format!(
        "{count} attachments on the feature {feature} are called {name}, so which of them the source {verb} cannot be told and none was touched"
    )
}

#[derive(Clone, Copy)]
enum Method {
    Post,
    Put,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Method::Post => "POST",
            Method::Put => "PUT",
        }
    }
}

/// The id of the row called `name` in a listed array, `None` when the answer
/// is not an array or nothing in it carries that name.
fn named_id(listed: &serde_json::Value, name: &str) -> Option<String> {
    listed.as_array()?.iter().find_map(|row| {
        (row.get("name")?.as_str()? == name)
            .then(|| row.get("id")?.as_str().map(str::to_string))
            .flatten()
    })
}

fn id_of(route: &str, body: &serde_json::Value) -> Result<String, LoadError> {
    body.get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| LoadError::NoId {
            route: route.to_string(),
            body: body.to_string(),
        })
}

fn dataset_id<'a>(
    loaded: &'a Loaded,
    class: &NewRelationship,
    name: &str,
) -> Result<&'a str, LoadError> {
    loaded
        .datasets
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| LoadError::UnknownDataset {
            class: class.name.clone(),
            dataset: name.to_string(),
        })
}

/// `POST /api/v1/datasets/{id}/subtypes`: the sidecar's subtype with the domain
/// names swapped for the ids their domains came back with. The one place a
/// sidecar field is not posted as it stands.
#[derive(Serialize)]
struct SubtypeBody<'a> {
    subtype_field: &'a str,
    name: &'a str,
    code: i32,
    default_values: &'a serde_json::Map<String, serde_json::Value>,
    domain_assignments: serde_json::Map<String, serde_json::Value>,
}

fn base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// `POST /api/v1/datasets/{id}/branches`.
#[derive(Serialize)]
struct BranchBody<'a> {
    name: &'a str,
    created_by: &'a str,
}

/// `POST /api/v1/branches/{id}/commit`. The operations are the sidecar's
/// feature lines put in an array: each one already carries the `insert`,
/// `update` or `delete` tag ptolemy's `DiffOpRequest` is read by, so nothing
/// here rewrites them.
#[derive(Serialize)]
struct CommitBody<'a> {
    message: &'a str,
    author: &'a str,
    operations: &'a [FeatureOp],
}

/// `POST /api/v1/branches/{branch}/features/{feature}/attachments`: the
/// sidecar's attachment with the file read and base64ed into `data`.
#[derive(Serialize)]
struct UploadBody<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content_type: Option<&'a str>,
    data: String,
    metadata: &'a serde_json::Value,
    created_by: &'a str,
}

/// `POST /api/v1/datasets/{id}/relationships`: the sidecar's class with each
/// side's dataset name swapped for the id it was created under.
#[derive(Serialize)]
struct RelationshipBody<'a> {
    name: &'a str,
    origin_dataset_id: &'a str,
    destination_dataset_id: &'a str,
    origin_foreign_key: &'a str,
    cardinality: &'a str,
    forward_label: &'a str,
    backward_label: &'a str,
}
