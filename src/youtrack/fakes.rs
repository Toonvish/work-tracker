//! In-memory `YouTrackApi` for unit tests, plus builders for DTOs.

use std::collections::HashMap;
use std::sync::Mutex;

use super::{
    ActivityQuery, YouTrackApi, YtActivity, YtCategory, YtCustomField, YtIssue, YtIssueRef,
    YtTarget,
};
use crate::error::ApiError;
use crate::http::{FakeErr, Listing, YOUTRACK};

pub struct FakeYouTrack {
    pub projects: Result<Vec<String>, FakeErr>,
    /// Keyed by the exact query string. An unknown query returns `Ok(empty)`.
    pub searches: HashMap<String, Result<Vec<YtIssue>, FakeErr>>,
    /// An unknown id returns `Ok(None)`.
    pub by_id: HashMap<String, Result<Option<YtIssue>, FakeErr>>,
    /// Matched by `categories.len()` (FULL = 15, CORE = 5). No match returns `Ok(empty)`.
    pub activities: Vec<(usize, Result<Vec<YtActivity>, FakeErr>)>,
    /// Marks every returned `Listing` as truncated.
    pub truncate: bool,
    pub calls: Mutex<Vec<String>>,
}

impl FakeYouTrack {
    /// Everything is empty and succeeds.
    pub fn new() -> FakeYouTrack {
        FakeYouTrack {
            projects: Ok(Vec::new()),
            searches: HashMap::new(),
            by_id: HashMap::new(),
            activities: Vec::new(),
            truncate: false,
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Recorded calls whose text starts with `prefix`.
    pub fn calls_with(&self, prefix: &str) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with(prefix))
            .collect()
    }

    /// Queries passed to `search_issues`, in order.
    pub fn search_queries(&self) -> Vec<String> {
        self.calls_with("search ")
            .into_iter()
            .map(|c| c["search ".len()..].to_string())
            .collect()
    }

    pub fn add_search(&mut self, query: &str, issues: Vec<YtIssue>) {
        self.searches.insert(query.to_string(), Ok(issues));
    }

    pub fn add_by_id(&mut self, requested: &str, issue: YtIssue) {
        self.by_id.insert(requested.to_string(), Ok(Some(issue)));
    }

    fn record(&self, s: String) {
        self.calls.lock().unwrap().push(s);
    }
}

impl Default for FakeYouTrack {
    fn default() -> Self {
        Self::new()
    }
}

fn err(e: &FakeErr, what: &str) -> ApiError {
    e.to_api(YOUTRACK, &format!("fake://youtrack/{what}"))
}

impl YouTrackApi for FakeYouTrack {
    fn project_short_names(&self) -> Result<Vec<String>, ApiError> {
        self.record("project_short_names".into());
        self.projects.clone().map_err(|e| err(&e, "projects"))
    }

    fn search_issues(&self, query: &str) -> Result<Listing<YtIssue>, ApiError> {
        self.record(format!("search {query}"));
        match self.searches.get(query) {
            None => Ok(Listing {
                items: Vec::new(),
                truncated: self.truncate,
            }),
            Some(Ok(items)) => Ok(Listing {
                items: items.clone(),
                truncated: self.truncate,
            }),
            Some(Err(e)) => Err(err(e, "issues")),
        }
    }

    fn issue_by_id(&self, id: &str) -> Result<Option<YtIssue>, ApiError> {
        self.record(format!("issue_by_id {id}"));
        match self.by_id.get(id) {
            None => Ok(None),
            Some(Ok(v)) => Ok(v.clone()),
            Some(Err(e)) => Err(err(e, "issue")),
        }
    }

    fn my_activities(&self, q: &ActivityQuery) -> Result<Listing<YtActivity>, ApiError> {
        self.record(format!(
            "my_activities {} {} {} {}",
            q.categories.len(),
            q.start_ms,
            q.end_ms,
            q.issue_query.as_deref().unwrap_or("-")
        ));
        match self
            .activities
            .iter()
            .find(|(n, _)| *n == q.categories.len())
        {
            None => Ok(Listing {
                items: Vec::new(),
                truncated: self.truncate,
            }),
            Some((_, Ok(items))) => Ok(Listing {
                items: items.clone(),
                truncated: self.truncate,
            }),
            Some((_, Err(e))) => Err(err(e, "activities")),
        }
    }
}

/// An unresolved issue whose `State` custom field is an object `{name: state}`.
pub fn yt_issue(id: &str, summary: &str, updated_ms: i64, state: &str) -> YtIssue {
    YtIssue {
        id_readable: id.into(),
        summary: Some(summary.into()),
        updated: Some(updated_ms),
        resolved: None,
        custom_fields: vec![YtCustomField {
            name: "State".into(),
            value: Some(serde_json::json!({ "name": state })),
        }],
    }
}

/// An activity on an issue itself (`target.idReadable`), e.g. `CustomFieldCategory`.
pub fn activity_on_issue(category: &str, id: &str, timestamp_ms: i64) -> YtActivity {
    YtActivity {
        timestamp: timestamp_ms,
        category: Some(YtCategory {
            id: category.into(),
        }),
        target: Some(YtTarget {
            id_readable: Some(id.into()),
            issue: None,
        }),
    }
}

/// An activity on a comment (`target.issue.idReadable`), e.g. `CommentsCategory`.
pub fn activity_on_comment(category: &str, id: &str, timestamp_ms: i64) -> YtActivity {
    YtActivity {
        timestamp: timestamp_ms,
        category: Some(YtCategory {
            id: category.into(),
        }),
        target: Some(YtTarget {
            id_readable: None,
            issue: Some(YtIssueRef {
                id_readable: id.into(),
            }),
        }),
    }
}
