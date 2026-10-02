//! Static table output (DESIGN 8.3, plain layout).

use std::io::{IsTerminal, Write};

use chrono::{Datelike, Local, Utc};

use super::rows::{Layout, build_rows, format_row, plain_title_width};
use crate::report::Report;

/// Prints one line per row. Colour only on a TTY that allows it.
/// A closed pipe (for example `| head`) ends the output quietly.
pub fn print(report: &Report, now: chrono::DateTime<Utc>) {
    let now_year = now.with_timezone(&Local).year();
    let rows = build_rows(report, &Local, now_year);
    let layout = Layout::Plain {
        title_width: plain_title_width(&rows),
    };
    let color = std::io::stdout().is_terminal() && console::colors_enabled();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for row in &rows {
        if writeln!(out, "{}", format_row(row, layout, color)).is_err() {
            break;
        }
    }
}
