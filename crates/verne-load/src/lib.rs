//! Push a verne sidecar into a running ptolemy.
//!
//! A thin client, on purpose. Every request body is a struct out of
//! `verne_core::sidecar` serialised as it stands, so there is no place for the
//! sidecar's shape and ptolemy's to drift apart without the compiler noticing.
//! Two bodies are not: a subtype's `domain_assignments` and a relationship
//! class's two sides name ptolemy rows by id, and no id exists until the load
//! is running. Those are the only two swaps, and they are named as such.
//!
//! No GDAL: the sidecar is JSON, and loading one needs nothing that read it.
//!
//! # Order and authorisation
//!
//! Datasets first, then the domains and subtypes that hang off one, then the
//! relationship classes, which name two datasets and cannot be created before
//! both exist.
//!
//! ptolemy grants the creator of a dataset an admin row on it and enforces a
//! write ladder on every mutating route thereafter, so the loader has to create
//! the datasets itself: a loader pointed at somebody else's dataset would need
//! a grant it has no way to mint.

use std::collections::BTreeMap;

use serde::Serialize;
use verne_core::sidecar::{DatasetPlan, NewRelationship, NewSubtype, Sidecar};

/// Prefix every ptolemy route shares.
const API: &str = "/api/v1";

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
}

/// What a load created, keyed by the names the sidecar used.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    /// Dataset name to the id ptolemy gave it.
    pub datasets: BTreeMap<String, String>,
    /// `(dataset, domain)` to the id ptolemy gave that domain. A geodatabase
    /// domain two datasets use is two rows here, as it is two rows there.
    pub domains: BTreeMap<(String, String), String>,
    /// `(dataset, subtype)` to its id.
    pub subtypes: BTreeMap<(String, String), String>,
    /// Relationship class name to its id.
    pub relationships: BTreeMap<String, String>,
}

impl Loaded {
    pub fn sentence(&self) -> String {
        format!(
            "{} datasets, {} domains, {} subtypes, {} relationship classes.",
            self.datasets.len(),
            self.domains.len(),
            self.subtypes.len(),
            self.relationships.len()
        )
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
    /// There is no rollback: a load that fails part way leaves what it already
    /// created, and the error names the route that refused it. Undoing it is a
    /// matter for whoever holds the admin rows the load minted.
    pub fn load(&self, sidecar: &Sidecar) -> Result<Loaded, LoadError> {
        let mut loaded = Loaded::default();
        for plan in &sidecar.datasets {
            let id = self.create_dataset(plan)?;
            loaded
                .datasets
                .insert(plan.dataset.name.clone(), id.clone());
            self.create_domains(plan, &id, &mut loaded)?;
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
        Ok(loaded)
    }

    fn create_dataset(&self, plan: &DatasetPlan) -> Result<String, LoadError> {
        let route = format!("{API}/datasets");
        // the response is the whole dataset, not just its id
        let body = self.post(&route, &plan.dataset)?;
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

    fn post<T: Serialize>(&self, route: &str, body: &T) -> Result<serde_json::Value, LoadError> {
        let response = self
            .client
            .post(format!("{}{route}", self.base))
            .bearer_auth(&self.token)
            .json(body)
            .send()?;
        let status = response.status();
        let text = response.text()?;
        if !status.is_success() {
            return Err(LoadError::Refused {
                method: "POST",
                route: route.to_string(),
                status: status.as_u16(),
                body: text,
            });
        }
        serde_json::from_str(&text).map_err(|_| LoadError::NoId {
            route: route.to_string(),
            body: text,
        })
    }
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
