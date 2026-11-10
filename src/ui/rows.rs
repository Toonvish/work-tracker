//! Row model and formatting for the list views (DESIGN 8.3). Pure and unit-tested.

use std::collections::BTreeSet;
use std::fmt::Display;

use chrono::{DateTime, Datelike, TimeZone, Utc};
use console::{Alignment, StyledObject, measure_text_width, pad_str, style};

use crate::model::{Issue, MergeRequest, MrState, Role};
use crate::report::{Entry, Report};
use crate::text::sanitize_line;

pub(super) const ISSUE_W: usize = 9;
pub(super) const MR_W: usize = 16;
pub(super) const ISSUE_STATE_W: usize = 12;
pub(super) const MR_STATE_W: usize = 8;
pub(super) const UPDATED_W: usize = 11;
const ROLES_W: usize = 18;
const SEP: &str = "  ";
pub(super) const ELLIPSIS: &str = "\u{2026}";
/// Shown in the issue or MR columns when the task has none.
pub(super) const NONE: &str = "-";

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
pub struct State {
    pub text: String,
    pub style: StateStyle,
}

/// One row per task: an issue with all its MRs, an issue without MR, or an MR without issue.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// `SP-123`; empty for an MR without issue.
    pub issue: String,
    /// `proj!456`, or `proj!456 +2` with more MRs; empty for an issue without MR.
    pub mr: String,
    pub title: String,
    pub issue_state: Option<State>,
    /// State of the MR shown in the `mr` column.
    pub mr_state: Option<State>,
    /// Already formatted (local time), latest of the issue and its MRs.
    pub updated: String,
    /// Union of the issue and MR roles.
    pub roles: String,
    /// Plain-layout links: the issue first, then every MR.
    pub urls: Vec<String>,
}

/// Column widths of the plain table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub title_width: usize,
    pub roles_width: usize,
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

/// `ref` is dropped when other roles explain why the task is listed.
pub(super) fn roles_text(mut roles: BTreeSet<Role>) -> String {
    if roles.len() > 1 {
        roles.remove(&Role::Referenced);
    }
    roles
        .into_iter()
        .map(role_name)
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

pub(super) fn mr_state(mr: &MergeRequest) -> State {
    State {
        text: mr_state_text(mr.state).to_string(),
        style: mr_style(mr.state),
    }
}

pub(super) fn issue_state(issue: &Issue) -> State {
    State {
        text: sanitize_line(&issue.state),
        style: if issue.resolved {
            StateStyle::IssueResolved
        } else {
            StateStyle::IssueOpen
        },
    }
}

pub(super) fn format_updated<Tz: TimeZone>(t: DateTime<Utc>, tz: &Tz, now_year: i32) -> String
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

/// `proj!456` (last path segment), plus ` +N` for further MRs. When it exceeds the
/// column, the project name is cut from the left (`…ject!456 +1`).
fn mr_ref(mr: &MergeRequest, more: usize) -> String {
    let path = sanitize_line(&mr.project_path);
    let name = path.rsplit('/').next().unwrap_or_default();
    let suffix = match more {
        0 => format!("!{}", mr.iid),
        n => format!("!{} +{n}", mr.iid),
    };
    let full = format!("{name}{suffix}");
    if measure_text_width(&full) <= MR_W {
        return full;
    }
    let avail = MR_W.saturating_sub(measure_text_width(&suffix) + 1);
    let mut tail: Vec<char> = Vec::new();
    let mut used = 0usize;
    for c in name.chars().rev() {
        let w = measure_text_width(c.encode_utf8(&mut [0u8; 4]));
        if used + w > avail {
            break;
        }
        used += w;
        tail.push(c);
    }
    tail.reverse();
    format!("{ELLIPSIS}{}{suffix}", tail.into_iter().collect::<String>())
}

fn issue_row<Tz: TimeZone>(issue: &Issue, mrs: &[MergeRequest], tz: &Tz, now_year: i32) -> Row
where
    Tz::Offset: Display,
{
    let updated = mrs
        .iter()
        .map(|m| m.updated_at)
        .fold(issue.updated, DateTime::max);
    let roles = mrs
        .iter()
        .flat_map(|m| m.roles.iter().copied())
        .chain(issue.roles.iter().copied())
        .collect();
    Row {
        issue: sanitize_line(&issue.id.to_string()),
        mr: mrs
            .first()
            .map(|m| mr_ref(m, mrs.len() - 1))
            .unwrap_or_default(),
        title: sanitize_line(&issue.summary),
        issue_state: Some(issue_state(issue)),
        mr_state: mrs.first().map(mr_state),
        updated: format_updated(updated, tz, now_year),
        roles: roles_text(roles),
        urls: std::iter::once(&issue.web_url)
            .chain(mrs.iter().map(|m| &m.web_url))
            .map(|u| sanitize_line(u))
            .collect(),
    }
}

fn orphan_row<Tz: TimeZone>(mr: &MergeRequest, tz: &Tz, now_year: i32) -> Row
where
    Tz::Offset: Display,
{
    let mut title = sanitize_line(&mr.title);
    if !mr.issue_ids.is_empty() {
        let ids: Vec<String> = mr.issue_ids.iter().map(|i| i.to_string()).collect();
        title.push_str(&format!(" \u{2192} {}", ids.join(", ")));
    }
    Row {
        issue: String::new(),
        mr: mr_ref(mr, 0),
        title,
        issue_state: None,
        mr_state: Some(mr_state(mr)),
        updated: format_updated(mr.updated_at, tz, now_year),
        roles: roles_text(mr.roles.clone()),
        urls: vec![sanitize_line(&mr.web_url)],
    }
}

/// One row per entry, in entry order.
pub fn build_rows<Tz: TimeZone>(report: &Report, tz: &Tz, now_year: i32) -> Vec<Row>
where
    Tz::Offset: Display,
{
    report
        .entries
        .iter()
        .map(|entry| match entry {
            Entry::Issue { issue, mrs } => issue_row(issue, mrs, tz, now_year),
            Entry::OrphanMr(mr) => orphan_row(mr, tz, now_year),
        })
        .collect()
}

/// Plain layout for `rows`: titles `min(80, widest)`, at least 5; roles as wide as the
/// widest, at least 18, so the links line up.
pub fn plain_layout(rows: &[Row]) -> Layout {
    Layout {
        title_width: plain_title_width(rows),
        roles_width: rows
            .iter()
            .map(|r| measure_text_width(&r.roles))
            .fold(ROLES_W, usize::max),
    }
}

fn plain_title_width(rows: &[Row]) -> usize {
    rows.iter()
        .map(|r| measure_text_width(&r.title))
        .max()
        .unwrap_or(0)
        .clamp(5, 80)
}

fn cell(s: &str, width: usize) -> String {
    pad_str(s, width, Alignment::Left, Some(ELLIPSIS)).into_owned()
}

/// Applies `f` when `color` is on. Empty strings stay empty so no stray escapes trail a line.
fn paint(
    s: String,
    color: bool,
    f: impl FnOnce(StyledObject<String>) -> StyledObject<String>,
) -> String {
    if color && !s.is_empty() {
        f(style(s).force_styling(true)).to_string()
    } else {
        s
    }
}

fn state_cell(state: Option<&State>, width: usize, color: bool) -> String {
    let Some(state) = state else {
        return paint(cell(NONE, width), color, StyledObject::dim);
    };
    paint(cell(&state.text, width), color, |s| match state.style {
        StateStyle::Merged => s.magenta(),
        StateStyle::Opened => s.green(),
        StateStyle::Draft => s.yellow(),
        StateStyle::Closed => s.red(),
        StateStyle::IssueOpen => s,
        StateStyle::IssueResolved => s.dim(),
    })
}

/// A reference cell (issue or MR) in its column colour, or a dim `-` when empty.
fn ref_cell(
    s: &str,
    width: usize,
    color: bool,
    f: impl FnOnce(StyledObject<String>) -> StyledObject<String>,
) -> String {
    if s.is_empty() {
        paint(cell(NONE, width), color, StyledObject::dim)
    } else {
        paint(cell(s, width), color, f)
    }
}

fn join(parts: Vec<String>) -> String {
    parts.join(SEP).trim_end().to_string()
}

/// Formats one row. Colour is applied only when `color` is true.
pub fn format_row(row: &Row, layout: Layout, color: bool) -> String {
    let parts = vec![
        ref_cell(&row.issue, ISSUE_W, color, |s| s.cyan().bold()),
        ref_cell(&row.mr, MR_W, color, |s| s.blue().bright()),
        cell(&row.title, layout.title_width),
        state_cell(row.issue_state.as_ref(), ISSUE_STATE_W, color),
        state_cell(row.mr_state.as_ref(), MR_STATE_W, color),
        paint(cell(&row.updated, UPDATED_W), color, StyledObject::dim),
        paint(
            pad_str(&row.roles, layout.roles_width, Alignment::Left, None).into_owned(),
            color,
            StyledObject::dim,
        ),
        row.urls.join(SEP),
    ];
    join(parts)
}

/// The column header line, aligned with `format_row` for the same layout.
pub fn format_header(layout: Layout, color: bool) -> String {
    let parts = vec![
        cell("ISSUE", ISSUE_W),
        cell("MR", MR_W),
        cell("TITLE", layout.title_width),
        cell("ISSUE STATE", ISSUE_STATE_W),
        cell("MR STATE", MR_STATE_W),
        cell("UPDATED", UPDATED_W),
        pad_str("ROLES", layout.roles_width, Alignment::Left, None).into_owned(),
        "URL".to_string(),
    ];
    paint(join(parts), color, StyledObject::bold)
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
    fn one_row_per_entry() {
        let r = rows();
        assert_eq!(r.len(), 4);
        let refs: Vec<(&str, &str)> = r.iter().map(|r| (&*r.issue, &*r.mr)).collect();
        assert_eq!(
            refs,
            vec![
                ("SP-1", ""),
                ("SP-2", "p!20"),
                ("SP-3", "a!30 +1"),
                ("", "orphan!9")
            ]
        );
    }

    #[test]
    fn urls_list_issue_then_mrs() {
        let r = rows();
        assert_eq!(r[0].urls, vec!["https://yt.example.com/issue/SP-1"]);
        assert_eq!(
            r[2].urls,
            vec![
                "https://yt.example.com/issue/SP-3",
                "https://gl.example.com/g/a/-/merge_requests/30",
                "https://gl.example.com/g/b/-/merge_requests/31",
            ]
        );
        assert_eq!(
            r[3].urls,
            vec!["https://gl.example.com/g/orphan/-/merge_requests/9"]
        );
    }

    #[test]
    fn orphan_title_lists_referenced_ids() {
        let r = rows();
        assert_eq!(r[3].title, "Lonely \u{2192} SP-9, SP-10");
        assert_eq!(r[1].title, "One MR");
    }

    #[test]
    fn issue_and_mr_cells() {
        let r = rows();
        let open = |text: &str, style| {
            Some(State {
                text: text.into(),
                style,
            })
        };
        assert_eq!(r[0].issue_state, open("In Progress", StateStyle::IssueOpen));
        assert_eq!(r[0].mr_state, None);
        assert_eq!(r[0].roles, "assignee");
        assert_eq!(r[1].mr_state, open("opened", StateStyle::Opened));
        assert_eq!(r[1].roles, "author,assignee");
        assert_eq!(r[3].issue_state, None);
        assert_eq!(r[3].roles, "author");
    }

    #[test]
    fn updated_is_latest_of_issue_and_mrs() {
        let mut m = mr(1, "g/p", "t", t(2026, 9, 30, 12, 0));
        m.updated_at = t(2026, 9, 30, 12, 0);
        let rep = report(vec![Entry::Issue {
            issue: issue("SP-1", "x", t(2026, 9, 30, 9, 0)),
            mrs: vec![m],
        }]);
        assert_eq!(build_rows(&rep, &Berlin, 2026)[0].updated, "09-30 14:00");
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
        let style = |s: &Option<State>| s.as_ref().unwrap().style;
        assert_eq!(style(&r[0].issue_state), StateStyle::IssueResolved);
        assert_eq!(style(&r[1].mr_state), StateStyle::Merged);
        assert_eq!(style(&r[2].mr_state), StateStyle::Closed);
        assert_eq!(r[2].mr_state.as_ref().unwrap().text, "locked");
        assert_eq!(style(&r[3].mr_state), StateStyle::Draft);
    }

    #[test]
    fn referenced_role_is_dropped_when_other_roles_exist() {
        let u = t(2026, 9, 30, 9, 0);
        let mut i = issue("SP-1", "x", u);
        i.roles = [Role::Referenced].into();
        let mut m = mr(1, "g/p", "t", u);
        m.roles = [Role::Author, Role::Commenter].into();
        let rep = report(vec![
            Entry::Issue {
                issue: i.clone(),
                mrs: vec![m],
            },
            Entry::Issue {
                issue: i,
                mrs: vec![],
            },
        ]);
        let r = build_rows(&rep, &Berlin, 2026);
        assert_eq!(r[0].roles, "author,commenter");
        assert_eq!(r[1].roles, "ref");
    }

    #[test]
    fn mr_ref_is_truncated_from_the_left_keeping_iid() {
        let u = t(2026, 9, 30, 9, 0);
        let m = mr(456, "group/sub/very-long-project-name", "t", u);
        let rep = report(vec![Entry::Issue {
            issue: issue("SP-1", "x", u),
            mrs: vec![m.clone(), m],
        }]);
        let r = build_rows(&rep, &Berlin, 2026);
        let reference = &r[0].mr;
        assert!(reference.starts_with('\u{2026}'), "{reference}");
        assert!(reference.ends_with("name!456 +1"), "{reference}");
        assert_eq!(measure_text_width(reference), MR_W);
    }

    #[test]
    fn short_mr_ref_uses_last_path_segment() {
        let u = t(2026, 9, 30, 9, 0);
        let rep = report(vec![Entry::OrphanMr(mr(456, "g/sub/proj", "t", u))]);
        assert_eq!(build_rows(&rep, &Berlin, 2026)[0].mr, "proj!456");
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
            let states = [&r.issue_state, &r.mr_state];
            let state_texts = states.iter().filter_map(|s| s.as_ref().map(|s| &s.text));
            for text in [&r.issue, &r.mr, &r.title, &r.roles]
                .into_iter()
                .chain(state_texts)
                .chain(&r.urls)
            {
                assert!(!text.chars().any(char::is_control), "{text:?}");
            }
        }
        assert_eq!(rows[0].title, "sum  mary");
        assert_eq!(rows[1].title, "line1 line2  [31mred \u{2192} SP-1");
    }

    fn plain(title_width: usize) -> Layout {
        Layout {
            title_width,
            roles_width: ROLES_W,
        }
    }

    fn row(title: &str, roles: &str) -> Row {
        Row {
            issue: "SP-123".into(),
            mr: "proj!45".into(),
            title: title.into(),
            issue_state: Some(State {
                text: "In Progress".into(),
                style: StateStyle::IssueOpen,
            }),
            mr_state: Some(State {
                text: "opened".into(),
                style: StateStyle::Opened,
            }),
            updated: "09-30 11:00".into(),
            roles: roles.into(),
            urls: vec![
                "https://yt.example.com/issue/SP-123".into(),
                "https://gl.example.com/g/proj/-/merge_requests/45".into(),
            ],
        }
    }

    #[test]
    fn header_aligns_with_rows() {
        let plain = plain(20);
        let header = format_header(plain, false);
        let line = format_row(&row("Fix login", "author"), plain, false);
        for (label, value) in [
            ("MR", "proj!45"),
            ("TITLE", "Fix login"),
            ("ISSUE STATE", "In Progress"),
            ("MR STATE", "opened"),
            ("UPDATED", "09-30"),
            ("ROLES", "author"),
        ] {
            assert_eq!(header.find(label), line.find(value), "{label}");
        }
        assert_eq!(
            format_header(plain, false).find("URL"),
            line.find("https://")
        );
    }

    #[test]
    fn missing_issue_or_mr_shows_dash() {
        let mut r = row("t", "author");
        r.issue = String::new();
        r.issue_state = None;
        let s = format_row(&r, plain(5), false);
        assert!(s.starts_with("-          proj!45"), "{s}");
        assert!(s.contains("t      -             opened"), "{s}");
    }

    #[test]
    fn plain_row_ends_with_all_urls_without_trailing_spaces() {
        let r = row("Fix login", "assignee");
        let s = format_row(&r, plain(20), false);
        assert!(
            s.ends_with(
                "https://yt.example.com/issue/SP-123  https://gl.example.com/g/proj/-/merge_requests/45"
            ),
            "{s}"
        );
        assert!(!s.ends_with(' '));
        assert!(!s.contains('\x1b'));
    }

    #[test]
    fn plain_roles_column_fits_the_widest_roles() {
        let rows = [row("t", "author"), row("t", &"r".repeat(30))];
        assert_eq!(plain_layout(&rows).roles_width, 30);
        let lines: Vec<String> = rows
            .iter()
            .map(|r| format_row(r, plain_layout(&rows), false))
            .collect();
        assert_eq!(lines[0].find("https://"), lines[1].find("https://"));
        assert_eq!(plain_layout(&rows[..1]).roles_width, ROLES_W);
    }

    #[test]
    fn plain_roles_are_not_truncated() {
        let roles = "author,reviewer,approver,commenter,merger,participant";
        let r = row("t", roles);
        let s = format_row(&r, plain(10), false);
        assert!(s.contains(roles), "{s}");
    }

    #[test]
    fn plain_title_is_cut_to_title_width() {
        let r = row(&"y".repeat(50), "a");
        let s = format_row(&r, plain(10), false);
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
        r.mr_state.as_mut().unwrap().style = StateStyle::Merged;
        let uncolored = format_row(&r, plain(10), false);
        assert!(!uncolored.contains('\x1b'));
        let colored = format_row(&r, plain(10), true);
        assert!(
            colored.contains("\x1b[35m"),
            "magenta for merged: {colored:?}"
        );
        assert!(colored.contains("\x1b[36m"), "cyan issue: {colored:?}");
        assert!(
            colored.contains("\x1b[38;5;12m"),
            "bright blue MR: {colored:?}"
        );
        assert!(!colored.ends_with(' '));
        // Same visible text with and without colour.
        assert_eq!(console::strip_ansi_codes(&colored), uncolored);
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
            r.mr_state.as_mut().unwrap().style = st;
            let s = format_row(&r, plain(10), true);
            assert!(s.contains(code), "{st:?}: {s:?}");
        }
    }
}
