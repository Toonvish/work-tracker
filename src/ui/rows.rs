//! Row model and formatting for the list views (DESIGN 8.3). Pure and unit-tested.

use std::fmt::Display;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use console::{Alignment, measure_text_width, pad_str, style, truncate_str};

use crate::model::{Issue, MergeRequest, MrState, Role};
use crate::report::{Entry, Report};
use crate::text::sanitize_line;

const KIND_W: usize = 6;
const REF_W: usize = 24;
const STATE_W: usize = 12;
const UPDATED_W: usize = 11;
const ROLES_W: usize = 18;
/// Fixed columns (6 + 24 + 12 + 11 + 18) plus 5 separators of 2 columns.
const FIXED_W: usize = KIND_W + REF_W + STATE_W + UPDATED_W + ROLES_W + 5 * 2;
const SEP: &str = "  ";
const ELLIPSIS: &str = "\u{2026}";

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Url(String),
    ChooseMr {
        issue_url: String,
        /// (label `!456 group/proj  <title>`, url)
        mrs: Vec<(String, String)>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateStyle {
    Opened,
    Draft,
    Merged,
    Closed,
    IssueOpen,
    IssueResolved,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// `ISSUE`, `LINK`, `MR` or `  └ MR`.
    pub kind: &'static str,
    pub reference: String,
    pub title: String,
    pub state: String,
    pub state_style: StateStyle,
    /// Already formatted (local time).
    pub updated: String,
    pub roles: String,
    pub url: String,
    pub target: Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Interactive { width: usize },
    Plain { title_width: usize },
}

fn role_name(r: Role) -> &'static str {
    match r {
        Role::Author => "author",
        Role::Reviewer => "reviewer",
        Role::Approver => "approver",
        Role::Commenter => "commenter",
        Role::Merger => "merger",
        Role::Participant => "participant",
        Role::Assignee => "assignee",
        Role::Updater => "updater",
        Role::Referenced => "ref",
    }
}

fn roles_text<'a>(roles: impl IntoIterator<Item = &'a Role>) -> String {
    roles
        .into_iter()
        .map(|r| role_name(*r))
        .collect::<Vec<_>>()
        .join(",")
}

fn mr_state_text(s: MrState) -> &'static str {
    match s {
        MrState::Opened => "opened",
        MrState::Draft => "draft",
        MrState::Merged => "merged",
        MrState::Closed => "closed",
        MrState::Locked => "locked",
    }
}

fn mr_style(s: MrState) -> StateStyle {
    match s {
        MrState::Opened => StateStyle::Opened,
        MrState::Draft => StateStyle::Draft,
        MrState::Merged => StateStyle::Merged,
        MrState::Closed | MrState::Locked => StateStyle::Closed,
    }
}

fn format_updated<Tz: TimeZone>(t: DateTime<Utc>, tz: &Tz, now_year: i32) -> String
where
    Tz::Offset: Display,
{
    let local = t.with_timezone(tz);
    if local.year() == now_year {
        local.format("%m-%d %H:%M").to_string()
    } else {
        local.format("%Y-%m-%d").to_string()
    }
}

/// `!456 group/proj`, with the path cut from the left (`!456 …oup/proj`) when it exceeds 24 columns.
fn mr_ref(mr: &MergeRequest) -> String {
    let prefix = format!("!{} ", mr.iid);
    let path = sanitize_line(&mr.project_path);
    let full = format!("{prefix}{path}");
    if measure_text_width(&full) <= REF_W {
        return full;
    }
    let avail = REF_W.saturating_sub(measure_text_width(&prefix) + 1);
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0usize;
    for c in path.chars().rev() {
        let w = measure_text_width(c.encode_utf8(&mut [0u8; 4]));
        if used + w > avail {
            break;
        }
        used += w;
        tail.push(c);
    }
    tail.reverse();
    format!("{prefix}{ELLIPSIS}{}", tail.into_iter().collect::<String>())
}

fn mr_label(mr: &MergeRequest) -> String {
    format!(
        "!{} {}  {}",
        mr.iid,
        sanitize_line(&mr.project_path),
        sanitize_line(&mr.title)
    )
}

fn issue_row(issue: &Issue, mrs: &[MergeRequest], updated: String) -> Row {
    let (kind, target) = match mrs {
        [] => ("ISSUE", Target::Url(issue.web_url.clone())),
        [one] => ("LINK", Target::Url(one.web_url.clone())),
        many => (
            "LINK",
            Target::ChooseMr {
                issue_url: issue.web_url.clone(),
                mrs: many
                    .iter()
                    .map(|m| (mr_label(m), m.web_url.clone()))
                    .collect(),
            },
        ),
    };
    Row {
        kind,
        reference: sanitize_line(&issue.id.to_string()),
        title: sanitize_line(&issue.summary),
        state: sanitize_line(&issue.state),
        state_style: if issue.resolved {
            StateStyle::IssueResolved
        } else {
            StateStyle::IssueOpen
        },
        updated,
        roles: roles_text(&issue.roles),
        url: sanitize_line(&issue.web_url),
        target,
    }
}

fn mr_row<Tz: TimeZone>(
    mr: &MergeRequest,
    kind: &'static str,
    with_refs: bool,
    tz: &Tz,
    now_year: i32,
) -> Row
where
    Tz::Offset: Display,
{
    let mut title = sanitize_line(&mr.title);
    if with_refs && !mr.issue_ids.is_empty() {
        let ids: Vec<String> = mr.issue_ids.iter().map(|i| i.to_string()).collect();
        title.push_str(&format!(" \u{2192} {}", ids.join(", ")));
    }
    Row {
        kind,
        reference: mr_ref(mr),
        title,
        state: mr_state_text(mr.state).to_string(),
        state_style: mr_style(mr.state),
        updated: format_updated(mr.updated_at, tz, now_year),
        roles: roles_text(&mr.roles),
        url: sanitize_line(&mr.web_url),
        target: Target::Url(mr.web_url.clone()),
    }
}

/// One row per entry; an issue with MRs is a `LINK` row followed by one child row per MR.
pub fn build_rows<Tz: TimeZone>(report: &Report, tz: &Tz, now_year: i32) -> Vec<Row>
where
    Tz::Offset: Display,
{
    let mut rows = Vec::new();
    for entry in &report.entries {
        match entry {
            Entry::Issue { issue, mrs } => {
                rows.push(issue_row(
                    issue,
                    mrs,
                    format_updated(issue.updated, tz, now_year),
                ));
                for mr in mrs {
                    rows.push(mr_row(mr, "  \u{2514} MR", false, tz, now_year));
                }
            }
            Entry::OrphanMr(mr) => rows.push(mr_row(mr, "MR", true, tz, now_year)),
        }
    }
    rows
}

/// `min(80, widest title)`, at least 5.
pub fn plain_title_width(rows: &[Row]) -> usize {
    rows.iter()
        .map(|r| measure_text_width(&r.title))
        .max()
        .unwrap_or(0)
        .clamp(5, 80)
}

fn cell(s: &str, width: usize) -> String {
    pad_str(s, width, Alignment::Left, Some(ELLIPSIS)).into_owned()
}

fn state_styled(padded: String, st: StateStyle) -> String {
    let s = style(padded).force_styling(true);
    match st {
        StateStyle::Merged => s.magenta(),
        StateStyle::Opened => s.green(),
        StateStyle::Draft => s.yellow(),
        StateStyle::Closed => s.red(),
        StateStyle::IssueOpen => s,
        StateStyle::IssueResolved => s.dim(),
    }
    .to_string()
}

/// Formats one row. Colour is applied only when `color` is true.
pub fn format_row(row: &Row, layout: Layout, color: bool) -> String {
    let (title_w, roles_w, interactive_width) = match layout {
        Layout::Interactive { width } => (
            width.saturating_sub(FIXED_W).max(20),
            Some(ROLES_W),
            Some(width),
        ),
        Layout::Plain { title_width } => (title_width, None, None),
    };

    let kind = cell(row.kind, KIND_W);
    let kind = if color {
        style(kind).force_styling(true).bold().to_string()
    } else {
        kind
    };
    let state = cell(&row.state, STATE_W);
    let state = if color {
        state_styled(state, row.state_style)
    } else {
        state
    };

    let mut parts = vec![
        kind,
        cell(&row.reference, REF_W),
        cell(&row.title, title_w),
        state,
        cell(&row.updated, UPDATED_W),
    ];
    match roles_w {
        Some(w) => parts.push(truncate_str(&row.roles, w, ELLIPSIS).into_owned()),
        None => {
            parts.push(pad_str(&row.roles, ROLES_W, Alignment::Left, None).into_owned());
            parts.push(row.url.clone());
        }
    }
    let line = parts.join(SEP);
    let line = match interactive_width {
        Some(w) => truncate_str(&line, w, ELLIPSIS).into_owned(),
        None => line,
    };
    line.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Role;
    use crate::ui::fixtures::{issue, mr, report, t};
    use chrono_tz::Europe::Berlin;

    fn mixed_report() -> Report {
        let u = t(2026, 9, 30, 9, 0);
        let mut orphan = mr(9, "g/orphan", "Lonely", u);
        orphan.issue_ids = vec!["SP-9".parse().unwrap(), "SP-10".parse().unwrap()];
        report(vec![
            Entry::Issue {
                issue: issue("SP-1", "No MR", u),
                mrs: vec![],
            },
            Entry::Issue {
                issue: issue("SP-2", "One MR", u),
                mrs: vec![mr(20, "g/p", "mr twenty", u)],
            },
            Entry::Issue {
                issue: issue("SP-3", "Two MRs", u),
                mrs: vec![mr(30, "g/a", "first", u), mr(31, "g/b", "second", u)],
            },
            Entry::OrphanMr(orphan),
        ])
    }

    fn rows() -> Vec<Row> {
        build_rows(&mixed_report(), &Berlin, 2026)
    }

    #[test]
    fn kind_markers_per_entry_type() {
        let kinds: Vec<&str> = rows().iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                "ISSUE",
                "LINK",
                "  \u{2514} MR",
                "LINK",
                "  \u{2514} MR",
                "  \u{2514} MR",
                "MR"
            ]
        );
        for r in rows() {
            assert!(measure_text_width(r.kind) <= KIND_W);
        }
    }

    #[test]
    fn target_rules() {
        let r = rows();
        // 0 MRs: the issue.
        assert_eq!(
            r[0].target,
            Target::Url("https://yt.example.com/issue/SP-1".into())
        );
        // 1 MR: the MR, from the group row and from the child row.
        let mr20 = "https://gl.example.com/g/p/-/merge_requests/20".to_string();
        assert_eq!(r[1].target, Target::Url(mr20.clone()));
        assert_eq!(r[2].target, Target::Url(mr20));
        // 2 MRs: a choice with both MRs.
        match &r[3].target {
            Target::ChooseMr { issue_url, mrs } => {
                assert_eq!(issue_url, "https://yt.example.com/issue/SP-3");
                assert_eq!(mrs.len(), 2);
                assert_eq!(mrs[0].0, "!30 g/a  first");
                assert_eq!(mrs[0].1, "https://gl.example.com/g/a/-/merge_requests/30");
                assert_eq!(mrs[1].0, "!31 g/b  second");
            }
            other => panic!("expected ChooseMr, got {other:?}"),
        }
        // Children and orphans open their MR.
        assert_eq!(
            r[4].target,
            Target::Url("https://gl.example.com/g/a/-/merge_requests/30".into())
        );
        assert_eq!(
            r[6].target,
            Target::Url("https://gl.example.com/g/orphan/-/merge_requests/9".into())
        );
    }

    #[test]
    fn orphan_title_lists_referenced_ids_only_for_orphans() {
        let r = rows();
        assert_eq!(r[6].title, "Lonely \u{2192} SP-9, SP-10");
        assert_eq!(r[2].title, "mr twenty");
    }

    #[test]
    fn issue_and_mr_cells() {
        let r = rows();
        assert_eq!(r[0].reference, "SP-1");
        assert_eq!(r[0].state, "In Progress");
        assert_eq!(r[0].state_style, StateStyle::IssueOpen);
        assert_eq!(r[0].roles, "assignee");
        assert_eq!(r[0].url, "https://yt.example.com/issue/SP-1");
        assert_eq!(r[2].reference, "!20 g/p");
        assert_eq!(r[2].state, "opened");
        assert_eq!(r[2].state_style, StateStyle::Opened);
        assert_eq!(r[2].roles, "author");
        assert_eq!(r[2].url, "https://gl.example.com/g/p/-/merge_requests/20");
    }

    #[test]
    fn resolved_issue_and_mr_state_styles() {
        let u = t(2026, 9, 30, 9, 0);
        let mut i = issue("SP-1", "x", u);
        i.resolved = true;
        let mut merged = mr(1, "g/p", "m", u);
        merged.state = MrState::Merged;
        let mut locked = mr(2, "g/p", "l", u);
        locked.state = MrState::Locked;
        let mut draft = mr(3, "g/p", "d", u);
        draft.state = MrState::Draft;
        let rep = report(vec![
            Entry::Issue {
                issue: i,
                mrs: vec![],
            },
            Entry::OrphanMr(merged),
            Entry::OrphanMr(locked),
            Entry::OrphanMr(draft),
        ]);
        let r = build_rows(&rep, &Berlin, 2026);
        assert_eq!(r[0].state_style, StateStyle::IssueResolved);
        assert_eq!(r[1].state_style, StateStyle::Merged);
        assert_eq!(r[2].state_style, StateStyle::Closed);
        assert_eq!(r[2].state, "locked");
        assert_eq!(r[3].state_style, StateStyle::Draft);
    }

    #[test]
    fn referenced_role_shows_as_ref() {
        let u = t(2026, 9, 30, 9, 0);
        let mut i = issue("SP-1", "x", u);
        i.roles = [Role::Referenced].into();
        let mut m = mr(1, "g/p", "t", u);
        m.roles = [Role::Author, Role::Commenter].into();
        let rep = report(vec![Entry::Issue {
            issue: i,
            mrs: vec![m],
        }]);
        let r = build_rows(&rep, &Berlin, 2026);
        assert_eq!(r[0].roles, "ref");
        assert_eq!(r[1].roles, "author,commenter");
    }

    #[test]
    fn ref_is_truncated_from_the_left_keeping_iid() {
        let u = t(2026, 9, 30, 9, 0);
        let m = mr(456, "group/sub/very/long/project-path", "t", u);
        let rep = report(vec![Entry::OrphanMr(m)]);
        let r = build_rows(&rep, &Berlin, 2026);
        let reference = &r[0].reference;
        assert!(reference.starts_with("!456 \u{2026}"), "{reference}");
        assert!(reference.ends_with("project-path"), "{reference}");
        assert_eq!(measure_text_width(reference), REF_W);
    }

    #[test]
    fn short_ref_is_not_truncated() {
        let u = t(2026, 9, 30, 9, 0);
        let rep = report(vec![Entry::OrphanMr(mr(456, "g/p", "t", u))]);
        assert_eq!(build_rows(&rep, &Berlin, 2026)[0].reference, "!456 g/p");
    }

    #[test]
    fn updated_format_for_current_and_other_year() {
        let rep = report(vec![
            Entry::OrphanMr(mr(1, "g/p", "now", t(2026, 9, 30, 9, 5))),
            Entry::OrphanMr(mr(2, "g/p", "old", t(2025, 12, 31, 23, 30))),
        ]);
        let r = build_rows(&rep, &Berlin, 2026);
        // 09:05Z is 11:05 in Berlin (CEST).
        assert_eq!(r[0].updated, "09-30 11:05");
        // 23:30Z on 31 Dec is already 1 Jan 2026 in Berlin: the local year decides the format.
        assert_eq!(r[1].updated, "01-01 00:30");
    }

    #[test]
    fn other_year_uses_full_date() {
        let rep = report(vec![Entry::OrphanMr(mr(
            2,
            "g/p",
            "old",
            t(2025, 6, 15, 10, 0),
        ))]);
        let r = build_rows(&rep, &Berlin, 2026);
        assert_eq!(r[0].updated, "2025-06-15");
        assert_eq!(measure_text_width(&r[0].updated), 10);
    }

    #[test]
    fn control_characters_are_sanitized() {
        let u = t(2026, 9, 30, 9, 0);
        let mut m = mr(1, "g/p\nx", "line1\nline2\t\x1b[31mred", u);
        m.issue_ids = vec!["SP-1".parse().unwrap()];
        let mut i = issue("SP-5", "sum\r\nmary", u);
        i.state = "St\nate".into();
        let rep = report(vec![
            Entry::Issue {
                issue: i,
                mrs: vec![m.clone()],
            },
            Entry::OrphanMr(m),
        ]);
        let rows = build_rows(&rep, &Berlin, 2026);
        for r in &rows {
            for text in [&r.reference, &r.title, &r.state, &r.roles, &r.url] {
                assert!(!text.chars().any(char::is_control), "{text:?}");
            }
            let label = format_row(r, Layout::Interactive { width: 120 }, false);
            assert!(!label.chars().any(char::is_control), "{label:?}");
        }
        assert_eq!(rows[0].title, "sum  mary");
        assert_eq!(rows[1].title, "line1 line2  [31mred");
        if let Target::ChooseMr { .. } = rows[0].target {
            panic!("single MR must not be a choice");
        }
    }

    #[test]
    fn choose_mr_labels_are_sanitized() {
        let u = t(2026, 9, 30, 9, 0);
        let rep = report(vec![Entry::Issue {
            issue: issue("SP-1", "x", u),
            mrs: vec![mr(1, "g/a", "a\nb", u), mr(2, "g/b", "c\x1bd", u)],
        }]);
        let r = build_rows(&rep, &Berlin, 2026);
        let Target::ChooseMr { mrs, .. } = &r[0].target else {
            panic!("expected ChooseMr");
        };
        for (label, _) in mrs {
            assert!(!label.chars().any(char::is_control), "{label:?}");
        }
    }

    fn row(title: &str, roles: &str) -> Row {
        Row {
            kind: "LINK",
            reference: "SP-123".into(),
            title: title.into(),
            state: "In Progress".into(),
            state_style: StateStyle::IssueOpen,
            updated: "09-30 11:00".into(),
            roles: roles.into(),
            url: "https://yt.example.com/issue/SP-123".into(),
            target: Target::Url("https://yt.example.com/issue/SP-123".into()),
        }
    }

    #[test]
    fn interactive_row_matches_literal() {
        let r = row("Fix login", "assignee,commenter");
        assert_eq!(
            format_row(&r, Layout::Interactive { width: 120 }, false),
            "LINK    SP-123                    Fix login                                In Progress   09-30 11:00  assignee,commenter"
        );
    }

    #[test]
    fn interactive_row_has_no_trailing_spaces() {
        let r = row("Fix login", "");
        let s = format_row(&r, Layout::Interactive { width: 120 }, false);
        assert!(!s.ends_with(' '), "{s:?}");
        assert!(s.ends_with("09-30 11:00"), "{s:?}");
    }

    #[test]
    fn interactive_roles_are_truncated_to_18() {
        let r = row("t", "author,reviewer,approver,commenter");
        let s = format_row(&r, Layout::Interactive { width: 200 }, false);
        assert!(s.ends_with("author,reviewer,a\u{2026}"), "{s}");
    }

    #[test]
    fn interactive_labels_fit_width_and_have_no_escapes() {
        let long = "very long title ".repeat(30);
        let wide = "\u{65E5}\u{672C}\u{8A9E}".repeat(60);
        for title in [long.as_str(), wide.as_str(), "short"] {
            let r = row(title, "author,reviewer,approver,commenter,merger");
            for width in [60usize, 101, 160] {
                let label = format_row(&r, Layout::Interactive { width }, false);
                assert!(
                    measure_text_width(&label) <= width,
                    "width {width}: {} > {width}: {label}",
                    measure_text_width(&label)
                );
                assert!(!label.contains('\x1b'));
                assert!(!label.ends_with(' '), "{label:?}");
            }
        }
    }

    #[test]
    fn interactive_title_width_is_at_least_20() {
        let r = row(&"x".repeat(100), "a");
        let label = format_row(&r, Layout::Interactive { width: 90 }, false);
        // 90 - 81 = 9, raised to 20: the title is cut to 20 columns, and the row itself to 90.
        assert!(
            label.contains(&format!("{}\u{2026}", "x".repeat(19))),
            "{label}"
        );
        assert!(measure_text_width(&label) <= 90);
    }

    #[test]
    fn plain_row_ends_with_url_without_trailing_spaces() {
        let r = row("Fix login", "assignee");
        let s = format_row(&r, Layout::Plain { title_width: 20 }, false);
        assert!(s.ends_with("https://yt.example.com/issue/SP-123"), "{s}");
        assert!(!s.ends_with(' '));
        assert!(!s.contains('\x1b'));
    }

    #[test]
    fn plain_roles_are_not_truncated() {
        let roles = "author,reviewer,approver,commenter,merger,participant";
        let r = row("t", roles);
        let s = format_row(&r, Layout::Plain { title_width: 10 }, false);
        assert!(s.contains(roles), "{s}");
    }

    #[test]
    fn plain_title_is_cut_to_title_width() {
        let r = row(&"y".repeat(50), "a");
        let s = format_row(&r, Layout::Plain { title_width: 10 }, false);
        assert!(s.contains(&format!("{}\u{2026}", "y".repeat(9))), "{s}");
    }

    #[test]
    fn plain_title_width_bounds() {
        assert_eq!(plain_title_width(&[]), 5);
        assert_eq!(plain_title_width(&[row("ab", "")]), 5);
        assert_eq!(plain_title_width(&[row("abcdefgh", ""), row("abc", "")]), 8);
        assert_eq!(plain_title_width(&[row(&"z".repeat(200), "")]), 80);
    }

    #[test]
    fn color_only_when_requested() {
        let mut r = row("t", "a");
        r.state_style = StateStyle::Merged;
        let plain = format_row(&r, Layout::Plain { title_width: 10 }, false);
        assert!(!plain.contains('\x1b'));
        let colored = format_row(&r, Layout::Plain { title_width: 10 }, true);
        assert!(colored.contains("\x1b["), "{colored:?}");
        assert!(
            colored.contains("\x1b[35m"),
            "magenta for merged: {colored:?}"
        );
        assert!(!colored.ends_with(' '));
    }

    #[test]
    fn state_styles_use_documented_colors() {
        let cases = [
            (StateStyle::Opened, "\x1b[32m"),
            (StateStyle::Draft, "\x1b[33m"),
            (StateStyle::Closed, "\x1b[31m"),
            (StateStyle::IssueResolved, "\x1b[2m"),
        ];
        for (st, code) in cases {
            let mut r = row("t", "a");
            r.state_style = st;
            let s = format_row(&r, Layout::Plain { title_width: 10 }, true);
            assert!(s.contains(code), "{st:?}: {s:?}");
        }
    }
}
