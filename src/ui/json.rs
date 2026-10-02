//! JSON output (DESIGN 8.5). Dedicated view structs keep the documented shape independent of the domain types.

use std::collections::BTreeSet;
use std::fmt::Display;
use std::io::Write;

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};
use serde::Serialize;

use crate::error::CliError;
use crate::model::{Issue, IssueId, MergeRequest, MrState, Role};
use crate::report::{Entry, Report};
use crate::window::{Window, WindowOrigin};

#[derive(Debug, Serialize)]
pub struct Document {
    pub window: WindowView,
    pub counts: CountsView,
    pub entries: Vec<EntryView>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WindowView {
    pub start: String,
    pub end: String,
    /// `meeting`, `previous` or `explicit`.
    pub origin: &'static str,
    pub previous: u32,
}

#[derive(Debug, Serialize)]
pub struct CountsView {
    pub issues: usize,
    pub merge_requests: usize,
    pub linked_groups: usize,
    pub orphan_merge_requests: usize,
    pub issues_without_mr: usize,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryView {
    Issue {
        issue: IssueView,
        merge_requests: Vec<MrView>,
    },
    MergeRequest {
        merge_request: MrView,
    },
}

#[derive(Debug, Serialize)]
pub struct IssueView {
    pub id: IssueId,
    pub summary: String,
    pub state: String,
    pub resolved: bool,
    pub web_url: String,
    pub updated: String,
    pub roles: BTreeSet<Role>,
    pub in_window: bool,
}

#[derive(Debug, Serialize)]
pub struct MrView {
    pub project_id: u64,
    pub iid: u64,
    pub project_path: String,
    pub title: String,
    pub source_branch: String,
    pub state: MrState,
    pub web_url: String,
    pub author: String,
    pub created_at: String,
    pub updated_at: String,
    pub roles: BTreeSet<Role>,
    pub issue_ids: Vec<IssueId>,
}

fn stamp<Tz: TimeZone>(t: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: Display,
{
    t.with_timezone(tz)
        .to_rfc3339_opts(SecondsFormat::Secs, false)
}

fn issue_view<Tz: TimeZone>(i: &Issue, tz: &Tz) -> IssueView
where
    Tz::Offset: Display,
{
    IssueView {
        id: i.id.clone(),
        summary: i.summary.clone(),
        state: i.state.clone(),
        resolved: i.resolved,
        web_url: i.web_url.clone(),
        updated: stamp(i.updated, tz),
        roles: i.roles.clone(),
        in_window: i.in_window,
    }
}

fn mr_view<Tz: TimeZone>(m: &MergeRequest, tz: &Tz) -> MrView
where
    Tz::Offset: Display,
{
    MrView {
        project_id: m.project_id,
        iid: m.iid,
        project_path: m.project_path.clone(),
        title: m.title.clone(),
        source_branch: m.source_branch.clone(),
        state: m.state,
        web_url: m.web_url.clone(),
        author: m.author.clone(),
        created_at: stamp(m.created_at, tz),
        updated_at: stamp(m.updated_at, tz),
        roles: m.roles.clone(),
        issue_ids: m.issue_ids.clone(),
    }
}

/// Builds the document with all times in `tz`.
pub fn build_document<Tz: TimeZone>(
    report: &Report,
    window: &Window,
    warnings: &[String],
    tz: &Tz,
) -> Document
where
    Tz::Offset: Display,
{
    let (origin, previous) = match window.origin {
        WindowOrigin::Explicit => ("explicit", 0),
        WindowOrigin::Meeting { previous, .. } if previous > 0 => ("previous", previous),
        WindowOrigin::Meeting { .. } => ("meeting", 0),
    };
    let c = &report.counts;
    Document {
        window: WindowView {
            start: stamp(window.start, tz),
            end: stamp(window.end, tz),
            origin,
            previous,
        },
        counts: CountsView {
            issues: c.issues,
            merge_requests: c.mrs,
            linked_groups: c.linked_groups,
            orphan_merge_requests: c.orphan_mrs,
            issues_without_mr: c.issues_without_mr,
        },
        entries: report
            .entries
            .iter()
            .map(|e| match e {
                Entry::Issue { issue, mrs } => EntryView::Issue {
                    issue: issue_view(issue, tz),
                    merge_requests: mrs.iter().map(|m| mr_view(m, tz)).collect(),
                },
                Entry::OrphanMr(m) => EntryView::MergeRequest {
                    merge_request: mr_view(m, tz),
                },
            })
            .collect(),
        warnings: warnings.to_vec(),
    }
}

/// Pretty-prints the document to stdout.
pub fn print(report: &Report, window: &Window, warnings: &[String]) -> Result<(), CliError> {
    let doc = build_document(report, window, warnings, &chrono::Local);
    let stdout = std::io::stdout();
    write_document(&mut stdout.lock(), &doc)
}

/// Writes the document; a closed pipe (e.g. `| head`) ends quietly as success.
fn write_document<W: Write>(out: &mut W, doc: &Document) -> Result<(), CliError> {
    if let Err(e) = serde_json::to_writer_pretty(&mut *out, doc) {
        if e.io_error_kind() == Some(std::io::ErrorKind::BrokenPipe) {
            return Ok(());
        }
        return Err(CliError::runtime(e));
    }
    match writeln!(out) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(CliError::runtime(e)),
        Ok(()) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Counts, Report};
    use crate::ui::fixtures::{issue, mr, report, t};
    use crate::window::Meeting;
    use chrono::{NaiveTime, Weekday};
    use chrono_tz::Europe::Berlin;
    use serde_json::Value;

    fn meeting() -> Meeting {
        Meeting {
            weekday: Weekday::Tue,
            start: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
            end: NaiveTime::from_hms_opt(10, 0, 0).unwrap(),
        }
    }

    fn window(origin: WindowOrigin) -> Window {
        Window {
            start: t(2026, 9, 29, 8, 0),
            end: t(2026, 9, 30, 9, 0),
            origin,
        }
    }

    fn sample() -> Report {
        let u = t(2026, 9, 30, 8, 30);
        let mut linked = mr(456, "g/p", "SP-123 fix", u);
        linked.issue_ids = vec!["SP-123".parse().unwrap()];
        let mut r = report(vec![
            Entry::Issue {
                issue: issue("SP-123", "Login", u),
                mrs: vec![linked],
            },
            Entry::OrphanMr(mr(7, "g/o", "Orphan", u)),
        ]);
        r.counts = Counts {
            issues: 1,
            mrs: 2,
            linked_groups: 1,
            orphan_mrs: 1,
            issues_without_mr: 0,
        };
        r
    }

    fn doc(origin: WindowOrigin, warnings: &[String]) -> Value {
        serde_json::to_value(build_document(
            &sample(),
            &window(origin),
            warnings,
            &Berlin,
        ))
        .unwrap()
    }

    fn meeting_origin(previous: u32) -> WindowOrigin {
        WindowOrigin::Meeting {
            meeting: meeting(),
            ended_at: t(2026, 9, 29, 8, 0),
            previous,
        }
    }

    struct FailingWriter(std::io::ErrorKind);

    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(self.0))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn document() -> Document {
        build_document(&sample(), &window(meeting_origin(0)), &[], &Berlin)
    }

    #[test]
    fn broken_pipe_is_success() {
        let mut w = FailingWriter(std::io::ErrorKind::BrokenPipe);
        assert!(write_document(&mut w, &document()).is_ok());
    }

    #[test]
    fn other_write_errors_are_runtime_failures() {
        let mut w = FailingWriter(std::io::ErrorKind::PermissionDenied);
        let err = write_document(&mut w, &document()).unwrap_err();
        assert_eq!(err.code, 1);
    }

    #[test]
    fn write_document_writes_json_and_newline() {
        let mut buf: Vec<u8> = Vec::new();
        write_document(&mut buf, &document()).unwrap();
        assert!(buf.ends_with(b"}\n"));
        let v: Value = serde_json::from_slice(&buf).unwrap();
        assert!(v.get("entries").is_some());
    }

    #[test]
    fn documented_keys_are_present() {
        let v = doc(meeting_origin(0), &["boom".to_string()]);
        let mut top: Vec<&String> = v.as_object().unwrap().keys().collect();
        top.sort();
        assert_eq!(top, ["counts", "entries", "warnings", "window"]);

        let w = &v["window"];
        assert_eq!(w["start"], "2026-09-29T10:00:00+02:00");
        assert_eq!(w["end"], "2026-09-30T11:00:00+02:00");
        assert_eq!(w["origin"], "meeting");
        assert_eq!(w["previous"], 0);

        let c = &v["counts"];
        assert_eq!(c["issues"], 1);
        assert_eq!(c["merge_requests"], 2);
        assert_eq!(c["linked_groups"], 1);
        assert_eq!(c["orphan_merge_requests"], 1);
        assert_eq!(c["issues_without_mr"], 0);

        assert_eq!(v["warnings"], serde_json::json!(["boom"]));
    }

    #[test]
    fn entries_have_kind_and_shape() {
        let v = doc(meeting_origin(0), &[]);
        let entries = v["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);

        let e0 = &entries[0];
        assert_eq!(e0["kind"], "issue");
        assert_eq!(e0["issue"]["id"], "SP-123");
        assert_eq!(e0["issue"]["summary"], "Login");
        assert_eq!(e0["issue"]["state"], "In Progress");
        assert_eq!(e0["issue"]["resolved"], false);
        assert_eq!(e0["issue"]["in_window"], true);
        assert_eq!(e0["issue"]["roles"], serde_json::json!(["assignee"]));
        assert_eq!(e0["issue"]["updated"], "2026-09-30T10:30:00+02:00");
        let m = &e0["merge_requests"][0];
        assert_eq!(m["iid"], 456);
        assert_eq!(m["project_id"], 1);
        assert_eq!(m["project_path"], "g/p");
        assert_eq!(m["state"], "opened");
        assert_eq!(m["roles"], serde_json::json!(["author"]));
        assert_eq!(m["issue_ids"], serde_json::json!(["SP-123"]));
        assert_eq!(m["created_at"], "2026-09-30T10:30:00+02:00");
        assert!(m.get("description").is_none());

        let e1 = &entries[1];
        assert_eq!(e1["kind"], "merge_request");
        assert_eq!(e1["merge_request"]["iid"], 7);
        assert!(e1.get("issue").is_none());
    }

    #[test]
    fn origin_variants() {
        assert_eq!(doc(meeting_origin(0), &[])["window"]["origin"], "meeting");
        let v = doc(meeting_origin(2), &[]);
        assert_eq!(v["window"]["origin"], "previous");
        assert_eq!(v["window"]["previous"], 2);
        let v = doc(WindowOrigin::Explicit, &[]);
        assert_eq!(v["window"]["origin"], "explicit");
        assert_eq!(v["window"]["previous"], 0);
    }

    #[test]
    fn empty_report_has_empty_entries() {
        let v = serde_json::to_value(build_document(
            &report(vec![]),
            &window(WindowOrigin::Explicit),
            &[],
            &Berlin,
        ))
        .unwrap();
        assert_eq!(v["entries"], serde_json::json!([]));
        assert_eq!(v["warnings"], serde_json::json!([]));
    }
}
