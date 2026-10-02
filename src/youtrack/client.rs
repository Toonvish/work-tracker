//! `YouTrackClient`: the real `YouTrackApi` over reqwest (blocking) plus pure URL builders.

use reqwest::Url;
use reqwest::header::HeaderValue;
use serde::de::DeserializeOwned;

use super::{
    ACTIVITY_PAGE_CAP, ACTIVITY_PAGE_SIZE, ActivityQuery, ISSUE_FIELDS, ISSUE_PAGE_CAP,
    ISSUE_PAGE_SIZE, YouTrackApi, YtActivity, YtIssue, YtProject,
};
use crate::config::YouTrackConfig;
use crate::error::ApiError;
use crate::http::{
    Listing, Page, YOUTRACK, build_client_for, check_response, decode, log_verbose, paginate,
};

/// Builds `<base>/api<path>`. `base` is validated by `YouTrackClient::new`.
fn api_url(base: &str, path: &str) -> Url {
    let s = format!("{}/api{}", base.trim_end_matches('/'), path);
    Url::parse(&s).unwrap_or_else(|e| panic!("invalid YouTrack URL {s:?}: {e}"))
}

/// `GET /api/admin/projects?fields=shortName,archived&$top=1000`
pub fn admin_projects_url(base: &str) -> Url {
    let mut url = api_url(base, "/admin/projects");
    url.query_pairs_mut()
        .append_pair("fields", "shortName,archived")
        .append_pair("$top", "1000");
    url
}

/// `GET /api/issues?query=..&fields=..&$top=..&$skip=..`
pub fn issues_search_url(base: &str, query: &str, top: usize, skip: usize) -> Url {
    let mut url = api_url(base, "/issues");
    url.query_pairs_mut()
        .append_pair("query", query)
        .append_pair("fields", ISSUE_FIELDS)
        .append_pair("$top", &top.to_string())
        .append_pair("$skip", &skip.to_string());
    url
}

/// `GET /api/issues/<id>?fields=..`
pub fn issue_url(base: &str, id: &str) -> Url {
    let mut url = api_url(base, "/issues");
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.push(id);
    }
    url.query_pairs_mut().append_pair("fields", ISSUE_FIELDS);
    url
}

/// `GET /api/activities?categories=..&author=me&start=..&end=..&reverse=true[&issueQuery=..]`
pub fn activities_url(base: &str, q: &ActivityQuery, top: usize, skip: usize) -> Url {
    let mut url = api_url(base, "/activities");
    {
        let mut p = url.query_pairs_mut();
        p.append_pair("categories", &q.categories.join(","))
            .append_pair("author", "me")
            .append_pair("start", &q.start_ms.to_string())
            .append_pair("end", &q.end_ms.to_string())
            .append_pair("reverse", "true");
        if let Some(iq) = &q.issue_query {
            p.append_pair("issueQuery", iq);
        }
        p.append_pair(
            "fields",
            "timestamp,category(id),target(idReadable,issue(idReadable))",
        )
        .append_pair("$top", &top.to_string())
        .append_pair("$skip", &skip.to_string());
    }
    url
}

pub struct YouTrackClient {
    base: String,
    token: HeaderValue,
    http: reqwest::blocking::Client,
    verbose: bool,
}

impl YouTrackClient {
    pub fn new(cfg: &YouTrackConfig, verbose: bool) -> Result<YouTrackClient, ApiError> {
        let base = cfg.url.trim_end_matches('/').to_string();
        if let Err(e) = Url::parse(&base) {
            return Err(ApiError::Other {
                service: YOUTRACK,
                message: format!("invalid [youtrack].url {:?}: {e}", cfg.url),
            });
        }
        let mut token = HeaderValue::from_str(&format!("Bearer {}", cfg.token)).map_err(|_| {
            ApiError::Other {
                service: YOUTRACK,
                message: "the YouTrack token contains characters that are not allowed in a header"
                    .to_string(),
            }
        })?;
        token.set_sensitive(true);
        Ok(YouTrackClient {
            base,
            token,
            http: build_client_for(YOUTRACK)?,
            verbose,
        })
    }

    /// One GET: send, read the body as text, check the status, decode.
    fn get<T: DeserializeOwned>(&self, url: Url, count: fn(&T) -> usize) -> Result<T, ApiError> {
        let network = |source| ApiError::Network {
            service: YOUTRACK,
            url: url.to_string(),
            source,
        };
        let resp = self
            .http
            .get(url.clone())
            .header(reqwest::header::AUTHORIZATION, self.token.clone())
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .map_err(network)?;
        let status = resp.status().as_u16();
        let body = resp.text().map_err(network)?;
        let bytes = body.len();
        let result = check_response(YOUTRACK, url.as_str(), status, body)
            .and_then(|b| decode::<T>(YOUTRACK, url.as_str(), &b));
        let n_items = result.as_ref().map_or(0, count);
        log_verbose(self.verbose, "GET", url.as_str(), status, n_items, bytes);
        result
    }

    fn get_page<T: DeserializeOwned>(&self, url: Url, top: usize) -> Result<Page<T>, ApiError> {
        let items = self.get::<Vec<T>>(url, Vec::len)?;
        let has_more = items.len() == top;
        Ok(Page { items, has_more })
    }
}

impl YouTrackApi for YouTrackClient {
    fn project_short_names(&self) -> Result<Vec<String>, ApiError> {
        let projects = self.get::<Vec<YtProject>>(admin_projects_url(&self.base), Vec::len)?;
        Ok(projects.into_iter().map(|p| p.short_name).collect())
    }

    fn search_issues(&self, query: &str) -> Result<Listing<YtIssue>, ApiError> {
        paginate(ISSUE_PAGE_CAP, |i| {
            let skip = i as usize * ISSUE_PAGE_SIZE;
            self.get_page(
                issues_search_url(&self.base, query, ISSUE_PAGE_SIZE, skip),
                ISSUE_PAGE_SIZE,
            )
        })
    }

    fn issue_by_id(&self, id: &str) -> Result<Option<YtIssue>, ApiError> {
        match self.get::<YtIssue>(issue_url(&self.base, id), |_| 1) {
            Ok(issue) => Ok(Some(issue)),
            Err(ApiError::NotFound { .. }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn my_activities(&self, q: &ActivityQuery) -> Result<Listing<YtActivity>, ApiError> {
        paginate(ACTIVITY_PAGE_CAP, |i| {
            let skip = i as usize * ACTIVITY_PAGE_SIZE;
            self.get_page(
                activities_url(&self.base, q, ACTIVITY_PAGE_SIZE, skip),
                ACTIVITY_PAGE_SIZE,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::youtrack::{CORE_CATEGORIES, FULL_CATEGORIES};
    use std::collections::HashMap;

    const BASE: &str = "https://youtrack.example.com";

    fn map(url: &Url) -> HashMap<String, String> {
        url.query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn aq(issue_query: Option<&str>) -> ActivityQuery {
        ActivityQuery {
            categories: FULL_CATEGORIES.to_vec(),
            start_ms: 1_790_000_000_000,
            end_ms: 1_790_100_000_000,
            issue_query: issue_query.map(str::to_string),
        }
    }

    #[test]
    fn admin_projects() {
        let u = admin_projects_url(BASE);
        assert_eq!(u.path(), "/api/admin/projects");
        let m = map(&u);
        assert_eq!(m["fields"], "shortName,archived");
        assert_eq!(m["$top"], "1000");
        assert_eq!(
            admin_projects_url("https://h/").path(),
            "/api/admin/projects"
        );
    }

    #[test]
    fn issues_search_pairs() {
        for i in 0..3usize {
            let u = issues_search_url(
                BASE,
                "assignee: me updated: 2026-09-27 .. 2026-10-02",
                100,
                i * 100,
            );
            assert_eq!(u.path(), "/api/issues");
            let m = map(&u);
            assert_eq!(m["query"], "assignee: me updated: 2026-09-27 .. 2026-10-02");
            assert_eq!(m["fields"], ISSUE_FIELDS);
            assert_eq!(m["$top"], "100");
            assert_eq!(m["$skip"], (i * 100).to_string());
        }
    }

    #[test]
    fn issue_by_id_url() {
        let u = issue_url(BASE, "SP-12");
        assert_eq!(u.path(), "/api/issues/SP-12");
        assert_eq!(map(&u)["fields"], ISSUE_FIELDS);
    }

    #[test]
    fn issue_url_encodes_odd_ids() {
        let u = issue_url(BASE, "a/b");
        assert_eq!(u.path(), "/api/issues/a%2Fb");
    }

    #[test]
    fn activities_pairs() {
        let u = activities_url(BASE, &aq(None), 200, 400);
        assert_eq!(u.path(), "/api/activities");
        let m = map(&u);
        assert_eq!(m["categories"], FULL_CATEGORIES.join(","));
        assert!(m["categories"].starts_with("CommentsCategory,CommentTextCategory,"));
        assert_eq!(m["author"], "me");
        assert_eq!(m["start"], "1790000000000");
        assert_eq!(m["end"], "1790100000000");
        assert_eq!(m["reverse"], "true");
        assert_eq!(
            m["fields"],
            "timestamp,category(id),target(idReadable,issue(idReadable))"
        );
        assert_eq!(m["$top"], "200");
        assert_eq!(m["$skip"], "400");
        assert!(!m.contains_key("issueQuery"));
    }

    #[test]
    fn activities_issue_query_only_when_set() {
        let u = activities_url(BASE, &aq(Some("project: SP")), 200, 0);
        assert_eq!(map(&u)["issueQuery"], "project: SP");
        let mut q = aq(None);
        q.categories = CORE_CATEGORIES.to_vec();
        let m = map(&activities_url(BASE, &q, 200, 0));
        assert_eq!(m["categories"].split(',').count(), 5);
        assert!(!m.contains_key("issueQuery"));
    }

    #[test]
    fn client_new_validates_token_and_url() {
        let cfg = YouTrackConfig {
            url: BASE.to_string(),
            token: "perm:abc".to_string(),
            projects: vec![],
            state_field: "State".to_string(),
        };
        assert!(YouTrackClient::new(&cfg, false).is_ok());
        let bad = YouTrackConfig {
            token: "bad\ntoken".to_string(),
            ..cfg.clone()
        };
        let e = YouTrackClient::new(&bad, false).err().unwrap();
        assert!(!e.to_string().contains("bad"));
        let bad_url = YouTrackConfig {
            url: "not a url".to_string(),
            ..cfg
        };
        assert!(YouTrackClient::new(&bad_url, false).is_err());
    }
}
