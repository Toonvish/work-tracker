//! Interactive list (DESIGN 8.4): dialoguer `Select` plus `open::that_detached`.

use std::fmt;

use chrono::{Datelike, Local, Utc};
use console::{Term, truncate_str};
use dialoguer::Select;
use dialoguer::theme::{ColorfulTheme, Theme};

use super::rows::{Layout, Row, Target, build_rows, format_header, format_row};
use crate::error::CliError;
use crate::report::Report;

const HELP: &str = "Enter: open in browser \u{00B7} \u{2191}/\u{2193} move \u{00B7} Esc/q: quit";
const OPEN_ISSUE: &str = "Open issue in YouTrack";
/// Room for dialoguer's ` [Page 12/34] ` after the header when the list is paged.
const PAGING_W: usize = 16;

/// Renders list items from index keys. dialoguer 0.12 sizes items by their *byte* length
/// when redrawing (`prompts/select.rs`), so ANSI codes or non-ASCII text in a full-width
/// label would make it clear lines above the list. It only ever sees the short keys.
struct ListTheme<'a> {
    inner: ColorfulTheme,
    /// The prompt is the column header, indented like the items.
    header: bool,
    plain: &'a [String],
    colored: &'a [String],
}

impl Theme for ListTheme<'_> {
    fn format_select_prompt(&self, f: &mut dyn fmt::Write, prompt: &str) -> fmt::Result {
        if self.header {
            write!(f, "  {prompt}")
        } else {
            self.inner.format_select_prompt(f, prompt)
        }
    }

    /// The active row is drawn uncolored so the theme's highlight covers the whole row.
    fn format_select_prompt_item(
        &self,
        f: &mut dyn fmt::Write,
        key: &str,
        active: bool,
    ) -> fmt::Result {
        let i: usize = key.parse().map_err(|_| fmt::Error)?;
        let labels = if active { self.plain } else { self.colored };
        self.inner.format_select_prompt_item(f, &labels[i], active)
    }
}

fn open_url(u: &str) {
    match open::that_detached(u) {
        Ok(()) => eprintln!("Opened {u}"),
        Err(e) => eprintln!("warning: could not open browser ({e}); URL: {u}"),
    }
}

/// Shows `plain`/`colored` (same length, same visible text) and returns the chosen index.
fn select(
    prompt: &str,
    header: bool,
    plain: &[String],
    colored: &[String],
    cursor: usize,
    max_length: usize,
) -> Result<Option<usize>, CliError> {
    let theme = ListTheme {
        inner: ColorfulTheme::default(),
        header,
        plain,
        colored,
    };
    let keys: Vec<String> = (0..plain.len()).map(|i| i.to_string()).collect();
    Select::with_theme(&theme)
        .with_prompt(prompt)
        .items(&keys)
        .default(cursor)
        .max_length(max_length)
        .report(false)
        .interact_opt()
        .map_err(CliError::runtime)
}

/// Sub-menu for an issue with two or more MRs. Returns after one open or when the user goes back.
fn choose_mr(
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
    if let Some(j) = select(HELP, false, &items, &items, 0, max_length)? {
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
    // Window line, counts, warnings and the help line sit above dialoguer's redraw region.
    let used = 3 + warning_lines;
    let max_length = term_rows.saturating_sub(used + 1).max(3);

    let now_year = now.with_timezone(&Local).year();
    let rows: Vec<Row> = build_rows(report, &Local, now_year);
    // dialoguer renders on stderr.
    let color = console::colors_enabled_stderr();
    let labels = |color| -> Vec<String> {
        rows.iter()
            .map(|r| format_row(r, Layout::Interactive { width }, color))
            .collect()
    };
    let (plain, colored) = (labels(false), labels(color));
    // Same capacity rule as dialoguer's paging (src/paging.rs).
    let paged = rows.len() > max_length.clamp(3, term_rows.max(3)) - 2;
    let header = format_header(Layout::Interactive { width }, color);
    let header = if paged {
        truncate_str(&header, width.saturating_sub(PAGING_W), "\u{2026}").into_owned()
    } else {
        header
    };

    super::out_line(HELP);
    let mut cursor = 0usize;
    while let Some(i) = select(&header, true, &plain, &colored, cursor, max_length)? {
        cursor = i;
        match &rows[i].target {
            Target::Url(u) => open_url(u),
            Target::ChooseMr { issue_url, mrs } => {
                choose_mr(issue_url, mrs, width, max_length)?;
            }
        }
    }
    Ok(())
}
