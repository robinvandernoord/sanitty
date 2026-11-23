//! All Keys (F1): every action with its key, grouped, filterable; Enter runs it.

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

use crate::keymap::Action;

pub struct AllKeys {
    /// (action, label, group, key as shown)
    rows: Vec<(Action, &'static str, &'static str, String)>,
    query: String,
    selected: usize,
}

pub enum Outcome {
    Pending,
    Close,
    Run(Action),
}

impl AllKeys {
    /// `key_for` gives the key to show for an action (empty when unbound).
    pub fn new(key_for: impl Fn(Action) -> String) -> Self {
        let rows = Action::ALL
            .iter()
            .map(|(action, label, group)| (*action, *label, *group, key_for(*action)))
            .collect();
        Self {
            rows,
            query: String::new(),
            selected: 0,
        }
    }

    fn visible(&self) -> Vec<usize> {
        let needle = self.query.to_lowercase();
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, (_, label, group, key))| {
                needle.is_empty()
                    || label.to_lowercase().contains(&needle)
                    || key.to_lowercase().contains(&needle)
                    || group.to_lowercase().contains(&needle)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let count = self.visible().len();
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Enter => {
                if let Some((action, ..)) = self
                    .visible()
                    .get(self.selected)
                    .and_then(|idx| self.rows.get(*idx))
                {
                    return Outcome::Run(*action);
                }
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(count.saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(count.saturating_sub(1)),
            KeyCode::Backspace => {
                crate::picker::erase(&mut self.query, key);
                self.selected = 0;
            }
            KeyCode::Char(chr) if !ctrl => {
                self.query.push(chr);
                self.selected = 0;
            }
            _ => {}
        }
        Outcome::Pending
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &karet_theme::Theme) {
        let width = 72.min(area.width);
        let height = (area.height * 8 / 10).max(10.min(area.height));
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + area.height.saturating_sub(height) / 2,
            width,
            height,
        };
        frame.render_widget(Clear, rect);
        let base = crate::styles::surface(theme);
        let muted = theme.style(karet_core::ThemeRole::Muted);
        let accent = theme
            .token_style(karet_core::TokenId::FUNCTION)
            .add_modifier(Modifier::BOLD);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(" All Keys ")
            .style(base);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", accent),
                Span::raw(self.query.as_str()),
            ])),
            Rect { height: 1, ..inner },
        );
        let query_width = u16::try_from(self.query.chars().count()).unwrap_or(u16::MAX);
        frame.set_cursor_position((
            inner.x.saturating_add(2).saturating_add(query_width),
            inner.y,
        ));

        // Group headers interleaved with rows; scroll so the selection shows.
        let mut lines: Vec<(Line, bool)> = Vec::new();
        let mut group = "";
        let visible_rows = self
            .visible()
            .into_iter()
            .filter_map(|idx| self.rows.get(idx));
        for (pos, (_, label, row_group, key)) in visible_rows.enumerate() {
            if *row_group != group {
                group = row_group;
                lines.push((
                    Line::styled(format!(" {row_group}"), base.add_modifier(Modifier::BOLD)),
                    false,
                ));
            }
            let style = if pos == self.selected {
                base.add_modifier(Modifier::REVERSED)
            } else {
                base
            };
            lines.push((
                Line::from(vec![
                    Span::styled(
                        format!("   {key:>20}  "),
                        if pos == self.selected { style } else { muted },
                    ),
                    Span::styled(format!("{label:<44}"), style),
                ]),
                pos == self.selected,
            ));
        }
        let body = Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        };
        let sel_line = lines
            .iter()
            .position(|(_, is_selected)| *is_selected)
            .unwrap_or(0);
        let skip = sel_line.saturating_sub(body.height as usize * 2 / 3);
        let shown: Vec<Line> = lines.into_iter().skip(skip).map(|(line, _)| line).collect();
        frame.render_widget(Paragraph::new(shown), body);
    }
}
