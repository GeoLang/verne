//! What feature services a portal holds, so an operator can pick one instead of
//! hunting for a FeatureServer URL to paste.
//!
//! The portal's own search route answers this, and it is the sharing API rather
//! than the service API: it hangs off `sharing/rest` and not off any one
//! service. Reading it is a read like every other one in this crate, and the
//! same credentials ride on it, which is what makes a private org's services
//! visible here at all.

use crate::{ArcgisError, Fetch, client};

/// One feature service the portal's search named. `url` is the FeatureServer
/// route, which is the whole point: it goes straight into an inspect.
pub struct PortalService {
    pub title: String,
    pub owner: String,
    pub url: String,
}

/// The most results the search route will put in one response, per its
/// reference. Asking for more is not an error, it is silently the same 100.
const PAGE: i64 = 100;

/// Every feature service the portal will admit to, optionally narrowed to one
/// owner.
///
/// The walk is bounded by the API rather than by the org: `total` is counted
/// accurately only to 10,000 and pagination stops there, so a huge org's
/// listing is the API's first ten thousand services and not all of them.
pub fn feature_services(
    fetch: &dyn Fetch,
    portal: &str,
    owner: Option<&str>,
) -> Result<Vec<PortalService>, ArcgisError> {
    let route = format!("{}/search", sharing_rest(portal)?);
    let query = match owner {
        Some(owner) => format!(r#"type:"Feature Service" AND owner:"{owner}""#),
        None => r#"type:"Feature Service""#.to_string(),
    };
    let mut found = Vec::new();
    // 1-based, unlike every other offset in this crate
    let mut start = 1;
    loop {
        let page = client::json(
            fetch,
            &route,
            &[
                ("q", query.clone()),
                ("num", PAGE.to_string()),
                ("start", start.to_string()),
            ],
        )?;
        let results = page
            .get("results")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| ArcgisError::BadShape {
                route: route.clone(),
                message: "no results array, and an empty listing must not be guessed at".into(),
            })?;
        for item in results {
            // a portal item with no url is registered content the portal cannot
            // hand out a route to, and there is nothing to inspect
            let Some(url) = item.get("url").and_then(serde_json::Value::as_str) else {
                continue;
            };
            found.push(PortalService {
                title: string(item, "title"),
                owner: string(item, "owner"),
                url: url.to_string(),
            });
        }
        // arcgis.com sends -1 when the results run out and the reference says
        // the field is simply absent; either ends the walk, and so does a
        // nextStart that does not advance, which would otherwise page forever
        match page.get("nextStart").and_then(serde_json::Value::as_i64) {
            Some(next) if next > start => start = next,
            _ => return Ok(found),
        }
    }
}

fn string(item: &serde_json::Value, name: &str) -> String {
    item.get(name)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The portal's `sharing/rest` root. An operator has either the portal's home
/// or its REST root to hand, and appending the suffix to a URL that already
/// carries it asks for a route no portal serves.
fn sharing_rest(portal: &str) -> Result<String, ArcgisError> {
    let trimmed = portal.trim_end_matches('/');
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err(ArcgisError::BadUrl(
            portal.to_string(),
            "expected a portal URL starting with http:// or https://, such as \
             https://www.arcgis.com"
                .into(),
        ));
    }
    if trimmed.ends_with("/sharing/rest") {
        return Ok(trimmed.to_string());
    }
    Ok(format!("{trimmed}/sharing/rest"))
}

#[cfg(test)]
mod tests {
    use super::sharing_rest;

    #[test]
    fn a_portal_home_gets_the_rest_root_appended() {
        let root = sharing_rest("https://www.arcgis.com/").expect("accepted");
        assert_eq!(root, "https://www.arcgis.com/sharing/rest");
    }

    #[test]
    fn a_rest_root_is_not_given_a_second_one() {
        let root = sharing_rest("https://host/portal/sharing/rest").expect("accepted");
        assert_eq!(root, "https://host/portal/sharing/rest");
    }

    #[test]
    fn a_portal_without_a_scheme_is_refused() {
        assert!(sharing_rest("www.arcgis.com").is_err());
    }
}
