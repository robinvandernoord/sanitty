//! The result grid: rows of text cells with a cell cursor, scrolling both
//! ways. Used in the Database view and in the result popup over the code.

use std::time::Duration;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use karet_core::ThemeRole;
use karet_theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;

use super::driver::ResultSet;

/// Columns are at most this wide; longer values are cut.
const MAX_WIDTH: usize = 40;
/// Rows looked at to size the columns.
const SAMPLE_ROWS: usize = 200;

pub struct Grid {
    pub result: ResultSet,
    /// The SQL that gave these rows, to run again.
    pub sql: String,
    pub row: usize,
    pub col: usize,
    top: usize,
    left: usize,
    page: usize,
    widths: Vec<usize>,
}

impl Grid {
    pub fn new(result: ResultSet, sql: String) -> Self {
        let widths = result
            .columns
            .iter()
            .enumerate()
            .map(|(idx, name)| {
                let longest = result
                    .rows
                    .iter()
                    .take(SAMPLE_ROWS)
                    .map(|row| {
                        row.get(idx)
                            .map_or(4, |cell| shown(cell.as_deref()).chars().count())
                    })
                    .max()
                    .unwrap_or(0);
                longest.max(name.chars().count()).clamp(4, MAX_WIDTH)
            })
            .collect();
        Self {
            result,
            sql,
            row: 0,
            col: 0,
            top: 0,
            left: 0,
            page: 10,
            widths,
        }
    }

    /// Keep the cursor where it was (after running the same SQL again).
    pub fn keep_place(&mut self, old: &Self) {
        self.row = old.row.min(self.result.rows.len().saturating_sub(1));
        self.col = old.col.min(self.result.columns.len().saturating_sub(1));
        self.top = old.top;
        self.left = old.left;
    }

    /// The value under the cursor (`None` inside for NULL).
    pub fn cell(&self) -> Option<&Option<String>> {
        self.result
            .rows
            .get(self.row)
            .and_then(|row| row.get(self.col))
    }

    pub fn column_name(&self) -> Option<&str> {
        self.result.columns.get(self.col).map(String::as_str)
    }

    /// What the result was, for a title: "3 rows, 12 ms".
    pub fn summary(&self) -> String {
        summary(&self.result)
    }

    /// Cursor keys. True when the key was one.
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let rows = self.result.rows.len();
        let cols = self.result.columns.len();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Up => self.row = self.row.saturating_sub(1),
            KeyCode::Down => self.row = (self.row + 1).min(rows.saturating_sub(1)),
            KeyCode::Left => self.col = self.col.saturating_sub(1),
            KeyCode::Right => self.col = (self.col + 1).min(cols.saturating_sub(1)),
            KeyCode::PageUp => self.row = self.row.saturating_sub(self.page),
            KeyCode::PageDown => self.row = (self.row + self.page).min(rows.saturating_sub(1)),
            KeyCode::Home if ctrl => self.row = 0,
            KeyCode::End if ctrl => self.row = rows.saturating_sub(1),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = cols.saturating_sub(1),
            _ => return false,
        }
        true
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme, focused: bool) {
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        if self.result.columns.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled(format!("  {}", self.summary()), muted)).style(base),
                area,
            );
            return;
        }
        let height = usize::from(area.height.saturating_sub(1));
        self.page = height.saturating_sub(1).max(1);
        if self.row < self.top {
            self.top = self.row;
        } else if height > 0 && self.row >= self.top + height {
            self.top = self.row + 1 - height;
        }
        let gutter = self.result.rows.len().max(1).to_string().len() + 1;
        let room = usize::from(area.width).saturating_sub(gutter);
        self.scroll_columns(room);
        let visible: Vec<usize> = self.visible_columns(room);
        let selection = theme.style(ThemeRole::Selection).fg.map_or_else(
            || Style::default().add_modifier(Modifier::REVERSED),
            |colour| Style::default().bg(colour),
        );
        let header_style = base.add_modifier(Modifier::BOLD);
        let mut header = vec![Span::styled(" ".repeat(gutter), muted)];
        for &idx in &visible {
            let name = self.result.columns.get(idx).map_or("", String::as_str);
            let width = self.widths.get(idx).copied().unwrap_or(4);
            let style = if focused && idx == self.col {
                header_style.patch(selection)
            } else {
                header_style
            };
            header.push(Span::styled(fit(name, width), style));
            header.push(Span::styled(" \u{2502} ", muted));
        }
        let mut lines = vec![Line::from(header)];
        for (row_idx, row) in self
            .result
            .rows
            .iter()
            .enumerate()
            .skip(self.top)
            .take(height)
        {
            let mut spans = vec![Span::styled(
                format!("{:>width$} ", row_idx + 1, width = gutter - 1),
                muted,
            )];
            for &idx in &visible {
                let width = self.widths.get(idx).copied().unwrap_or(4);
                let value = row.get(idx).and_then(Option::as_deref);
                let mut style = if value.is_none() { muted } else { base };
                if row_idx == self.row && (idx == self.col || !focused) {
                    style = style.patch(selection);
                }
                spans.push(Span::styled(fit(&shown(value), width), style));
                spans.push(Span::styled(" \u{2502} ", muted));
            }
            lines.push(Line::from(spans));
        }
        frame.render_widget(Paragraph::new(lines).style(base), area);
    }

    /// Move the first shown column so the cursor's column fits in `room`.
    fn scroll_columns(&mut self, room: usize) {
        if self.col < self.left {
            self.left = self.col;
            return;
        }
        while self.left < self.col {
            let used: usize = (self.left..=self.col)
                .map(|idx| self.widths.get(idx).copied().unwrap_or(4) + 3)
                .sum();
            if used <= room {
                break;
            }
            self.left += 1;
        }
    }

    fn visible_columns(&self, room: usize) -> Vec<usize> {
        let mut used = 0;
        let mut out = Vec::new();
        for idx in self.left..self.result.columns.len() {
            let width = self.widths.get(idx).copied().unwrap_or(4) + 3;
            if used + width > room && !out.is_empty() {
                break;
            }
            used += width;
            out.push(idx);
        }
        out
    }
}

/// "3 rows, 12 ms", "2 rows changed, 4 ms".
pub fn summary(result: &ResultSet) -> String {
    let time = millis(result.elapsed);
    if result.columns.is_empty() && result.reads {
        return format!("No rows, {time}");
    }
    if result.columns.is_empty() {
        let changed = match result.affected {
            1 => "1 row changed".to_string(),
            count => format!("{count} rows changed"),
        };
        return format!("{changed}, {time}");
    }
    let rows = match result.rows.len() {
        0 => "No rows".to_string(),
        1 => "1 row".to_string(),
        count if result.truncated => format!("first {count} rows"),
        count => format!("{count} rows"),
    };
    format!("{rows}, {time}")
}

fn millis(elapsed: Duration) -> String {
    let total_ms = elapsed.as_millis();
    if total_ms >= 10_000 {
        format!("{:.1} s", elapsed.as_secs_f64())
    } else {
        format!("{total_ms} ms")
    }
}

/// A cell's text on one line: NULL as `NULL`, line breaks as `⏎`, tabs as
/// spaces.
fn shown(value: Option<&str>) -> String {
    match value {
        None => "NULL".into(),
        Some(text) => text
            .replace("\r\n", "\u{23ce}")
            .replace(['\n', '\r'], "\u{23ce}")
            .replace('\t', " "),
    }
}

fn fit(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count > width {
        let cut: String = text.chars().take(width.saturating_sub(1)).collect();
        format!("{cut}\u{2026}")
    } else {
        format!("{text}{}", " ".repeat(width - count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(cols: usize, rows: usize) -> Grid {
        let result = ResultSet {
            columns: (0..cols).map(|idx| format!("column{idx}")).collect(),
            rows: (0..rows)
                .map(|row| (0..cols).map(|col| Some(format!("{row}-{col}"))).collect())
                .collect(),
            ..ResultSet::default()
        };
        Grid::new(result, String::new())
    }

    #[test]
    fn keeps_the_cursor_column_in_view() {
        let mut grid = grid(10, 3);
        // Each column is 7 wide plus 3 for the separator.
        grid.col = 6;
        grid.scroll_columns(25);
        assert_eq!(grid.left, 5);
        assert_eq!(grid.visible_columns(25), [5, 6]);
        grid.col = 1;
        grid.scroll_columns(25);
        assert_eq!(grid.left, 1);
    }

    #[test]
    fn moves_within_the_rows() {
        let mut grid = grid(2, 3);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        grid.handle_key(&key(KeyCode::Down));
        grid.handle_key(&key(KeyCode::PageDown));
        grid.handle_key(&key(KeyCode::Right));
        grid.handle_key(&key(KeyCode::Right));
        assert_eq!((grid.row, grid.col), (2, 1));
        assert_eq!(grid.cell(), Some(&Some("2-1".to_string())));
    }

    #[test]
    fn shows_values_on_one_line() {
        assert_eq!(shown(None), "NULL");
        assert_eq!(shown(Some("a\r\nb\tc")), "a\u{23ce}b c");
        assert_eq!(fit("abcdef", 4), "abc\u{2026}");
    }
}
