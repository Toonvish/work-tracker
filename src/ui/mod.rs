//! Output: mode selection, header and the three renderers (DESIGN 8).

use std::io::Write;

use chrono::{DateTime, Local, Utc};

use crate::error::CliError;
use crate::report::{Counts, Report};
use crate::window::{Window, format_window_line};

pub mod interactive;
pub mod json;
pub mod plain;
pub mod rows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Json,
    Plain,
    Interactive,
}

/// 8.1: `--json` wins; `--plain` or any non-TTY standard stream gives plain.
pub fn select_mode(
    json: bool,
    plain: bool,
    stdout_tty: bool,
    stderr_tty: bool,
    stdin_tty: bool,
) -> OutputMode {
    if json {
        OutputMode::Json
    } else if plain || !stdout_tty || !stderr_tty || !stdin_tty {
        OutputMode::Plain
    } else {
        OutputMode::Interactive
    }
}

fn plural(n: usize, singular: &str, plural: &str) -> String {
    format!("{n} {}", if n == 1 { singular } else { plural })
}

/// `3 issues · 5 MRs · 2 linked · 1 MR without issue · 1 issue without MR`
pub fn format_counts(c: &Counts) -> String {
    [
        plural(c.issues, "issue", "issues"),
        plural(c.mrs, "MR", "MRs"),
        format!("{} linked", c.linked_groups),
        plural(c.orphan_mrs, "MR without issue", "MRs without issue"),
        plural(c.issues_without_mr, "issue without MR", "issues without MR"),
    ]
    .join(" \u{00B7} ")
}

fn print_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("warning: {}", crate::text::sanitize_line(w));
    }
}

/// Writes a line to stdout; a closed pipe is not an error.
fn out_line(s: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{s}");
}

/// Prints the report in the chosen mode. Warnings always go to stderr.
pub fn render(
    mode: OutputMode,
    report: &Report,
    window: &Window,
    warnings: &[String],
    now: DateTime<Utc>,
) -> Result<(), CliError> {
    if mode == OutputMode::Json {
        print_warnings(warnings);
        return json::print(report, window, warnings);
    }

    out_line(&format_window_line(&Local, window, now));
    out_line(&format_counts(&report.counts));
    print_warnings(warnings);

    if report.entries.is_empty() {
        out_line("No activity found in this window.");
        return Ok(());
    }
    match mode {
        OutputMode::Plain => {
            plain::print(report, now);
            Ok(())
        }
        OutputMode::Interactive => interactive::run(report, now, warnings.len()),
        OutputMode::Json => unreachable!("handled above"),
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    //! Small model builders shared by the UI tests.

    use std::collections::BTreeSet;

    use chrono::{DateTime, TimeZone, Utc};

    use crate::model::{Issue, IssueId, MergeRequest, MrState, Role};
    use crate::report::{Counts, Entry, Report};

    pub fn t(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    pub fn id(s: &str) -> IssueId {
        s.parse().unwrap()
    }

    pub fn issue(ident: &str, summary: &str, updated: DateTime<Utc>) -> Issue {
        Issue {
            id: id(ident),
            summary: summary.into(),
            state: "In Progress".into(),
            resolved: false,
            web_url: format!("https://yt.example.com/issue/{ident}"),
            updated,
            roles: BTreeSet::from([Role::Assignee]),
            in_window: true,
        }
    }

    pub fn mr(iid: u64, path: &str, title: &str, updated: DateTime<Utc>) -> MergeRequest {
        MergeRequest {
            project_id: 1,
            iid,
            project_path: path.into(),
            title: title.into(),
            description: String::new(),
            source_branch: "feature".into(),
            state: MrState::Opened,
            web_url: format!("https://gl.example.com/{path}/-/merge_requests/{iid}"),
            author: "eric".into(),
            created_at: updated,
            updated_at: updated,
            roles: BTreeSet::from([Role::Author]),
            issue_ids: Vec::new(),
        }
    }

    pub fn report(entries: Vec<Entry>) -> Report {
        Report {
            entries,
            counts: Counts {
                issues: 0,
                mrs: 0,
                linked_groups: 0,
                orphan_mrs: 0,
                issues_without_mr: 0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_mode_truth_table() {
        for plain in [false, true] {
            for so in [false, true] {
                for se in [false, true] {
                    for si in [false, true] {
                        assert_eq!(select_mode(true, plain, so, se, si), OutputMode::Json);
                    }
                }
            }
        }
        for so in [false, true] {
            for se in [false, true] {
                for si in [false, true] {
                    assert_eq!(select_mode(false, true, so, se, si), OutputMode::Plain);
                    let expected = if so && se && si {
                        OutputMode::Interactive
                    } else {
                        OutputMode::Plain
                    };
                    assert_eq!(select_mode(false, false, so, se, si), expected);
                }
            }
        }
    }

    #[test]
    fn counts_plural() {
        let c = Counts {
            issues: 3,
            mrs: 5,
            linked_groups: 2,
            orphan_mrs: 2,
            issues_without_mr: 0,
        };
        assert_eq!(
            format_counts(&c),
            "3 issues \u{00B7} 5 MRs \u{00B7} 2 linked \u{00B7} 2 MRs without issue \u{00B7} 0 issues without MR"
        );
    }

    #[test]
    fn counts_singular() {
        let c = Counts {
            issues: 1,
            mrs: 1,
            linked_groups: 1,
            orphan_mrs: 1,
            issues_without_mr: 1,
        };
        assert_eq!(
            format_counts(&c),
            "1 issue \u{00B7} 1 MR \u{00B7} 1 linked \u{00B7} 1 MR without issue \u{00B7} 1 issue without MR"
        );
    }
}
