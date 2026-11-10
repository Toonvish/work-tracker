//! The full, untruncated description of one task for the detail pane of the interactive
//! list. Pure and unit-tested.

use std::fmt::Display;

use chrono::TimeZone;

use super::rows::{State, format_updated, issue_state, mr_state, roles_text};
use crate::model::{Issue, MergeRequest, Role};
use crate::report::Entry;
use crate::text::sanitize_line;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Issue,
    Mr,
}

/// An issue or a merge request of a task.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub kind: Kind,
    /// `SP-123` or `group/proj!456`.
    pub reference: String,
    /// The whole issue summary or MR title.
    pub title: String,
    pub state: State,
    /// Roles, update time, branch and the like, shown after the state.
    pub meta: Vec<String>,
    pub url: String,
}

/// What a key press in the list asks to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Open {
    /// Enter: the most recently updated MR, or the issue when there is no MR.
    Default,
    Issue,
    /// The n-th MR, counted from 1.
    Mr(usize),
}

fn issue_item<Tz: TimeZone>(issue: &Issue, tz: &Tz, now_year: i32) -> Item
where
    Tz::Offset: Display,
{
    let mut meta = Vec::new();
    let roles = roles_text(issue.roles.clone());
    if !roles.is_empty() {
        meta.push(roles.replace(',', ", "));
    }
    meta.push(format!(
        "updated {}",
        format_updated(issue.updated, tz, now_year)
    ));
    Item {
        kind: Kind::Issue,
        reference: sanitize_line(&issue.id.to_string()),
        title: sanitize_line(&issue.summary),
        state: issue_state(issue),
        meta,
        url: issue.web_url.clone(),
    }
}

fn mr_item<Tz: TimeZone>(mr: &MergeRequest, orphan: bool, tz: &Tz, now_year: i32) -> Item
where
    Tz::Offset: Display,
{
    let mut meta = Vec::new();
    let roles = roles_text(mr.roles.clone());
    if !roles.is_empty() {
        meta.push(roles.replace(',', ", "));
    }
    if !mr.roles.contains(&Role::Author) && !mr.author.is_empty() {
        meta.push(format!("by {}", sanitize_line(&mr.author)));
    }
    meta.push(format!(
        "updated {}",
        format_updated(mr.updated_at, tz, now_year)
    ));
    if !mr.source_branch.is_empty() {
        meta.push(sanitize_line(&mr.source_branch));
    }
    if orphan && !mr.issue_ids.is_empty() {
        let ids: Vec<String> = mr.issue_ids.iter().map(|i| i.to_string()).collect();
        meta.push(format!("refers to {}", ids.join(", ")));
    }
    Item {
        kind: Kind::Mr,
        reference: format!("{}!{}", sanitize_line(&mr.project_path), mr.iid),
        title: sanitize_line(&mr.title),
        state: mr_state(mr),
        meta,
        url: mr.web_url.clone(),
    }
}

/// The issue (when there is one) followed by every MR of the entry.
pub fn items<Tz: TimeZone>(entry: &Entry, tz: &Tz, now_year: i32) -> Vec<Item>
where
    Tz::Offset: Display,
{
    match entry {
        Entry::Issue { issue, mrs } => std::iter::once(issue_item(issue, tz, now_year))
            .chain(mrs.iter().map(|m| mr_item(m, false, tz, now_year)))
            .collect(),
        Entry::OrphanMr(mr) => vec![mr_item(mr, true, tz, now_year)],
    }
}

/// The URL `open` refers to, or why there is nothing to open.
pub fn url_for(items: &[Item], open: Open) -> Result<&str, &'static str> {
    let issue = items.iter().find(|i| i.kind == Kind::Issue);
    let mut mrs = items.iter().filter(|i| i.kind == Kind::Mr);
    let found = match open {
        Open::Default => mrs.next().or(issue).ok_or("Nothing to open"),
        Open::Issue => issue.ok_or("This merge request has no issue"),
        Open::Mr(n) => n
            .checked_sub(1)
            .and_then(|i| mrs.nth(i))
            .ok_or("No merge request with that number"),
    };
    found.map(|i| i.url.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixtures::{issue, mr, t};
    use chrono_tz::Europe::Berlin;

    fn entry(mrs: Vec<MergeRequest>) -> Entry {
        Entry::Issue {
            issue: issue("SP-1", "Fix the login", t(2026, 9, 30, 9, 0)),
            mrs,
        }
    }

    #[test]
    fn issue_then_mrs_with_full_text() {
        let long = "feat(auth): ".to_string() + &"very long title ".repeat(20);
        let mut other = mr(31, "g/sub/b", "second", t(2026, 9, 30, 10, 0));
        other.roles = [Role::Reviewer].into();
        other.author = "anna".into();
        let e = entry(vec![mr(30, "g/a", &long, t(2026, 9, 30, 10, 0)), other]);
        let it = items(&e, &Berlin, 2026);
        assert_eq!(it.len(), 3);
        assert_eq!(
            (it[0].kind, &*it[0].reference, &*it[0].title),
            (Kind::Issue, "SP-1", "Fix the login")
        );
        assert_eq!(it[0].state.text, "In Progress");
        assert_eq!(it[0].meta, vec!["assignee", "updated 09-30 11:00"]);
        assert_eq!(it[1].reference, "g/a!30");
        assert_eq!(it[1].title, long);
        assert_eq!(it[1].meta, vec!["author", "updated 09-30 12:00", "feature"]);
        assert_eq!(it[2].reference, "g/sub/b!31");
        assert_eq!(
            it[2].meta,
            vec!["reviewer", "by anna", "updated 09-30 12:00", "feature"]
        );
    }

    #[test]
    fn orphan_lists_the_issues_it_refers_to() {
        let mut m = mr(9, "g/orphan", "Lonely\nline", t(2026, 9, 30, 9, 0));
        m.issue_ids = vec!["SP-9".parse().unwrap(), "SP-10".parse().unwrap()];
        let it = items(&Entry::OrphanMr(m), &Berlin, 2026);
        assert_eq!(it.len(), 1);
        assert_eq!(it[0].kind, Kind::Mr);
        assert_eq!(it[0].title, "Lonely line");
        assert_eq!(it[0].meta.last().unwrap(), "refers to SP-9, SP-10");
    }

    #[test]
    fn what_each_key_opens() {
        let u = t(2026, 9, 30, 9, 0);
        let none = items(&entry(vec![]), &Berlin, 2026);
        let two = items(
            &entry(vec![mr(30, "g/a", "a", u), mr(31, "g/b", "b", u)]),
            &Berlin,
            2026,
        );
        let orphan = items(&Entry::OrphanMr(mr(9, "g/o", "o", u)), &Berlin, 2026);
        let issue_url = "https://yt.example.com/issue/SP-1";
        let mr_url = |p: &str, n: u32| format!("https://gl.example.com/g/{p}/-/merge_requests/{n}");

        assert_eq!(url_for(&none, Open::Default), Ok(issue_url));
        assert_eq!(url_for(&none, Open::Issue), Ok(issue_url));
        assert!(url_for(&none, Open::Mr(1)).is_err());

        assert_eq!(url_for(&two, Open::Default), Ok(&*mr_url("a", 30)));
        assert_eq!(url_for(&two, Open::Issue), Ok(issue_url));
        assert_eq!(url_for(&two, Open::Mr(1)), Ok(&*mr_url("a", 30)));
        assert_eq!(url_for(&two, Open::Mr(2)), Ok(&*mr_url("b", 31)));
        assert!(url_for(&two, Open::Mr(3)).is_err());
        assert!(url_for(&two, Open::Mr(0)).is_err());

        assert_eq!(url_for(&orphan, Open::Default), Ok(&*mr_url("o", 9)));
        assert!(url_for(&orphan, Open::Issue).is_err());
    }
}
