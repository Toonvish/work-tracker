//! Interactive list (DESIGN 8.4): a table of tasks with a detail pane for the selected
//! one, drawn with ratatui in an inline viewport below the window and counts lines.
//! The detail pane shows MR descriptions as rendered markdown, with their images.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use chrono::{Datelike, Local, Utc};
use console::truncate_str;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect, Size};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row as TableRow, Table, TableState, Wrap};
use ratatui::{Frame, TerminalOptions, Viewport};
use ratatui_image::Resize;
use ratatui_image::picker::Picker;
use ratatui_image::sliced::{SignedPosition, SlicedImage, SlicedProtocol};

use super::detail::{Item, Kind, Open, items, url_for};
use super::markdown::{ImageRef, Part, render, split};
use super::rows::{
    ELLIPSIS, ISSUE_STATE_W, ISSUE_W, MR_STATE_W, MR_W, NONE, Row, State, StateStyle, UPDATED_W,
    build_rows,
};
use crate::error::CliError;
use crate::report::Report;

const HELP: &str = "\u{2191}/\u{2193} move \u{00B7} PgUp/PgDn: scroll \u{00B7} Tab: zoom \u{00B7} Enter: open MR \u{00B7} i: issue \u{00B7} 1-9: n-th MR \u{00B7} q: quit";
const HIGHLIGHT: &str = "\u{276F} ";
const SPACING: u16 = 2;
/// Narrow terminals drop columns until the title has this much room.
const MIN_TITLE_W: usize = 24;
/// Most text lines of the detail pane when no MR has a description.
const MAX_DETAIL_LINES: usize = 9;
/// Most terminal rows an image takes.
const MAX_IMAGE_ROWS: u16 = 30;
/// Lines scrolled by `J` / `K`.
const SCROLL_STEP: isize = 3;

/// Downloads a file attached to an MR description: `(project id, secret, filename)`.
pub type Fetch = Arc<dyn Fn(u64, &str, &str) -> Result<Vec<u8>, String> + Send + Sync>;

/// `(project id, secret, filename)` of an uploaded image.
type ImageKey = (u64, String, String);

enum Picture {
    Loading,
    Ready(SlicedProtocol),
    Failed(String),
}

/// A vertical piece of the detail pane.
enum Piece {
    Text {
        lines: Vec<Line<'static>>,
        /// Rows at the width the document was built for.
        height: usize,
    },
    /// An uploaded image; shown as one line of text until it is loaded.
    Image { alt: String, key: ImageKey },
}

/// The detail pane content of one row, laid out for one width.
struct Doc {
    index: usize,
    width: u16,
    pieces: Vec<Piece>,
}

struct View {
    rows: Vec<Row>,
    /// Per row: the issue and MRs behind it.
    details: Vec<Vec<Item>>,
    table: TableState,
    /// Result of the last open, shown in place of the help line.
    status: Option<String>,
    color: bool,
    /// Whether any MR has a description: the detail pane then takes all free rows.
    rich: bool,
    /// The detail pane covers the table.
    zoom: bool,
    /// First visible row of the detail pane content.
    scroll: usize,
    /// Rows of the detail pane at the last draw; one page when scrolling.
    page: usize,
    doc: Option<Doc>,
    pictures: HashMap<ImageKey, Picture>,
}

impl View {
    fn new(rows: Vec<Row>, details: Vec<Vec<Item>>, color: bool) -> View {
        let rich = details
            .iter()
            .flatten()
            .any(|i| !i.description.trim().is_empty());
        View {
            rows,
            details,
            table: TableState::new().with_selected(0),
            status: None,
            color,
            rich,
            zoom: false,
            scroll: 0,
            page: 1,
            doc: None,
            pictures: HashMap::new(),
        }
    }

    fn selected(&self) -> usize {
        self.table.selected().unwrap_or(0)
    }

    fn select(&mut self, i: usize) {
        let i = i.min(self.rows.len().saturating_sub(1));
        if i != self.selected() {
            self.scroll = 0;
        }
        self.table.select(Some(i));
    }

    /// Rows of the compact detail pane (no descriptions anywhere) including its border.
    fn compact_detail_height(&self) -> usize {
        let lines = self.details.iter().map(|d| d.len() * 2).max().unwrap_or(0);
        // One spare line for a title that wraps.
        (lines + 1).min(MAX_DETAIL_LINES) + 1
    }

    /// Viewport rows wanted when `free` terminal rows are available.
    fn height(&self, free: usize) -> usize {
        if self.rich {
            free
        } else {
            (1 + self.rows.len() + self.compact_detail_height() + 1).min(free)
        }
    }

    /// Rows of the table (with its header) in a viewport of `total` rows.
    fn table_height(&self, total: u16) -> u16 {
        let want = self.rows.len() as u16 + 1;
        if self.zoom {
            0
        } else if self.rich {
            // The description gets at least three fifths of the viewport.
            want.min((total.saturating_sub(1) * 2 / 5).max(4))
        } else {
            total.saturating_sub(self.compact_detail_height() as u16 + 1)
        }
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

    /// Two lines per item: reference and full title, then state and the rest.
    fn head_lines(&self, item: &Item, number: Option<usize>) -> Vec<Line<'static>> {
        let mut head = Vec::new();
        if let Some(n) = number {
            head.push(Span::styled(format!("{n} "), self.dim()));
        }
        head.push(Span::styled(
            item.reference.clone(),
            self.ref_style(item.kind),
        ));
        head.push(Span::raw("  "));
        head.push(Span::styled(
            item.title.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ));
        let mut tail = vec![
            Span::raw("    "),
            Span::styled(item.state.text.clone(), self.state_style(item.state.style)),
        ];
        for m in &item.meta {
            tail.push(Span::styled(format!(" \u{00B7} {m}"), self.dim()));
        }
        vec![Line::from(head), Line::from(tail)]
    }

    /// Lays out the detail pane of row `index` for `width` columns.
    fn build_doc(&self, index: usize, width: u16) -> Doc {
        fn flush(pieces: &mut Vec<Piece>, lines: &mut Vec<Line<'static>>, width: u16) {
            if !lines.is_empty() {
                let lines = std::mem::take(lines);
                let height = wrapped(lines.clone()).line_count(width.max(1));
                pieces.push(Piece::Text { lines, height });
            }
        }

        let items = &self.details[index];
        let numbered = items.iter().filter(|i| i.kind == Kind::Mr).count() > 1;
        let mut mr_no = 0usize;
        let mut pieces = Vec::new();
        let mut lines: Vec<Line<'static>> = Vec::new();
        for item in items {
            let number = (numbered && item.kind == Kind::Mr).then(|| {
                mr_no += 1;
                mr_no
            });
            let parts = split(&item.description);
            // A described MR is set apart from what precedes it.
            if !parts.is_empty() && !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.extend(self.head_lines(item, number));
            for part in parts {
                lines.push(Line::default());
                match part {
                    Part::Text(markdown) => lines.extend(render(&markdown, width)),
                    Part::Image { alt, source } => match (source, item.project_id) {
                        (ImageRef::Upload { secret, filename }, Some(project)) => {
                            flush(&mut pieces, &mut lines, width);
                            pieces.push(Piece::Image {
                                alt,
                                key: (project, secret, filename),
                            });
                        }
                        (ImageRef::Upload { filename, .. }, None) => {
                            lines.push(Line::styled(
                                format!("[image] {alt} {filename}"),
                                self.dim(),
                            ));
                        }
                        (ImageRef::External(url), _) => {
                            lines.push(Line::styled(format!("[image] {alt} {url}"), self.dim()));
                        }
                    },
                }
            }
        }
        flush(&mut pieces, &mut lines, width);
        Doc {
            index,
            width,
            pieces,
        }
    }

    fn piece_height(&self, piece: &Piece) -> usize {
        match piece {
            Piece::Text { height, .. } => *height,
            Piece::Image { key, .. } => match self.pictures.get(key) {
                Some(Picture::Ready(p)) => usize::from(p.size().height).max(1),
                _ => 1,
            },
        }
    }

    /// Images of the selected row that have not been requested yet.
    fn missing_images(&self) -> Vec<ImageKey> {
        let Some(doc) = &self.doc else {
            return Vec::new();
        };
        let mut keys = Vec::new();
        for piece in &doc.pieces {
            if let Piece::Image { key, .. } = piece
                && !self.pictures.contains_key(key)
                && !keys.contains(key)
            {
                keys.push(key.clone());
            }
        }
        keys
    }
}

fn wrapped(lines: Vec<Line<'_>>) -> Paragraph<'_> {
    Paragraph::new(lines).wrap(Wrap { trim: false })
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

fn draw_table(frame: &mut Frame, v: &mut View, area: Rect) {
    let columns = Columns::for_width(usize::from(area.width));
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
    frame.render_stateful_widget(table, area, &mut state);
    v.table = state;
}

/// Draws the part of the selected row's document that is scrolled into `area`.
fn draw_doc(frame: &mut Frame, v: &View, doc: &Doc, area: Rect) {
    let bottom = area.height as isize;
    let mut top = -(v.scroll as isize);
    for piece in &doc.pieces {
        let height = v.piece_height(piece) as isize;
        let (from, to) = (top.max(0), (top + height).min(bottom));
        let piece_top = top;
        top += height;
        if from >= to {
            continue;
        }
        let visible = Rect {
            y: area.y + from as u16,
            height: (to - from) as u16,
            ..area
        };
        let one_line = |text: String| Line::styled(text, v.dim());
        match piece {
            Piece::Text { lines, .. } => {
                let skipped = (from - piece_top) as u16;
                frame.render_widget(wrapped(lines.clone()).scroll((skipped, 0)), visible);
            }
            Piece::Image { alt, key } => match v.pictures.get(key) {
                Some(Picture::Ready(picture)) => {
                    let position = SignedPosition::from((0, piece_top as i16));
                    frame.render_widget(SlicedImage::new(picture, position), area);
                }
                Some(Picture::Failed(why)) => {
                    let text = format!("[image not available: {why}] {alt} {}", key.2);
                    frame.render_widget(one_line(text), visible);
                }
                Some(Picture::Loading) | None => {
                    let text = format!("[loading image\u{2026}] {alt} {}", key.2);
                    frame.render_widget(one_line(text), visible);
                }
            },
        }
    }
}

fn draw(frame: &mut Frame, v: &mut View) {
    let total = frame.area();
    let [table_area, detail_area, help_area] = Layout::vertical([
        Constraint::Length(v.table_height(total.height)),
        Constraint::Min(2),
        Constraint::Length(1),
    ])
    .areas(total);

    if table_area.height > 0 {
        draw_table(frame, v, table_area);
    }

    let selected = v.selected();
    let block = Block::new().borders(Borders::TOP).border_style(v.dim());
    let inner = block.inner(detail_area);
    if !v
        .doc
        .as_ref()
        .is_some_and(|d| d.index == selected && d.width == inner.width)
    {
        v.doc = Some(v.build_doc(selected, inner.width));
    }
    let doc = v.doc.take().expect("the document was just built");
    let content: usize = doc.pieces.iter().map(|p| v.piece_height(p)).sum();
    let page = usize::from(inner.height);
    v.page = page.max(1);
    v.scroll = v.scroll.min(content.saturating_sub(page));

    let mut position = format!(" {}/{} ", selected + 1, v.rows.len());
    if content > page {
        position = format!(
            " lines {}-{} of {content} \u{00B7}{position}",
            v.scroll + 1,
            (v.scroll + page).min(content),
        );
    }
    frame.render_widget(
        block.title(Line::from(position).right_aligned()),
        detail_area,
    );
    draw_doc(frame, v, &doc, inner);
    v.doc = Some(doc);

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
    /// Scroll the detail pane by lines.
    Scroll(isize),
    /// Scroll the detail pane by pages.
    Page(isize),
    Zoom,
    Open(Open),
    None,
}

fn action(code: KeyCode, modifiers: KeyModifiers) -> Action {
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    match code {
        KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => Action::Quit,
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Up if shift => Action::Scroll(-SCROLL_STEP),
        KeyCode::Down if shift => Action::Scroll(SCROLL_STEP),
        KeyCode::Char('K') => Action::Scroll(-SCROLL_STEP),
        KeyCode::Char('J') => Action::Scroll(SCROLL_STEP),
        KeyCode::Up | KeyCode::Char('k') => Action::Move(-1),
        KeyCode::Down | KeyCode::Char('j') => Action::Move(1),
        KeyCode::PageUp => Action::Page(-1),
        KeyCode::PageDown | KeyCode::Char(' ') => Action::Page(1),
        KeyCode::Tab | KeyCode::Char('z') => Action::Zoom,
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
        // `draw` clamps the offset to the content.
        Action::Scroll(d) => v.scroll = v.scroll.saturating_add_signed(d),
        Action::Page(d) => {
            let page = v.page.saturating_sub(1).max(1) as isize;
            v.scroll = v.scroll.saturating_add_signed(d * page);
        }
        Action::Zoom => v.zoom = !v.zoom,
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

/// Downloads, decodes and encodes one image for the terminal on its own thread.
fn load_picture(
    fetch: &Fetch,
    picker: &Picker,
    key: &ImageKey,
    size: Size,
) -> Result<SlicedProtocol, String> {
    let bytes = fetch(key.0, &key.1, &key.2)?;
    let image = image::load_from_memory(&bytes).map_err(|e| e.to_string())?;
    SlicedProtocol::new_with_resize(picker, image, size, Resize::Fit(None))
        .map_err(|e| e.to_string())
}

/// Background image loading for the selected row.
struct Loader {
    fetch: Fetch,
    picker: Picker,
    tx: Sender<(ImageKey, Result<SlicedProtocol, String>)>,
    rx: Receiver<(ImageKey, Result<SlicedProtocol, String>)>,
}

impl Loader {
    /// Starts loading the images the selected row still lacks. True when any was started.
    fn start(&self, v: &mut View) -> bool {
        let keys = v.missing_images();
        let width = v.doc.as_ref().map_or(80, |d| d.width);
        for key in &keys {
            v.pictures.insert(key.clone(), Picture::Loading);
            let (fetch, picker, tx, key) = (
                self.fetch.clone(),
                self.picker.clone(),
                self.tx.clone(),
                key.clone(),
            );
            let size = Size::new(width.max(1), MAX_IMAGE_ROWS);
            std::thread::spawn(move || {
                let result = load_picture(&fetch, &picker, &key, size);
                let _ = tx.send((key, result));
            });
        }
        !keys.is_empty()
    }

    /// Stores the images that finished loading. True when any arrived.
    fn receive(&self, v: &mut View) -> bool {
        let mut any = false;
        while let Ok((key, result)) = self.rx.try_recv() {
            let picture = match result {
                Ok(p) => Picture::Ready(p),
                Err(why) => Picture::Failed(why),
            };
            v.pictures.insert(key, picture);
            any = true;
        }
        any
    }
}

/// Whether any MR description refers to an uploaded image.
fn has_images(details: &[Vec<Item>]) -> bool {
    details.iter().flatten().any(|item| {
        split(&item.description).iter().any(|p| {
            matches!(
                p,
                Part::Image {
                    source: ImageRef::Upload { .. },
                    ..
                }
            )
        })
    })
}

/// Runs the list until the user quits with Esc or `q`.
/// `warning_lines` is the number of warning lines already printed above the list;
/// `fetch` downloads the images of MR descriptions (none without GitLab).
pub fn run(
    report: &Report,
    now: chrono::DateTime<Utc>,
    warning_lines: usize,
    fetch: Option<Fetch>,
) -> Result<(), CliError> {
    let now_year = now.with_timezone(&Local).year();
    let mut view = View::new(
        build_rows(report, &Local, now_year),
        report
            .entries
            .iter()
            .map(|e| items(e, &Local, now_year))
            .collect(),
        console::colors_enabled(),
    );

    // Keep the window line, the counts and the warnings visible above the list.
    let (term_rows, _) = console::Term::stdout().size();
    let term_rows = usize::from(term_rows);
    let free = term_rows.saturating_sub(2 + warning_lines + 1).max(8);
    let height = view.height(free).min(term_rows.max(1));

    let mut terminal = ratatui::try_init_with_options(TerminalOptions {
        viewport: Viewport::Inline(height as u16),
    })
    .map_err(CliError::runtime)?;
    // Asking the terminal for its image protocol needs raw mode, and must precede event reads.
    let loader = fetch.filter(|_| has_images(&view.details)).map(|fetch| {
        let (tx, rx) = channel();
        Loader {
            fetch,
            picker: Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks()),
            tx,
            rx,
        }
    });
    let result = (|| -> std::io::Result<()> {
        let mut dirty = true;
        loop {
            if dirty {
                terminal.draw(|frame| draw(frame, &mut view))?;
                dirty = false;
            }
            if event::poll(Duration::from_millis(100))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if !handle(&mut view, action(key.code, key.modifiers)) {
                            break;
                        }
                        dirty = true;
                    }
                    Event::Resize(..) => dirty = true,
                    _ => {}
                }
            } else if let Some(loader) = &loader {
                // Idle: the selection has settled, so its images are worth loading.
                dirty |= loader.start(&mut view);
            }
            if let Some(loader) = &loader {
                dirty |= loader.receive(&mut view);
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
        View::new(
            build_rows(&rep, &Berlin, 2026),
            rep.entries
                .iter()
                .map(|e| items(e, &Berlin, 2026))
                .collect(),
            true,
        )
    }

    const SECRET: &str = "b2259262421e0480821f75988977bd75";

    /// One issue whose MR has a markdown description with an uploaded image.
    fn described() -> View {
        let u = t(2026, 9, 30, 9, 0);
        let mut m = mr(30, "g/a", "feat: Preview", u);
        m.project_id = 502;
        m.description = format!(
            "## What changed\n\n- first **bold** item\n- second item\n\n![shot](/uploads/{SECRET}/shot.png)\n\nAfter the image.\n\n{}",
            (1..=30)
                .map(|i| format!("Paragraph {i}."))
                .collect::<Vec<_>>()
                .join("\n\n")
        );
        let rep = report(vec![
            Entry::Issue {
                issue: issue("SP-1", "Login broken", u),
                mrs: vec![m],
            },
            Entry::OrphanMr(mr(9, "g/orphan", "Lonely", u)),
        ]);
        View::new(
            build_rows(&rep, &Berlin, 2026),
            rep.entries
                .iter()
                .map(|e| items(e, &Berlin, 2026))
                .collect(),
            true,
        )
    }

    fn key() -> ImageKey {
        (502, SECRET.to_string(), "shot.png".to_string())
    }

    fn screen_of(v: &mut View, width: u16, height: u16) -> Vec<String> {
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

    fn screen(v: &mut View, width: u16) -> Vec<String> {
        let height = v.height(100) as u16;
        screen_of(v, width, height)
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
        assert!(HELP.starts_with(s.last().unwrap().as_str()), "{s:#?}");
        assert!(!v.rich);
    }

    #[test]
    fn description_is_rendered_as_markdown_below_the_mr() {
        let mut v = described();
        assert!(v.rich);
        assert_eq!(v.height(40), 40, "a description takes all free rows");
        let s = screen_of(&mut v, 80, 30);
        // Two rows and the header: the table is as small as its content.
        assert!(s[1].starts_with("\u{276F} SP-1"), "{s:#?}");
        assert!(s[3].contains("lines 1-25 of"), "{s:#?}");
        assert_eq!(s[4], "SP-1  Login broken");
        assert_eq!(s[7], "g/a!30  feat: Preview");
        let text = s.join("\n");
        assert!(text.contains("What changed"), "{text}");
        assert!(
            text.contains("first bold item"),
            "markup is styled away: {text}"
        );
        assert!(
            text.contains("[loading image\u{2026}] shot shot.png"),
            "{text}"
        );
        assert!(text.contains("After the image."), "{text}");
        assert_eq!(v.missing_images(), vec![key()]);
    }

    #[test]
    fn detail_scrolls_by_page_and_line_and_resets_on_move() {
        let mut v = described();
        screen_of(&mut v, 80, 30);
        assert_eq!(v.page, 25);
        handle(&mut v, action(KeyCode::PageDown, KeyModifiers::NONE));
        assert_eq!(v.scroll, 24);
        let s = screen_of(&mut v, 80, 30);
        assert!(s[3].contains("lines 25-49 of"), "{s:#?}");
        assert!(!s.join("\n").contains("What changed"), "{s:#?}");
        handle(&mut v, action(KeyCode::Char('K'), KeyModifiers::SHIFT));
        assert_eq!(v.scroll, 21);
        handle(&mut v, action(KeyCode::Down, KeyModifiers::SHIFT));
        assert_eq!(v.scroll, 24);
        // The offset is clamped to the end of the content at the next draw.
        for _ in 0..10 {
            handle(&mut v, action(KeyCode::Char(' '), KeyModifiers::NONE));
        }
        let s = screen_of(&mut v, 80, 30);
        assert!(s.join("\n").contains("Paragraph 30."), "{s:#?}");
        assert!(s[3].contains(&format!("-{} of", v.scroll + 25)), "{s:#?}");
        handle(&mut v, action(KeyCode::PageUp, KeyModifiers::NONE));
        handle(&mut v, action(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!((v.selected(), v.scroll), (1, 0));
    }

    #[test]
    fn zoom_gives_the_whole_viewport_to_the_detail() {
        let mut v = described();
        handle(&mut v, action(KeyCode::Tab, KeyModifiers::NONE));
        let s = screen_of(&mut v, 80, 30);
        assert!(s[0].contains("lines 1-28 of"), "{s:#?}");
        assert_eq!(s[1], "SP-1  Login broken");
        handle(&mut v, action(KeyCode::Char('z'), KeyModifiers::NONE));
        let s = screen_of(&mut v, 80, 30);
        assert!(s[0].starts_with("  ISSUE"), "{s:#?}");
    }

    #[test]
    fn loaded_image_takes_its_rows_and_scrolls_with_the_text() {
        let mut v = described();
        let before = screen_of(&mut v, 80, 30);
        let line_of = |s: &[String], needle: &str| {
            s.iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} not in {s:#?}"))
        };
        let after_text = line_of(&before, "After the image.");

        let picker = Picker::halfblocks();
        let red = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            40,
            80,
            image::Rgb([200, 0, 0]),
        ));
        let picture =
            SlicedProtocol::new_with_resize(&picker, red, Size::new(80, 6), Resize::Fit(None))
                .unwrap();
        let rows = usize::from(picture.size().height);
        assert!((2..=6).contains(&rows), "{rows}");
        v.pictures.insert(key(), Picture::Ready(picture));
        assert!(v.missing_images().is_empty());

        let s = screen_of(&mut v, 80, 30);
        assert!(!s.join("\n").contains("loading image"), "{s:#?}");
        // The text after the image moved down by the image's extra rows.
        assert_eq!(line_of(&s, "After the image."), after_text + rows - 1);

        v.pictures.insert(key(), Picture::Failed("HTTP 404".into()));
        let s = screen_of(&mut v, 80, 30);
        assert!(
            s.join("\n")
                .contains("[image not available: HTTP 404] shot shot.png"),
            "{s:#?}"
        );
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
        handle(&mut v, action(KeyCode::Char('j'), KeyModifiers::NONE));
        handle(&mut v, action(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(v.selected(), 2);
        handle(&mut v, action(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(v.selected(), 1);
        handle(&mut v, action(KeyCode::Up, KeyModifiers::NONE));
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
