//! GitLab source: the `GitLabApi` trait (network boundary), API DTOs, client and collector.

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;

use crate::error::ApiError;
use crate::http::Listing;

pub mod client;
pub mod collect;
#[cfg(test)]
pub mod fakes;

/// Page cap for the MR queries (100 items per page).
pub const MR_PAGE_CAP: u32 = 20;
/// Page cap for the user events query (100 items per page).
pub const EVENT_PAGE_CAP: u32 = 30;
/// Items per page for every paginated GitLab request.
pub const PER_PAGE: usize = 100;

/// Query for `GET /merge_requests` (`scope=all`, `state=all`).
#[derive(Debug, Clone, PartialEq)]
pub struct MrQuery {
    pub author_id: Option<u64>,
    pub reviewer_id: Option<u64>,
    pub updated_after: DateTime<Utc>,
}

pub trait GitLabApi: Send + Sync {
    /// `GET /user`
    fn current_user(&self) -> Result<GlUser, ApiError>;
    /// `GET /users?username=`
    fn users_by_username(&self, username: &str) -> Result<Vec<GlUser>, ApiError>;
    /// Cap 20 pages.
    fn merge_requests(&self, q: &MrQuery) -> Result<Listing<GlMergeRequest>, ApiError>;
    /// Cap 30 pages.
    fn user_events(
        &self,
        user_id: u64,
        after: NaiveDate,
        before: NaiveDate,
    ) -> Result<Listing<GlEvent>, ApiError>;
    /// One request, `iids.len() <= 20`.
    fn project_merge_requests(
        &self,
        project_id: u64,
        iids: &[u64],
    ) -> Result<Vec<GlMergeRequest>, ApiError>;
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GlUser {
    pub id: u64,
    pub username: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GlUserRef {
    pub id: u64,
    pub username: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GlReferences {
    /// "group/proj!123"
    pub full: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GlMergeRequest {
    pub id: u64,
    pub iid: u64,
    pub project_id: u64,
    pub title: String,
    pub description: Option<String>,
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    pub source_branch: String,
    pub web_url: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub author: GlUserRef,
    #[serde(default)]
    pub reviewers: Vec<GlUserRef>,
    pub references: Option<GlReferences>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GlEvent {
    pub action_name: String,
    pub target_type: Option<String>,
    pub target_iid: Option<u64>,
    pub target_title: Option<String>,
    pub project_id: Option<u64>,
    pub created_at: DateTime<Utc>,
    pub note: Option<GlNote>,
    pub push_data: Option<GlPushData>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GlNote {
    pub noteable_type: Option<String>,
    pub noteable_iid: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GlPushData {
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    pub ref_type: Option<String>,
}
