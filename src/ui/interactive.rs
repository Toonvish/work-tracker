//! Interactive list (DESIGN 8.4): dialoguer `Select` plus `open::that_detached`.

use chrono::{Datelike, Local, Utc};
use console::{Term, truncate_str};
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;

use super::rows::{Layout, Row, Target, build_rows, format_row};
use crate::error::CliError;
use crate::report::Report;

const PROMPT: &str = "Enter: open in browser \u{00B7} \u{2191}/\u{2193} move \u{00B7} Esc/q: quit";
const OPEN_ISSUE: &str = "Open issue in YouTrack";

fn open_url(u: &str) {
    match open::that_detached(u) {
        Ok(()) => eprintln!("Opened {u}"),
        Err(e) => eprintln!("warning: could not open browser ({e}); URL: {u}"),
    }
}

fn select(
    theme: &ColorfulTheme,
    items: &[String],
    cursor: usize,
    max_length: usize,
) -> Result<Option<usize>, CliError> {
    Select::with_theme(theme)
        .with_prompt(PROMPT)
        .items(items)
        .default(cursor)
        .max_length(max_length)
        .report(false)
        .interact_opt()
        .map_err(CliError::runtime)
}

/// Sub-menu for an issue group with two or more MRs. Returns after one open or when the user goes back.
fn choose_mr(
    theme: &ColorfulTheme,
    issue_url: &str,
    mrs: &[(String, String)],
    width: usize,
    max_length: usize,
) -> Result<(), CliError> {
    let mut items: Vec<String> = mrs
        .iter()
        .map(|(label, _)| truncate_str(label, width, "\u{2026}").into_owned())
        .collect();
    items.push(OPEN_ISSUE.to_string());
    if let Some(j) = select(theme, &items, 0, max_length)? {
        match mrs.get(j) {
            Some((_, url)) => open_url(url),
            None => open_url(issue_url),
        }
    }
    Ok(())
}

/// Runs the select loop until the user quits with Esc or `q`.
/// `warning_lines` is the number of warning lines already printed above the list.
pub fn run(
    report: &Report,
    now: chrono::DateTime<Utc>,
    warning_lines: usize,
) -> Result<(), CliError> {
    let (term_rows, term_cols) = Term::stdout().size();
    let (term_rows, term_cols) = (usize::from(term_rows), usize::from(term_cols));
    let width = term_cols.saturating_sub(3).max(40);
    let used = 2 + warning_lines;
    let max_length = term_rows.saturating_sub(used + 1).max(3);

    let now_year = now.with_timezone(&Local).year();
    let rows: Vec<Row> = build_rows(report, &Local, now_year);
    let labels: Vec<String> = rows
        .iter()
        .map(|r| format_row(r, Layout::Interactive { width }, false))
        .collect();

    let theme = ColorfulTheme::default();
    let mut cursor = 0usize;
    while let Some(i) = select(&theme, &labels, cursor, max_length)? {
        cursor = i;
        match &rows[i].target {
            Target::Url(u) => open_url(u),
            Target::ChooseMr { issue_url, mrs } => {
                choose_mr(&theme, issue_url, mrs, width, max_length)?;
            }
        }
    }
    Ok(())
}
