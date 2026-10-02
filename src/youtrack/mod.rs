//! YouTrack source: the `YouTrackApi` trait (network boundary), API DTOs, client and collector.

use serde::Deserialize;

use crate::error::ApiError;
use crate::http::Listing;

pub mod client;
pub mod collect;
#[cfg(test)]
pub mod fakes;

/// Page cap for issue searches (100 items per page).
pub const ISSUE_PAGE_CAP: u32 = 20;
/// Items per page for issue searches.
pub const ISSUE_PAGE_SIZE: usize = 100;
/// Page cap for the activities query (200 items per page).
pub const ACTIVITY_PAGE_CAP: u32 = 25;
/// Items per page for the activities query.
pub const ACTIVITY_PAGE_SIZE: usize = 200;

/// Field selector for every issue request.
pub const ISSUE_FIELDS: &str = "idReadable,summary,created,updated,resolved,project(shortName),customFields(name,value(name,login,fullName))";

/// Every activity category that counts as work (6.3).
pub const FULL_CATEGORIES: &[&str] = &[
    "CommentsCategory",
    "CommentTextCategory",
    "CommentAttachmentsCategory",
    "CustomFieldCategory",
    "SummaryCategory",
    "DescriptionCategory",
    "IssueCreatedCategory",
    "IssueResolvedCategory",
    "ProjectCategory",
    "LinksCategory",
    "AttachmentsCategory",
    "AttachmentRenameCategory",
    "TagsCategory",
    "SprintCategory",
    "WorkItemCategory",
];

/// Retry set used when the server rejects some of `FULL_CATEGORIES` (HTTP 400).
pub const CORE_CATEGORIES: &[&str] = &[
    "CommentsCategory",
    "CustomFieldCategory",
    "SummaryCategory",
    "DescriptionCategory",
    "IssueCreatedCategory",
];

/// Query for `GET /api/activities` (`author=me`, `reverse=true`).
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityQuery {
    pub categories: Vec<&'static str>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub issue_query: Option<String>,
}

pub trait YouTrackApi: Send + Sync {
    /// `GET /api/admin/projects`
    fn project_short_names(&self) -> Result<Vec<String>, ApiError>;
    /// Cap 20 pages.
    fn search_issues(&self, query: &str) -> Result<Listing<YtIssue>, ApiError>;
    /// A 404 gives `Ok(None)`.
    fn issue_by_id(&self, id: &str) -> Result<Option<YtIssue>, ApiError>;
    /// Cap 25 pages.
    fn my_activities(&self, q: &ActivityQuery) -> Result<Listing<YtActivity>, ApiError>;
}

/// Settings for `collect`, built from the `[youtrack]` config section.
#[derive(Debug, Clone, PartialEq)]
pub struct YtSettings {
    pub base_url: String,
    pub projects: Vec<String>,
    pub state_field: String,
}

impl Default for YtSettings {
    fn default() -> Self {
        YtSettings {
            base_url: String::new(),
            projects: Vec::new(),
            state_field: "State".to_string(),
        }
    }
}

/// `[]` gives `None`, `[SP]` gives `project: SP`, `[SP, MS]` gives `(project: SP or project: MS)`.
pub fn project_clause(projects: &[String]) -> Option<String> {
    match projects {
        [] => None,
        [one] => Some(format!("project: {one}")),
        many => {
            let parts: Vec<String> = many.iter().map(|p| format!("project: {p}")).collect();
            Some(format!("({})", parts.join(" or ")))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct YtIssue {
    #[serde(rename = "idReadable")]
    pub id_readable: String,
    pub summary: Option<String>,
    pub updated: Option<i64>,
    pub resolved: Option<i64>,
    #[serde(rename = "customFields", default)]
    pub custom_fields: Vec<YtCustomField>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct YtCustomField {
    pub name: String,
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct YtActivity {
    pub timestamp: i64,
    pub category: Option<YtCategory>,
    pub target: Option<YtTarget>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct YtCategory {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct YtTarget {
    #[serde(rename = "idReadable")]
    pub id_readable: Option<String>,
    pub issue: Option<YtIssueRef>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct YtIssueRef {
    #[serde(rename = "idReadable")]
    pub id_readable: String,
}

/// One entry of `GET /api/admin/projects?fields=shortName,archived`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct YtProject {
    #[serde(rename = "shortName")]
    pub short_name: String,
    #[serde(default)]
    pub archived: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn project_clause_cases() {
        assert_eq!(project_clause(&[]), None);
        assert_eq!(project_clause(&s(&["SP"])), Some("project: SP".into()));
        assert_eq!(
            project_clause(&s(&["SP", "MS"])),
            Some("(project: SP or project: MS)".into())
        );
        assert_eq!(
            project_clause(&s(&["SP", "MS", "P"])),
            Some("(project: SP or project: MS or project: P)".into())
        );
    }

    #[test]
    fn default_settings() {
        let d = YtSettings::default();
        assert_eq!(d.base_url, "");
        assert!(d.projects.is_empty());
        assert_eq!(d.state_field, "State");
    }

    #[test]
    fn category_sets() {
        assert_eq!(FULL_CATEGORIES.len(), 15);
        assert_eq!(CORE_CATEGORIES.len(), 5);
        for c in CORE_CATEGORIES {
            assert!(FULL_CATEGORIES.contains(c));
        }
        assert!(!FULL_CATEGORIES.iter().any(|c| c.starts_with("Vcs")));
    }

    #[test]
    fn projects_fixture_deserializes() {
        let p: Vec<YtProject> =
            serde_json::from_str(include_str!("../../tests/fixtures/youtrack_projects.json"))
                .unwrap();
        assert_eq!(p.len(), 3);
        assert_eq!(p[0].short_name, "SP");
        assert_eq!(p[2].archived, Some(true));
    }

    #[test]
    fn activities_fixture_deserializes() {
        let a: Vec<YtActivity> = serde_json::from_str(include_str!(
            "../../tests/fixtures/youtrack_activities.json"
        ))
        .unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].category.as_ref().unwrap().id, "CommentsCategory");
        let t0 = a[0].target.as_ref().unwrap();
        assert_eq!(t0.id_readable, None);
        assert_eq!(t0.issue.as_ref().unwrap().id_readable, "SP-101");
        let t1 = a[1].target.as_ref().unwrap();
        assert_eq!(t1.id_readable.as_deref(), Some("MS-45"));
        assert!(t1.issue.is_none());
    }
}
