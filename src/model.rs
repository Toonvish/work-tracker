//! Domain types shared by the sources, the matcher and the UI.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::Serialize;

/// A YouTrack issue identifier such as `SP-123`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(into = "String")]
pub struct IssueId {
    /// Uppercase ASCII alphanumeric project short name.
    pub project: String,
    pub number: u32,
}

impl From<IssueId> for String {
    fn from(id: IssueId) -> String {
        id.to_string()
    }
}

impl fmt::Display for IssueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.project, self.number)
    }
}

impl FromStr for IssueId {
    type Err = String;

    /// Whole string must match `^[A-Za-z][A-Za-z0-9]{0,19}-[0-9]{1,7}$` (after trim).
    fn from_str(s: &str) -> Result<Self, String> {
        let t = s.trim();
        let bad = || format!("invalid issue id \"{t}\" (expected like SP-123)");
        let (project, number) = t.split_once('-').ok_or_else(bad)?;
        let mut chars = project.chars();
        let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
        let rest_ok = chars.all(|c| c.is_ascii_alphanumeric());
        if !first_ok || !rest_ok || project.len() > 20 {
            return Err(bad());
        }
        if number.is_empty() || number.len() > 7 || !number.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad());
        }
        let number: u32 = number.parse().map_err(|_| bad())?;
        if number == 0 {
            return Err(bad());
        }
        Ok(IssueId {
            project: project.to_ascii_uppercase(),
            number,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    // MR roles
    Author,
    Reviewer,
    Approver,
    Commenter,
    Merger,
    Participant,
    // Issue roles (Commenter is shared)
    Assignee,
    Updater,
    /// Issue fetched only because an in-window MR references it.
    Referenced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MrState {
    Opened,
    Draft,
    Merged,
    Closed,
    Locked,
}

impl MrState {
    /// Maps GitLab's `state` string plus the draft flag. Unknown states become `Opened`.
    pub fn from_gitlab(state: &str, draft: bool) -> MrState {
        match state {
            "opened" if draft => MrState::Draft,
            "merged" => MrState::Merged,
            "closed" => MrState::Closed,
            "locked" => MrState::Locked,
            _ => MrState::Opened,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MergeRequest {
    pub project_id: u64,
    pub iid: u64,
    /// "group/sub/project"
    pub project_path: String,
    pub title: String,
    #[serde(skip)]
    pub description: String,
    pub source_branch: String,
    pub state: MrState,
    pub web_url: String,
    /// Username.
    pub author: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub roles: BTreeSet<Role>,
    /// Extracted and validated, first-occurrence order.
    pub issue_ids: Vec<IssueId>,
}

impl MergeRequest {
    /// Identity used for dedup: `(project_id, iid)`.
    pub fn key(&self) -> (u64, u64) {
        (self.project_id, self.iid)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Issue {
    pub id: IssueId,
    pub summary: String,
    /// YouTrack State value name, or a fallback.
    pub state: String,
    pub resolved: bool,
    /// `<yt_url>/issue/<idReadable>`
    pub web_url: String,
    pub updated: DateTime<Utc>,
    pub roles: BTreeSet<Role>,
    /// False for Referenced-only issues.
    pub in_window: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(p: &str, n: u32) -> IssueId {
        IssueId {
            project: p.to_string(),
            number: n,
        }
    }

    #[test]
    fn parses_case_insensitively() {
        assert_eq!("sp-123".parse::<IssueId>(), Ok(id("SP", 123)));
        assert_eq!("  Sp-7 ".parse::<IssueId>(), Ok(id("SP", 7)));
        assert_eq!("ab12-1234567".parse::<IssueId>(), Ok(id("AB12", 1_234_567)));
    }

    #[test]
    fn rejects_invalid() {
        for s in [
            "SP-0",
            "SP",
            "-1",
            "SP-",
            "S_P-1",
            "1P-1",
            "SP-12345678",
            "SP-1a",
            "SP--1",
            "SP-1-2",
            "",
            "ABCDEFGHIJKLMNOPQRSTU-1",
        ] {
            assert!(s.parse::<IssueId>().is_err(), "{s:?} should fail");
        }
        assert!("ABCDEFGHIJKLMNOPQRST-1".parse::<IssueId>().is_ok());
    }

    #[test]
    fn display_and_serde() {
        let i = id("SP", 123);
        assert_eq!(i.to_string(), "SP-123");
        assert_eq!(serde_json::to_string(&i).unwrap(), "\"SP-123\"");
        assert_eq!(String::from(i), "SP-123");
    }

    #[test]
    fn mr_state_mapping() {
        assert_eq!(MrState::from_gitlab("opened", true), MrState::Draft);
        assert_eq!(MrState::from_gitlab("opened", false), MrState::Opened);
        assert_eq!(MrState::from_gitlab("merged", false), MrState::Merged);
        assert_eq!(MrState::from_gitlab("merged", true), MrState::Merged);
        assert_eq!(MrState::from_gitlab("closed", true), MrState::Closed);
        assert_eq!(MrState::from_gitlab("locked", false), MrState::Locked);
        assert_eq!(MrState::from_gitlab("weird", false), MrState::Opened);
        assert_eq!(serde_json::to_string(&MrState::Draft).unwrap(), "\"draft\"");
    }

    #[test]
    fn role_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Role::Author).unwrap(), "\"author\"");
        assert_eq!(
            serde_json::to_string(&Role::Referenced).unwrap(),
            "\"referenced\""
        );
    }
}
