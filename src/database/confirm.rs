//! The box that shows SQL which changes the database before it runs: in
//! the Database view and over the code (Run SQL from a file). One
//! component, so both behave the same.
//!
//! Every line of the SQL must have been on screen before Enter runs it: a
//! long batch on a short terminal scrolls, and Enter first says to look at
//! the rest. Only a plain Enter or `y` runs it; with Ctrl or Alt held (Alt+Y
//! is Delete Line) the key does nothing.

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use karet_core::ThemeRole;
use karet_theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

/// What a key did.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Pending,
    Run,
    Cancel,
    /// Enter before all of the SQL was seen: say so.
    NotSeen,
}

pub struct Confirm {
    pub sql: String,
    /// The first wrapped line shown.
    scroll: usize,
    /// Wrapped lines and the rows they get, as at the last draw.
    total: usize,
    rows: usize,
    /// One past the last wrapped line that has been on screen.
    seen: usize,
}

impl Confirm {
    pub fn new(sql: String) -> Self {
        Self {
            sql,
            scroll: 0,
            total: 0,
            rows: 0,
            seen: 0,
        }
    }

    fn seen_all(&self) -> bool {
        self.total > 0 && self.seen >= self.total
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Answer {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let last = self.total.saturating_sub(self.rows);
        let page = self.rows.max(1);
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') if plain => {
                if self.seen_all() {
                    Answer::Run
                } else {
                    Answer::NotSeen
                }
            }
            KeyCode::Esc => Answer::Cancel,
            KeyCode::Char('n') if plain => Answer::Cancel,
            KeyCode::Down => self.scroll_to(self.scroll + 1, last),
            KeyCode::Up => self.scroll_to(self.scroll.saturating_sub(1), last),
            KeyCode::PageDown => self.scroll_to(self.scroll + page, last),
            KeyCode::PageUp => self.scroll_to(self.scroll.saturating_sub(page), last),
            KeyCode::End => self.scroll_to(last, last),
            KeyCode::Home => self.scroll_to(0, last),
            _ => Answer::Pending,
        }
    }

    fn scroll_to(&mut self, scroll: usize, last: usize) -> Answer {
        self.scroll = scroll.min(last);
        Answer::Pending
    }

    /// Hints for the bar under it.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.seen_all() {
            vec![("Enter", "Run it"), ("Esc", "Cancel")]
        } else {
            vec![("Down", "See the rest first"), ("Esc", "Cancel")]
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        let warning = theme.style(ThemeRole::DiagnosticWarning);
        let longest = self
            .sql
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0);
        let width = u16::try_from(longest.saturating_add(4).max(50))
            .unwrap_or(u16::MAX)
            .min(area.width.saturating_sub(4));
        let inner_width = usize::from(width.saturating_sub(2)).max(1);
        let wrapped = wrap(&self.sql, inner_width);
        // Two lines of question above the SQL, one for where you are below.
        let room = usize::from(area.height.saturating_sub(2))
            .saturating_sub(5)
            .max(1);
        self.total = wrapped.len();
        self.rows = room.min(self.total);
        self.scroll = self.scroll.min(self.total.saturating_sub(self.rows));
        self.seen = self.seen.max(self.scroll + self.rows);
        let mut lines = vec![
            Line::styled("This changes the database. Run it?", warning),
            Line::raw(""),
        ];
        lines.extend(
            wrapped
                .iter()
                .skip(self.scroll)
                .take(self.rows)
                .map(|line| Line::raw(line.clone())),
        );
        let footer = if self.total > self.rows {
            let more = if self.seen_all() {
                "all seen"
            } else {
                "Down to see the rest before it can run"
            };
            format!(
                "lines {}-{} of {}: {more}",
                self.scroll + 1,
                self.scroll + self.rows,
                self.total
            )
        } else {
            String::new()
        };
        lines.push(Line::raw(""));
        lines.push(Line::styled(footer, muted));
        let height = u16::try_from(lines.len().saturating_add(2))
            .unwrap_or(u16::MAX)
            .min(area.height);
        let rect = Rect {
            x: area.x + area.width.saturating_sub(width) / 2,
            y: area.y + area.height.saturating_sub(height) / 3,
            width,
            height,
        };
        frame.render_widget(Clear, rect);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(" Run SQL ")
            .style(base);
        frame.render_widget(Paragraph::new(lines).block(block), rect);
    }
}

/// The SQL's lines cut to `width` characters each, so every one shows.
fn wrap(sql: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in sql.lines() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            out.push(String::new());
        }
        for chunk in chars.chunks(width) {
            out.push(chunk.iter().collect());
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn runs_only_after_all_was_seen_and_only_on_a_plain_key() {
        let mut confirm = Confirm::new("delete from t;\n".repeat(30));
        // As if drawn with room for 10 of its 30 lines.
        confirm.total = 30;
        confirm.rows = 10;
        confirm.seen = 10;
        assert_eq!(
            confirm.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE)),
            Answer::NotSeen
        );
        confirm.handle_key(&key(KeyCode::End, KeyModifiers::NONE));
        confirm.seen = confirm.scroll + confirm.rows;
        assert_eq!(
            confirm.handle_key(&key(KeyCode::Char('y'), KeyModifiers::ALT)),
            Answer::Pending
        );
        assert_eq!(
            confirm.handle_key(&key(KeyCode::Char('y'), KeyModifiers::CONTROL)),
            Answer::Pending
        );
        assert_eq!(
            confirm.handle_key(&key(KeyCode::Enter, KeyModifiers::ALT)),
            Answer::Pending
        );
        assert_eq!(
            confirm.handle_key(&key(KeyCode::Enter, KeyModifiers::NONE)),
            Answer::Run
        );
        assert_eq!(
            confirm.handle_key(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Answer::Cancel
        );
    }

    #[test]
    fn wraps_long_lines_so_nothing_is_cut_off() {
        assert_eq!(wrap("abcdef\n\nxy", 4), ["abcd", "ef", "", "xy"]);
    }
}
