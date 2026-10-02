//! `GitLabClient`: the real `GitLabApi` over reqwest (blocking) plus pure URL builders.

use chrono::{NaiveDate, SecondsFormat};
use reqwest::Url;
use reqwest::header::HeaderValue;
use serde::de::DeserializeOwned;

use super::{
    EVENT_PAGE_CAP, GitLabApi, GlEvent, GlMergeRequest, GlUser, MR_PAGE_CAP, MrQuery, PER_PAGE,
};
use crate::config::GitLabConfig;
use crate::error::ApiError;
use crate::http::{
    GITLAB, Listing, Page, build_client_for, check_response, decode, gitlab_has_more, log_verbose,
    paginate,
};

/// Builds `<base>/api/v4<path>`. `base` is validated by `GitLabClient::new`.
fn api_url(base: &str, path: &str) -> Url {
    let s = format!("{}/api/v4{}", base.trim_end_matches('/'), path);
    Url::parse(&s).unwrap_or_else(|e| panic!("invalid GitLab URL {s:?}: {e}"))
}

/// `GET /user`
pub fn user_url(base: &str) -> Url {
    api_url(base, "/user")
}

/// `GET /users?username=<u>`
pub fn users_by_username_url(base: &str, username: &str) -> Url {
    let mut url = api_url(base, "/users");
    url.query_pairs_mut().append_pair("username", username);
    url
}

/// `GET /merge_requests?scope=all&state=all&author_id|reviewer_id=..&updated_after=..`
pub fn mr_list_url(base: &str, q: &MrQuery, page: u32) -> Url {
    let mut url = api_url(base, "/merge_requests");
    {
        let mut p = url.query_pairs_mut();
        p.append_pair("scope", "all").append_pair("state", "all");
        if let Some(id) = q.author_id {
            p.append_pair("author_id", &id.to_string());
        }
        if let Some(id) = q.reviewer_id {
            p.append_pair("reviewer_id", &id.to_string());
        }
        p.append_pair(
            "updated_after",
            &q.updated_after.to_rfc3339_opts(SecondsFormat::Secs, true),
        )
        .append_pair("order_by", "updated_at")
        .append_pair("sort", "desc")
        .append_pair("per_page", &PER_PAGE.to_string())
        .append_pair("page", &page.to_string());
    }
    url
}

/// `GET /users/{uid}/events?after=..&before=..&sort=desc`
pub fn events_url(base: &str, uid: u64, after: NaiveDate, before: NaiveDate, page: u32) -> Url {
    let mut url = api_url(base, &format!("/users/{uid}/events"));
    url.query_pairs_mut()
        .append_pair("after", &after.format("%Y-%m-%d").to_string())
        .append_pair("before", &before.format("%Y-%m-%d").to_string())
        .append_pair("sort", "desc")
        .append_pair("per_page", &PER_PAGE.to_string())
        .append_pair("page", &page.to_string());
    url
}

/// `GET /projects/{pid}/merge_requests?state=all&iids[]=1&iids[]=2`
pub fn project_mrs_url(base: &str, pid: u64, iids: &[u64]) -> Url {
    let mut url = api_url(base, &format!("/projects/{pid}/merge_requests"));
    {
        let mut p = url.query_pairs_mut();
        p.append_pair("state", "all")
            .append_pair("per_page", &PER_PAGE.to_string());
        for iid in iids {
            p.append_pair("iids[]", &iid.to_string());
        }
    }
    url
}

pub struct GitLabClient {
    base: String,
    token: HeaderValue,
    http: reqwest::blocking::Client,
    verbose: bool,
}

impl GitLabClient {
    pub fn new(cfg: &GitLabConfig, verbose: bool) -> Result<GitLabClient, ApiError> {
        let base = cfg.url.trim_end_matches('/').to_string();
        if let Err(e) = Url::parse(&base) {
            return Err(ApiError::Other {
                service: GITLAB,
                message: format!("invalid [gitlab].url {:?}: {e}", cfg.url),
            });
        }
        let mut token = HeaderValue::from_str(&cfg.token).map_err(|_| ApiError::Other {
            service: GITLAB,
            message: "the GitLab token contains characters that are not allowed in a header"
                .to_string(),
        })?;
        token.set_sensitive(true);
        Ok(GitLabClient {
            base,
            token,
            http: build_client_for(GITLAB)?,
            verbose,
        })
    }

    /// One GET: send, read the body as text, check the status, decode.
    /// Returns the decoded value and the `X-Next-Page` header (if any).
    fn get<T: DeserializeOwned>(
        &self,
        url: Url,
        count: fn(&T) -> usize,
    ) -> Result<(T, Option<String>), ApiError> {
        let network = |source| ApiError::Network {
            service: GITLAB,
            url: url.to_string(),
            source,
        };
        let resp = self
            .http
            .get(url.clone())
            .header("PRIVATE-TOKEN", self.token.clone())
            .send()
            .map_err(network)?;
        let status = resp.status().as_u16();
        let next = resp
            .headers()
            .get("x-next-page")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let body = resp.text().map_err(network)?;
        let bytes = body.len();
        let result = check_response(GITLAB, url.as_str(), status, body)
            .and_then(|b| decode::<T>(GITLAB, url.as_str(), &b));
        let n_items = result.as_ref().map_or(0, count);
        log_verbose(self.verbose, "GET", url.as_str(), status, n_items, bytes);
        result.map(|v| (v, next))
    }

    fn get_page<T: DeserializeOwned>(&self, url: Url) -> Result<Page<T>, ApiError> {
        let (items, next) = self.get::<Vec<T>>(url, Vec::len)?;
        let has_more = gitlab_has_more(next.as_deref(), items.len(), PER_PAGE);
        Ok(Page { items, has_more })
    }
}

impl GitLabApi for GitLabClient {
    fn current_user(&self) -> Result<GlUser, ApiError> {
        self.get::<GlUser>(user_url(&self.base), |_| 1)
            .map(|(u, _)| u)
    }

    fn users_by_username(&self, username: &str) -> Result<Vec<GlUser>, ApiError> {
        self.get::<Vec<GlUser>>(users_by_username_url(&self.base, username), Vec::len)
            .map(|(u, _)| u)
    }

    fn merge_requests(&self, q: &MrQuery) -> Result<Listing<GlMergeRequest>, ApiError> {
        paginate(MR_PAGE_CAP, |i| {
            self.get_page(mr_list_url(&self.base, q, i + 1))
        })
    }

    fn user_events(
        &self,
        user_id: u64,
        after: NaiveDate,
        before: NaiveDate,
    ) -> Result<Listing<GlEvent>, ApiError> {
        paginate(EVENT_PAGE_CAP, |i| {
            self.get_page(events_url(&self.base, user_id, after, before, i + 1))
        })
    }

    fn project_merge_requests(
        &self,
        project_id: u64,
        iids: &[u64],
    ) -> Result<Vec<GlMergeRequest>, ApiError> {
        self.get::<Vec<GlMergeRequest>>(project_mrs_url(&self.base, project_id, iids), Vec::len)
            .map(|(v, _)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use std::collections::HashMap;

    const BASE: &str = "https://gitlab.example.com";

    fn pairs(url: &Url) -> Vec<(String, String)> {
        url.query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    }

    fn map(url: &Url) -> HashMap<String, String> {
        pairs(url).into_iter().collect()
    }

    fn after() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-29T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn user_urls() {
        assert_eq!(
            user_url(BASE).as_str(),
            "https://gitlab.example.com/api/v4/user"
        );
        assert_eq!(
            user_url("https://gitlab.example.com/").path(),
            "/api/v4/user"
        );
        let u = users_by_username_url(BASE, "eric x");
        assert_eq!(u.path(), "/api/v4/users");
        assert_eq!(map(&u)["username"], "eric x");
    }

    #[test]
    fn mr_list_authored() {
        let q = MrQuery {
            author_id: Some(7),
            reviewer_id: None,
            updated_after: after(),
        };
        let u = mr_list_url(BASE, &q, 3);
        assert_eq!(u.path(), "/api/v4/merge_requests");
        let m = map(&u);
        assert_eq!(m["scope"], "all");
        assert_eq!(m["state"], "all");
        assert_eq!(m["author_id"], "7");
        assert!(!m.contains_key("reviewer_id"));
        assert_eq!(m["updated_after"], "2026-09-29T08:00:00Z");
        assert_eq!(m["order_by"], "updated_at");
        assert_eq!(m["sort"], "desc");
        assert_eq!(m["per_page"], "100");
        assert_eq!(m["page"], "3");
        assert!(!m.contains_key("updated_before"));
    }

    #[test]
    fn mr_list_reviewer() {
        let q = MrQuery {
            author_id: None,
            reviewer_id: Some(9),
            updated_after: after(),
        };
        let m = map(&mr_list_url(BASE, &q, 1));
        assert_eq!(m["reviewer_id"], "9");
        assert!(!m.contains_key("author_id"));
        assert_eq!(m["scope"], "all");
        assert_eq!(m["page"], "1");
    }

    #[test]
    fn events_query() {
        let a = NaiveDate::from_ymd_opt(2026, 9, 27).unwrap();
        let b = NaiveDate::from_ymd_opt(2026, 10, 2).unwrap();
        let u = events_url(BASE, 7, a, b, 2);
        assert_eq!(u.path(), "/api/v4/users/7/events");
        let m = map(&u);
        assert_eq!(m["after"], "2026-09-27");
        assert_eq!(m["before"], "2026-10-02");
        assert_eq!(m["sort"], "desc");
        assert_eq!(m["per_page"], "100");
        assert_eq!(m["page"], "2");
        assert!(!m.contains_key("target_type"));
    }

    #[test]
    fn project_mrs_repeats_iids() {
        let u = project_mrs_url(BASE, 42, &[1, 5, 9]);
        assert_eq!(u.path(), "/api/v4/projects/42/merge_requests");
        let p = pairs(&u);
        let iids: Vec<&str> = p
            .iter()
            .filter(|(k, _)| k == "iids[]")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(iids, vec!["1", "5", "9"]);
        let m = map(&u);
        assert_eq!(m["state"], "all");
        assert_eq!(m["per_page"], "100");
    }

    #[test]
    fn client_new_validates() {
        let cfg = |url: &str, token: &str| GitLabConfig {
            url: url.to_string(),
            token: token.to_string(),
            username: None,
        };
        assert!(GitLabClient::new(&cfg(BASE, "glpat-x"), false).is_ok());
        assert!(GitLabClient::new(&cfg("http://", "glpat-x"), false).is_err());
        assert!(GitLabClient::new(&cfg(BASE, "bad\ntoken"), false).is_err());
    }
}
