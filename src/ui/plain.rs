//! Static table output (DESIGN 8.3, plain layout).

use std::io::{IsTerminal, Write};

use chrono::{Datelike, Local, Utc};

use super::rows::{build_rows, format_header, format_row, plain_layout};
use crate::report::Report;

/// Prints a column header, then one line per row. Colour only on a TTY that allows it.
/// A closed pipe (for example `| head`) ends the output quietly.
pub fn print(report: &Report, now: chrono::DateTime<Utc>) {
    let now_year = now.with_timezone(&Local).year();
    let rows = build_rows(report, &Local, now_year);
    let layout = plain_layout(&rows);
    let color = std::io::stdout().is_terminal() && console::colors_enabled();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let lines = std::iter::once(format_header(layout, color))
        .chain(rows.iter().map(|row| format_row(row, layout, color)));
    for line in lines {
        if writeln!(out, "{line}").is_err() {
            break;
        }
    }
}
