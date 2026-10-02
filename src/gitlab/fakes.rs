//! In-memory `GitLabApi` for unit tests, plus builders for DTOs.

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, NaiveDate, Utc};

use super::{
    GitLabApi, GlEvent, GlMergeRequest, GlNote, GlPushData, GlReferences, GlUser, GlUserRef,
    MrQuery,
};
use crate::error::ApiError;
use crate::http::{FakeErr, GITLAB, Listing};

pub struct FakeGitLab {
    pub user: Result<GlUser, FakeErr>,
    pub users_by_name: Vec<GlUser>,
    pub authored: Result<Vec<GlMergeRequest>, FakeErr>,
    pub reviewer: Result<Vec<GlMergeRequest>, FakeErr>,
    pub events: Result<Vec<GlEvent>, FakeErr>,
    pub project_mrs: HashMap<u64, Result<Vec<GlMergeRequest>, FakeErr>>,
    /// Marks every returned `Listing` as truncated.
    pub truncate: bool,
    pub calls: Mutex<Vec<String>>,
}

impl FakeGitLab {
    /// Current user is `eric` (id 7); everything else is empty and succeeds.
    pub fn new() -> FakeGitLab {
        FakeGitLab {
            user: Ok(GlUser {
                id: 7,
                username: "eric".into(),
            }),
            users_by_name: Vec::new(),
            authored: Ok(Vec::new()),
            reviewer: Ok(Vec::new()),
            events: Ok(Vec::new()),
            project_mrs: HashMap::new(),
            truncate: false,
            calls: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, s: String) {
        self.calls.lock().unwrap().push(s);
    }
}

impl Default for FakeGitLab {
    fn default() -> Self {
        Self::new()
    }
}

fn err(e: &FakeErr, what: &str) -> ApiError {
    e.to_api(GITLAB, &format!("fake://gitlab/{what}"))
}

impl GitLabApi for FakeGitLab {
    fn current_user(&self) -> Result<GlUser, ApiError> {
        self.record("current_user".into());
        self.user.clone().map_err(|e| err(&e, "user"))
    }

    fn users_by_username(&self, username: &str) -> Result<Vec<GlUser>, ApiError> {
        self.record(format!("users_by_username {username}"));
        Ok(self
            .users_by_name
            .iter()
            .filter(|u| u.username == username)
            .cloned()
            .collect())
    }

    fn merge_requests(&self, q: &MrQuery) -> Result<Listing<GlMergeRequest>, ApiError> {
        let (kind, id, src) = match (q.author_id, q.reviewer_id) {
            (Some(id), _) => ("author", id, &self.authored),
            (None, Some(id)) => ("reviewer", id, &self.reviewer),
            (None, None) => panic!("MrQuery without author_id or reviewer_id"),
        };
        self.record(format!(
            "merge_requests {kind} {id} {}",
            q.updated_after
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ));
        match src {
            Ok(items) => Ok(Listing {
                items: items.clone(),
                truncated: self.truncate,
            }),
            Err(e) => Err(err(e, "merge_requests")),
        }
    }

    fn user_events(
        &self,
        user_id: u64,
        after: NaiveDate,
        before: NaiveDate,
    ) -> Result<Listing<GlEvent>, ApiError> {
        self.record(format!("user_events {user_id} {after} {before}"));
        match &self.events {
            Ok(items) => Ok(Listing {
                items: items.clone(),
                truncated: self.truncate,
            }),
            Err(e) => Err(err(e, "events")),
        }
    }

    fn project_merge_requests(
        &self,
        project_id: u64,
        iids: &[u64],
    ) -> Result<Vec<GlMergeRequest>, ApiError> {
        let list: Vec<String> = iids.iter().map(u64::to_string).collect();
        self.record(format!("project_mrs {project_id} {}", list.join(",")));
        match self.project_mrs.get(&project_id) {
            None => Ok(Vec::new()),
            Some(Ok(items)) => Ok(items
                .iter()
                .filter(|m| iids.contains(&m.iid))
                .cloned()
                .collect()),
            Some(Err(e)) => Err(err(e, "project_merge_requests")),
        }
    }
}

/// Parses an RFC 3339 timestamp (test helper).
pub fn ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// An opened MR authored by user 7 (`eric`), created and updated at `updated`.
pub fn mr(
    pid: u64,
    iid: u64,
    title: &str,
    branch: &str,
    desc: &str,
    updated: DateTime<Utc>,
) -> GlMergeRequest {
    GlMergeRequest {
        id: pid * 1000 + iid,
        iid,
        project_id: pid,
        title: title.into(),
        description: Some(desc.into()),
        state: "opened".into(),
        draft: false,
        source_branch: branch.into(),
        web_url: format!("https://gitlab.example.com/grp/proj{pid}/-/merge_requests/{iid}"),
        created_at: updated,
        updated_at: updated,
        author: GlUserRef {
            id: 7,
            username: "eric".into(),
        },
        reviewers: Vec::new(),
        references: Some(GlReferences {
            full: format!("grp/proj{pid}!{iid}"),
        }),
    }
}

/// An event on a merge request (`target_type == "MergeRequest"`).
pub fn ev_mr(pid: u64, iid: u64, action: &str, at: DateTime<Utc>) -> GlEvent {
    GlEvent {
        action_name: action.into(),
        target_type: Some("MergeRequest".into()),
        target_iid: Some(iid),
        target_title: Some("title".into()),
        project_id: Some(pid),
        created_at: at,
        note: None,
        push_data: None,
    }
}

/// A note event; `target_type` is e.g. "Note" or "DiffNote", `noteable_type` e.g. "MergeRequest".
pub fn ev_note(
    pid: u64,
    target_type: &str,
    noteable_type: &str,
    noteable_iid: u64,
    at: DateTime<Utc>,
) -> GlEvent {
    GlEvent {
        action_name: "commented on".into(),
        target_type: Some(target_type.into()),
        target_iid: None,
        target_title: None,
        project_id: Some(pid),
        created_at: at,
        note: Some(GlNote {
            noteable_type: Some(noteable_type.into()),
            noteable_iid: Some(noteable_iid),
        }),
        push_data: None,
    }
}

/// A push to a branch.
pub fn ev_push(pid: u64, branch: &str, at: DateTime<Utc>) -> GlEvent {
    GlEvent {
        action_name: "pushed to".into(),
        target_type: None,
        target_iid: None,
        target_title: None,
        project_id: Some(pid),
        created_at: at,
        note: None,
        push_data: Some(GlPushData {
            git_ref: Some(branch.into()),
            ref_type: Some("branch".into()),
        }),
    }
}
