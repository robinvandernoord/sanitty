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
            .map(|(a, label, group)| (*a, *label, *group, key_for(*a)))
            .collect();
        Self {
            rows,
            query: String::new(),
            selected: 0,
        }
    }

    fn visible(&self) -> Vec<usize> {
        let q = self.query.to_lowercase();
        (0..self.rows.len())
            .filter(|i| {
                let (_, label, group, key) = &self.rows[*i];
                q.is_empty()
                    || label.to_lowercase().contains(&q)
                    || key.to_lowercase().contains(&q)
                    || group.to_lowercase().contains(&q)
            })
            .collect()
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let n = self.visible().len();
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Enter => {
                if let Some(i) = self.visible().get(self.selected) {
                    return Outcome::Run(self.rows[*i].0);
                }
            }
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(n.saturating_sub(1)),
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                self.query.push(c);
                self.selected = 0;
            }
            _ => {}
        }
        Outcome::Pending
    }

    pub fn render(&self, f: &mut Frame, area: Rect, theme: &karet_theme::Theme) {
        let w = 72.min(area.width);
        let h = (area.height * 8 / 10).max(10.min(area.height));
        let rect = Rect {
            x: area.x + (area.width - w) / 2,
            y: area.y + area.height.saturating_sub(h) / 2,
            width: w,
            height: h,
        };
        f.render_widget(Clear, rect);
        let base = crate::ui::surface(theme);
        let muted = theme.style(karet_core::ThemeRole::Muted);
        let accent = theme
            .token_style(karet_core::TokenId::FUNCTION)
            .add_modifier(Modifier::BOLD);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(" All Keys ")
            .style(base);
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", accent),
                Span::raw(self.query.as_str()),
            ])),
            Rect { height: 1, ..inner },
        );
        f.set_cursor_position((inner.x + 2 + self.query.chars().count() as u16, inner.y));

        // Group headers interleaved with rows; scroll so the selection shows.
        let mut lines: Vec<(Line, bool)> = Vec::new();
        let mut group = "";
        for (n, i) in self.visible().into_iter().enumerate() {
            let (_, label, g, key) = &self.rows[i];
            if *g != group {
                group = g;
                lines.push((
                    Line::styled(format!(" {g}"), base.add_modifier(Modifier::BOLD)),
                    false,
                ));
            }
            let style = if n == self.selected {
                base.add_modifier(Modifier::REVERSED)
            } else {
                base
            };
            lines.push((
                Line::from(vec![
                    Span::styled(
                        format!("   {key:>20}  "),
                        if n == self.selected { style } else { muted },
                    ),
                    Span::styled(format!("{label:<44}"), style),
                ]),
                n == self.selected,
            ));
        }
        let body = Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        };
        let sel_line = lines.iter().position(|(_, s)| *s).unwrap_or(0);
        let skip = sel_line.saturating_sub(body.height as usize * 2 / 3);
        let shown: Vec<Line> = lines.into_iter().skip(skip).map(|(l, _)| l).collect();
        f.render_widget(Paragraph::new(shown), body);
    }
}
