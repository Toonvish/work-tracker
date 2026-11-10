//! Interactive list (DESIGN 8.4): a table of tasks with a detail pane for the selected
//! one, drawn with ratatui in an inline viewport below the window and counts lines.

use chrono::{Datelike, Local, Utc};
use console::truncate_str;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row as TableRow, Table, TableState, Wrap};
use ratatui::{Frame, TerminalOptions, Viewport};

use super::detail::{Item, Kind, Open, items, url_for};
use super::rows::{
    ELLIPSIS, ISSUE_STATE_W, ISSUE_W, MR_STATE_W, MR_W, NONE, Row, State, StateStyle, UPDATED_W,
    build_rows,
};
use crate::error::CliError;
use crate::report::Report;

const HELP: &str = "\u{2191}/\u{2193} move \u{00B7} Enter: open MR \u{00B7} i: open issue \u{00B7} 1-9: open n-th MR \u{00B7} q/Esc: quit";
const HIGHLIGHT: &str = "\u{276F} ";
const SPACING: u16 = 2;
/// Narrow terminals drop columns until the title has this much room.
const MIN_TITLE_W: usize = 24;
/// Most text lines of the detail pane.
const MAX_DETAIL_LINES: usize = 9;

struct View {
    rows: Vec<Row>,
    /// Per row: the issue and MRs behind it.
    details: Vec<Vec<Item>>,
    table: TableState,
    /// Result of the last open, shown in place of the help line.
    status: Option<String>,
    color: bool,
}

impl View {
    fn selected(&self) -> usize {
        self.table.selected().unwrap_or(0)
    }

    fn select(&mut self, i: usize) {
        self.table
            .select(Some(i.min(self.rows.len().saturating_sub(1))));
    }

    /// Rows of the detail pane including its border.
    fn detail_height(&self) -> usize {
        let lines = self.details.iter().map(|d| d.len() * 2).max().unwrap_or(0);
        // One spare line for a title that wraps.
        (lines + 1).min(MAX_DETAIL_LINES) + 1
    }

    /// Viewport rows wanted: column header, every row, the detail pane and the help line.
    fn height(&self) -> usize {
        1 + self.rows.len() + self.detail_height() + 1
    }

    fn fg(&self, style: Style, color: Color) -> Style {
        if self.color { style.fg(color) } else { style }
    }

    fn dim(&self) -> Style {
        Style::new().add_modifier(Modifier::DIM)
    }

    fn state_style(&self, state: StateStyle) -> Style {
        let s = Style::new();
        match state {
            StateStyle::Merged => self.fg(s, Color::Magenta),
            StateStyle::Opened => self.fg(s, Color::Green),
            StateStyle::Draft => self.fg(s, Color::Yellow),
            StateStyle::Closed => self.fg(s, Color::Red),
            StateStyle::IssueOpen => s,
            StateStyle::IssueResolved => self.dim(),
        }
    }

    fn ref_style(&self, kind: Kind) -> Style {
        match kind {
            Kind::Issue => self.fg(Style::new().add_modifier(Modifier::BOLD), Color::Cyan),
            Kind::Mr => self.fg(Style::new(), Color::LightBlue),
        }
    }
}

fn or_none<'a>(text: &'a str, style: Style, dim: Style) -> Cell<'a> {
    if text.is_empty() {
        Cell::new(NONE).style(dim)
    } else {
        Cell::new(text).style(style)
    }
}

fn state_cell<'a>(v: &View, state: Option<&'a State>) -> Cell<'a> {
    match state {
        Some(s) => Cell::new(s.text.as_str()).style(v.state_style(s.style)),
        None => Cell::new(NONE).style(v.dim()),
    }
}

/// Which optional columns fit, and the width left for the title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    mr_state: bool,
    updated: bool,
    title_w: usize,
}

impl Columns {
    /// Drops UPDATED, then MR STATE, while the title would be narrower than `MIN_TITLE_W`.
    fn for_width(width: usize) -> Columns {
        let gap = SPACING as usize;
        let base = 2 + ISSUE_W + gap + MR_W + gap + gap + ISSUE_STATE_W;
        let (mr_state, updated) = (MR_STATE_W + gap, UPDATED_W + gap);
        let fits = |fixed: usize| width.saturating_sub(fixed) >= MIN_TITLE_W;
        let (show_mr_state, show_updated) = if fits(base + mr_state + updated) {
            (true, true)
        } else {
            (fits(base + mr_state), false)
        };
        let fixed = base
            + if show_mr_state { mr_state } else { 0 }
            + if show_updated { updated } else { 0 };
        Columns {
            mr_state: show_mr_state,
            updated: show_updated,
            title_w: width.saturating_sub(fixed),
        }
    }

    /// Keeps the cells of the columns that are shown. `all` is in column order.
    fn pick<T>(self, all: [T; 6]) -> Vec<T> {
        let shown = [true, true, true, true, self.mr_state, self.updated];
        all.into_iter()
            .zip(shown)
            .filter_map(|(cell, on)| on.then_some(cell))
            .collect()
    }
}

/// The selected row is drawn without colours so the highlight covers it evenly.
fn table_row<'a>(v: &View, row: &'a Row, columns: Columns, selected: bool) -> TableRow<'a> {
    let title = truncate_str(&row.title, columns.title_w, ELLIPSIS);
    let cells = columns.pick([
        or_none(&row.issue, v.ref_style(Kind::Issue), v.dim()),
        or_none(&row.mr, v.ref_style(Kind::Mr), v.dim()),
        Cell::new(title),
        state_cell(v, row.issue_state.as_ref()),
        state_cell(v, row.mr_state.as_ref()),
        Cell::new(row.updated.as_str()).style(v.dim()),
    ]);
    if selected {
        TableRow::new(cells.into_iter().map(|c| c.style(Style::reset())))
    } else {
        TableRow::new(cells)
    }
}

/// Two lines per item: reference and full title, then state and the rest.
fn detail_lines<'a>(v: &View, items: &'a [Item]) -> Vec<Line<'a>> {
    let numbered = items.iter().filter(|i| i.kind == Kind::Mr).count() > 1;
    let mut mr_no = 0usize;
    let mut lines = Vec::new();
    for item in items {
        let mut head = Vec::new();
        if numbered && item.kind == Kind::Mr {
            mr_no += 1;
            head.push(Span::styled(format!("{mr_no} "), v.dim()));
        }
        head.push(Span::styled(
            item.reference.as_str(),
            v.ref_style(item.kind),
        ));
        head.push(Span::raw("  "));
        head.push(Span::styled(
            item.title.as_str(),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        lines.push(Line::from(head));

        let mut tail = vec![
            Span::raw("    "),
            Span::styled(item.state.text.as_str(), v.state_style(item.state.style)),
        ];
        for m in &item.meta {
            tail.push(Span::styled(format!(" \u{00B7} {m}"), v.dim()));
        }
        lines.push(Line::from(tail));
    }
    lines
}

fn draw(frame: &mut Frame, v: &mut View) {
    let [table_area, detail_area, help_area] = Layout::vertical([
        Constraint::Min(2),
        Constraint::Length(v.detail_height() as u16),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let columns = Columns::for_width(usize::from(table_area.width));
    let selected = v.selected();
    let rows: Vec<TableRow> = v
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| table_row(v, r, columns, i == selected))
        .collect();
    let header =
        TableRow::new(columns.pick(["ISSUE", "MR", "TITLE", "ISSUE STATE", "MR STATE", "UPDATED"]))
            .style(Style::new().add_modifier(Modifier::BOLD));
    let table = Table::new(
        rows,
        columns.pick([
            Constraint::Length(ISSUE_W as u16),
            Constraint::Length(MR_W as u16),
            Constraint::Fill(1),
            Constraint::Length(ISSUE_STATE_W as u16),
            Constraint::Length(MR_STATE_W as u16),
            Constraint::Length(UPDATED_W as u16),
        ]),
    )
    .header(header)
    .column_spacing(SPACING)
    .highlight_symbol(HIGHLIGHT)
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = v.table;
    frame.render_stateful_widget(table, table_area, &mut state);
    v.table = state;

    let position = format!(" {}/{} ", selected + 1, v.rows.len());
    let detail = Paragraph::new(detail_lines(v, &v.details[selected]))
        .wrap(Wrap { trim: false })
        .block(
            Block::new()
                .borders(Borders::TOP)
                .border_style(v.dim())
                .title(Line::from(position).right_aligned()),
        );
    frame.render_widget(detail, detail_area);

    let help = match &v.status {
        Some(status) => Line::from(status.as_str()),
        None => Line::styled(HELP, v.dim()),
    };
    frame.render_widget(help, help_area);
}

fn open_url(u: &str) -> String {
    match open::that_detached(u) {
        Ok(()) => format!("Opened {u}"),
        Err(e) => format!("Could not open the browser ({e}): {u}"),
    }
}

enum Action {
    Quit,
    Move(isize),
    First,
    Last,
    Open(Open),
    None,
}

fn action(code: KeyCode, modifiers: KeyModifiers) -> Action {
    match code {
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Up | KeyCode::Char('k') => Action::Move(-1),
        KeyCode::Down | KeyCode::Char('j') => Action::Move(1),
        KeyCode::PageUp => Action::Move(-10),
        KeyCode::PageDown => Action::Move(10),
        KeyCode::Home | KeyCode::Char('g') => Action::First,
        KeyCode::End | KeyCode::Char('G') => Action::Last,
        KeyCode::Enter => Action::Open(Open::Default),
        KeyCode::Char('i') => Action::Open(Open::Issue),
        KeyCode::Char(c @ '1'..='9') => Action::Open(Open::Mr(c as usize - '0' as usize)),
        _ => Action::None,
    }
}

/// Applies a key press. Returns false when the user quits.
fn handle(v: &mut View, action: Action) -> bool {
    match action {
        Action::Quit => return false,
        Action::Move(d) => {
            v.select(v.selected().saturating_add_signed(d));
            v.status = None;
        }
        Action::First => v.select(0),
        Action::Last => v.select(usize::MAX),
        Action::Open(open) => {
            v.status = Some(match url_for(&v.details[v.selected()], open) {
                Ok(url) => open_url(url),
                Err(why) => why.to_string(),
            });
        }
        Action::None => {}
    }
    true
}

/// Runs the list until the user quits with Esc or `q`.
/// `warning_lines` is the number of warning lines already printed above the list.
pub fn run(
    report: &Report,
    now: chrono::DateTime<Utc>,
    warning_lines: usize,
) -> Result<(), CliError> {
    let now_year = now.with_timezone(&Local).year();
    let mut view = View {
        rows: build_rows(report, &Local, now_year),
        details: report
            .entries
            .iter()
            .map(|e| items(e, &Local, now_year))
            .collect(),
        table: TableState::new().with_selected(0),
        status: None,
        color: console::colors_enabled(),
    };

    // Keep the window line, the counts and the warnings visible above the list.
    let (term_rows, _) = console::Term::stdout().size();
    let term_rows = usize::from(term_rows);
    let free = term_rows.saturating_sub(2 + warning_lines + 1);
    let height = view.height().min(free.max(8)).min(term_rows.max(1));

    let mut terminal = ratatui::try_init_with_options(TerminalOptions {
        viewport: Viewport::Inline(height as u16),
    })
    .map_err(CliError::runtime)?;
    let result = (|| -> std::io::Result<()> {
        loop {
            terminal.draw(|frame| draw(frame, &mut view))?;
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && !handle(&mut view, action(key.code, key.modifiers))
            {
                break;
            }
        }
        // Leave nothing behind: the shell prompt returns right below the counts line.
        let top = terminal.get_frame().area().y;
        terminal.clear()?;
        terminal.set_cursor_position((0, top))
    })();
    ratatui::restore();
    result.map_err(CliError::runtime)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::Entry;
    use crate::ui::fixtures::{issue, mr, report, t};
    use chrono_tz::Europe::Berlin;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn view() -> View {
        let u = t(2026, 9, 30, 9, 0);
        let long = "feat(auth): Keep the session when the token is refreshed in the background";
        let rep = report(vec![
            Entry::Issue {
                issue: issue("SP-1", "Login broken", u),
                mrs: vec![mr(30, "g/a", long, u), mr(31, "g/b", "second", u)],
            },
            Entry::Issue {
                issue: issue("SP-2", "No MR", u),
                mrs: vec![],
            },
            Entry::OrphanMr(mr(9, "g/orphan", "Lonely", u)),
        ]);
        View {
            rows: build_rows(&rep, &Berlin, 2026),
            details: rep
                .entries
                .iter()
                .map(|e| items(e, &Berlin, 2026))
                .collect(),
            table: TableState::new().with_selected(0),
            status: None,
            color: true,
        }
    }

    fn screen(v: &mut View, width: u16) -> Vec<String> {
        let height = v.height() as u16;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| draw(f, v)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                let line: String = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
                line.trim_end().to_string()
            })
            .collect()
    }

    #[test]
    fn table_and_detail_of_the_selected_row() {
        let mut v = view();
        let s = screen(&mut v, 100);
        assert!(
            s[0].starts_with("  ISSUE      MR                TITLE"),
            "{s:#?}"
        );
        assert!(s[0].ends_with("ISSUE STATE   MR STATE  UPDATED"), "{s:#?}");
        assert!(s[1].starts_with("\u{276F} SP-1       a!30 +1           Login broken"));
        assert!(
            s[1].ends_with("In Progress   opened    09-30 11:00"),
            "{s:#?}"
        );
        assert!(
            s[2].starts_with("  SP-2       -                 No MR"),
            "{s:#?}"
        );
        assert!(
            s[3].starts_with("  -          orphan!9          Lonely"),
            "{s:#?}"
        );
        assert!(s[4].ends_with(" 1/3"), "{s:#?}");
        assert_eq!(s[5], "SP-1  Login broken");
        assert_eq!(
            s[6],
            "    In Progress \u{00B7} assignee \u{00B7} updated 09-30 11:00"
        );
        // The whole MR title, with a number because there are two MRs.
        assert_eq!(
            s[7],
            "1 g/a!30  feat(auth): Keep the session when the token is refreshed in the background"
        );
        assert_eq!(
            s[8],
            "    opened \u{00B7} author \u{00B7} updated 09-30 11:00 \u{00B7} feature"
        );
        assert_eq!(s[9], "2 g/b!31  second");
        assert_eq!(s.last().unwrap(), HELP);
    }

    #[test]
    fn long_titles_wrap_in_the_detail_pane() {
        let mut v = view();
        let s = screen(&mut v, 70).join("\n");
        assert!(s.contains("refreshed in\nthe background"), "{s}");
    }

    #[test]
    fn narrow_terminals_drop_columns_for_the_title() {
        let wide = Columns::for_width(120);
        assert_eq!(
            (wide.mr_state, wide.updated, wide.title_w),
            (true, true, 52)
        );
        let mid = Columns::for_width(80);
        assert_eq!((mid.mr_state, mid.updated, mid.title_w), (true, false, 25));
        let narrow = Columns::for_width(70);
        assert_eq!((narrow.mr_state, narrow.updated), (false, false));
        assert_eq!(narrow.title_w, 25);

        let mut v = view();
        let s = screen(&mut v, 80);
        assert!(s[0].ends_with("ISSUE STATE   MR STATE"), "{s:#?}");
        assert!(s[1].contains("Login broken"), "{s:#?}");
        assert!(s[1].ends_with("In Progress   opened"), "{s:#?}");
    }

    #[test]
    fn moving_selects_and_clamps() {
        let mut v = view();
        assert!(handle(&mut v, action(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(v.selected(), 1);
        let s = screen(&mut v, 100);
        assert!(s[2].starts_with("\u{276F} SP-2"), "{s:#?}");
        assert_eq!(s[5], "SP-2  No MR");
        handle(&mut v, action(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(v.selected(), 2);
        handle(&mut v, action(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(v.selected(), 1);
        handle(&mut v, action(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(v.selected(), 0);
        handle(&mut v, action(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(v.selected(), 2);
        handle(&mut v, action(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(v.selected(), 0);
    }

    #[test]
    fn quit_keys_and_missing_targets() {
        let mut v = view();
        for (code, m) in [
            (KeyCode::Char('q'), KeyModifiers::NONE),
            (KeyCode::Esc, KeyModifiers::NONE),
            (KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            assert!(!handle(&mut v, action(code, m)));
        }
        // Nothing is opened for an MR number that does not exist; the reason is shown.
        assert!(handle(
            &mut v,
            action(KeyCode::Char('7'), KeyModifiers::NONE)
        ));
        let s = screen(&mut v, 100);
        assert_eq!(s.last().unwrap(), "No merge request with that number");
        handle(&mut v, action(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(v.status, None);
    }
}
