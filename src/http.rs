//! Shared HTTP helpers: pagination, status mapping, decoding, client builder.
//! Everything here except `build_client` is pure and testable without a network.

use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::error::ApiError;
use crate::text::truncate_bytes;

/// `service` value used in every GitLab error.
pub const GITLAB: &str = "GitLab";
/// `service` value used in every YouTrack error.
pub const YOUTRACK: &str = "YouTrack";

/// One fetched page.
#[derive(Debug, Clone, PartialEq)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub has_more: bool,
}

/// All items of a paginated query. `truncated` = the page cap was reached while more pages existed.
#[derive(Debug, Clone, PartialEq)]
pub struct Listing<T> {
    pub items: Vec<T>,
    pub truncated: bool,
}

/// Fetches pages `0..max_pages` until one reports `has_more == false`.
/// The first error aborts and is returned: a query never yields partial results.
pub fn paginate<T>(
    max_pages: u32,
    mut fetch_page: impl FnMut(u32) -> Result<Page<T>, ApiError>,
) -> Result<Listing<T>, ApiError> {
    let mut items = Vec::new();
    for i in 0..max_pages {
        let page = fetch_page(i)?;
        items.extend(page.items);
        if !page.has_more {
            return Ok(Listing {
                items,
                truncated: false,
            });
        }
    }
    Ok(Listing {
        items,
        truncated: true,
    })
}

/// Header present: more pages iff it is non-empty. Header absent (stripped by a proxy):
/// more pages iff the page was full.
pub fn gitlab_has_more(next_page_header: Option<&str>, n_items: usize, per_page: usize) -> bool {
    match next_page_header {
        Some(v) => !v.trim().is_empty(),
        None => n_items == per_page,
    }
}

const GITLAB_SCOPE_HINT: &str =
    "; the GitLab token lacks the read_api scope (create a token with read_api)";
const YOUTRACK_FORBIDDEN_HINT: &str = "; the token's user lacks permission for this resource (check project access and that the token has the YouTrack scope)";

/// Maps a non-2xx status to an `ApiError`. Never includes the token.
pub fn map_status(service: &'static str, status: u16, url: &str, body: &str) -> ApiError {
    let url = url.to_string();
    match status {
        401 => ApiError::Unauthorized {
            service,
            url,
            hint: if service == YOUTRACK {
                "[youtrack].token or YOUTRACK_TOKEN"
            } else {
                "[gitlab].token or GITLAB_TOKEN"
            },
        },
        403 => {
            let hint = if service == YOUTRACK {
                YOUTRACK_FORBIDDEN_HINT.to_string()
            } else if body.contains("insufficient_scope") {
                GITLAB_SCOPE_HINT.to_string()
            } else {
                String::new()
            };
            ApiError::Forbidden { service, url, hint }
        }
        404 => ApiError::NotFound { service, url },
        _ => ApiError::Http {
            service,
            status,
            url,
            body: truncate_bytes(body, 300).to_string(),
        },
    }
}

/// 2xx gives the body back, anything else goes through `map_status`.
pub fn check_response(
    service: &'static str,
    url: &str,
    status: u16,
    body: String,
) -> Result<String, ApiError> {
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(map_status(service, status, url, &body))
    }
}

/// Decodes a JSON body; a failure becomes `ApiError::Decode` carrying the URL.
pub fn decode<T: DeserializeOwned>(
    service: &'static str,
    url: &str,
    body: &str,
) -> Result<T, ApiError> {
    serde_json::from_str(body).map_err(|source| ApiError::Decode {
        service,
        url: url.to_string(),
        source,
    })
}

/// Blocking client with a 30 s timeout and a `work-tracker/<version>` User-Agent.
/// A build failure is reported as a GitLab error; use `build_client_for` to name another service.
pub fn build_client() -> Result<reqwest::blocking::Client, ApiError> {
    build_client_for(GITLAB)
}

/// Like `build_client`, attributing a build failure to `service`.
pub fn build_client_for(service: &'static str) -> Result<reqwest::blocking::Client, ApiError> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("work-tracker/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|source| ApiError::Network {
            service,
            url: String::new(),
            source,
        })
}

/// `GET <url> -> <status> (<n> items, <bytes> B)`. Never contains headers or tokens.
pub fn format_verbose(
    method: &str,
    url: &str,
    status: u16,
    n_items: usize,
    bytes: usize,
) -> String {
    format!("{method} {url} -> {status} ({n_items} items, {bytes} B)")
}

/// Writes the verbose request line to stderr when `enabled`.
pub fn log_verbose(
    enabled: bool,
    method: &str,
    url: &str,
    status: u16,
    n_items: usize,
    bytes: usize,
) {
    if enabled {
        eprintln!("{}", format_verbose(method, url, status, n_items, bytes));
    }
}

/// Cloneable error injection for the fakes (`ApiError` is not `Clone`).
#[cfg(test)]
#[derive(Clone, Debug)]
pub enum FakeErr {
    Unauthorized,
    Forbidden,
    NotFound,
    Status(u16),
}

#[cfg(test)]
impl FakeErr {
    pub fn to_api(&self, service: &'static str, url: &str) -> ApiError {
        let status = match self {
            FakeErr::Unauthorized => 401,
            FakeErr::Forbidden => 403,
            FakeErr::NotFound => 404,
            FakeErr::Status(s) => *s,
        };
        map_status(service, status, url, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn other() -> ApiError {
        ApiError::Other {
            service: GITLAB,
            message: "boom".into(),
        }
    }

    #[test]
    fn paginate_stops_when_no_more() {
        let mut calls = 0;
        let l = paginate(5, |i| {
            calls += 1;
            Ok(Page {
                items: vec![i],
                has_more: i < 2,
            })
        })
        .unwrap();
        assert_eq!(calls, 3);
        assert_eq!(l.items, vec![0, 1, 2]);
        assert!(!l.truncated);
    }

    #[test]
    fn paginate_truncated_at_cap() {
        let l = paginate(3, |i| {
            Ok(Page {
                items: vec![i],
                has_more: true,
            })
        })
        .unwrap();
        assert_eq!(l.items, vec![0, 1, 2]);
        assert!(l.truncated);
    }

    #[test]
    fn paginate_not_truncated_when_last_page_at_cap_has_no_more() {
        let l = paginate(2, |i| {
            Ok(Page {
                items: vec![i],
                has_more: i == 0,
            })
        })
        .unwrap();
        assert!(!l.truncated);
        assert_eq!(l.items.len(), 2);
    }

    #[test]
    fn paginate_error_on_second_page_is_returned() {
        let r = paginate(5, |i| {
            if i == 1 {
                Err(other())
            } else {
                Ok(Page {
                    items: vec![i],
                    has_more: true,
                })
            }
        });
        assert!(matches!(r, Err(ApiError::Other { .. })));
    }

    #[test]
    fn has_more_cases() {
        assert!(gitlab_has_more(Some("2"), 10, 100));
        assert!(!gitlab_has_more(Some(""), 100, 100));
        assert!(!gitlab_has_more(Some("  "), 100, 100));
        assert!(gitlab_has_more(None, 100, 100));
        assert!(!gitlab_has_more(None, 37, 100));
    }

    const U: &str = "https://host/api/v4/user";

    #[test]
    fn unauthorized_hints_per_service() {
        let e = map_status(GITLAB, 401, U, "");
        assert!(matches!(e, ApiError::Unauthorized { .. }));
        assert!(e.to_string().contains("GITLAB_TOKEN"));
        assert!(e.to_string().contains(U));
        let e = map_status(YOUTRACK, 401, U, "");
        assert!(e.to_string().contains("YOUTRACK_TOKEN"));
        assert!(!e.to_string().contains("GITLAB_TOKEN"));
    }

    #[test]
    fn forbidden_variants() {
        let e = map_status(GITLAB, 403, U, r#"{"error":"insufficient_scope"}"#);
        assert!(matches!(e, ApiError::Forbidden { .. }));
        assert!(e.to_string().contains("read_api"));
        assert!(e.to_string().contains(U));
        let e = map_status(GITLAB, 403, U, r#"{"message":"403 Forbidden"}"#);
        assert!(matches!(e, ApiError::Forbidden { .. }));
        assert!(!e.to_string().contains("read_api"));
        assert_eq!(
            e.to_string(),
            format!("GitLab: access denied (403) for {U}")
        );
        let e = map_status(YOUTRACK, 403, U, "");
        assert!(e.to_string().contains("lacks permission"));
        assert!(e.to_string().contains(U));
    }

    #[test]
    fn not_found_and_other_statuses() {
        let e = map_status(GITLAB, 404, U, "x");
        assert!(matches!(e, ApiError::NotFound { .. }));
        assert!(e.to_string().contains(U));
        for s in [400u16, 500] {
            let e = map_status(YOUTRACK, s, U, "bad");
            assert!(matches!(&e, ApiError::Http { status, .. } if *status == s));
            assert!(e.to_string().contains(U));
            assert!(e.to_string().contains("bad"));
        }
    }

    #[test]
    fn http_body_is_cut_char_safe() {
        let body = format!("a{}", "€".repeat(333));
        assert_eq!(body.len(), 1000);
        match map_status(GITLAB, 500, U, &body) {
            ApiError::Http { body: b, .. } => {
                assert!(b.len() <= 300);
                assert!(body.starts_with(&b));
            }
            e => panic!("unexpected {e:?}"),
        }
    }

    #[test]
    fn status_accessor() {
        assert_eq!(map_status(GITLAB, 401, U, "").status(), Some(401));
        assert_eq!(map_status(GITLAB, 403, U, "").status(), Some(403));
        assert_eq!(map_status(GITLAB, 404, U, "").status(), Some(404));
        assert_eq!(map_status(GITLAB, 400, U, "").status(), Some(400));
        assert_eq!(other().status(), None);
    }

    #[test]
    fn check_response_passes_2xx_and_maps_rest() {
        assert_eq!(check_response(GITLAB, U, 200, "ok".into()).unwrap(), "ok");
        assert_eq!(check_response(GITLAB, U, 204, "".into()).unwrap(), "");
        let e = check_response(GITLAB, U, 500, "x".into()).unwrap_err();
        assert_eq!(e.status(), Some(500));
        let e = check_response(GITLAB, U, 301, "x".into()).unwrap_err();
        assert_eq!(e.status(), Some(301));
    }

    #[test]
    fn decode_ok_and_error() {
        let v: Vec<u32> = decode(GITLAB, U, "[1,2]").unwrap();
        assert_eq!(v, vec![1, 2]);
        let e = decode::<Vec<u32>>(GITLAB, U, "{").unwrap_err();
        assert!(matches!(e, ApiError::Decode { .. }));
        assert!(e.to_string().contains(U));
        assert_eq!(e.status(), None);
    }

    #[test]
    fn verbose_line_format() {
        assert_eq!(
            format_verbose("GET", U, 200, 3, 1234),
            format!("GET {U} -> 200 (3 items, 1234 B)")
        );
    }

    #[test]
    fn client_builds() {
        assert!(build_client().is_ok());
    }

    #[test]
    fn fake_err_maps() {
        assert_eq!(FakeErr::Unauthorized.to_api(GITLAB, U).status(), Some(401));
        assert_eq!(FakeErr::Forbidden.to_api(GITLAB, U).status(), Some(403));
        assert_eq!(FakeErr::NotFound.to_api(GITLAB, U).status(), Some(404));
        assert_eq!(FakeErr::Status(502).to_api(GITLAB, U).status(), Some(502));
    }
}
