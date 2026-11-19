//! Check Keys: a guided test of which default shortcuts reach sanitty.
//!
//! Terminals (T3's in particular) swallow some chords, and terminals without
//! the kitty keyboard protocol fold others (`Ctrl+Shift+N` arrives as
//! `Ctrl+N`). This screen asks for each default chord in turn and records what
//! actually arrived, then writes a Markdown report and copies it.

use std::path::PathBuf;

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
use crate::keymap::Chord;
use crate::keymap::DEFAULTS;

enum Result {
    Ok,
    ArrivedAs(Chord),
    Missing,
}

pub struct KeyCheck {
    steps: Vec<(Chord, Vec<Action>)>,
    results: Vec<Result>,
    /// The report path once finished.
    pub saved: Option<PathBuf>,
}

pub enum Outcome {
    Pending,
    /// The run is over; the report text is ready to copy.
    Finished(String),
    Close,
}

impl KeyCheck {
    pub fn new() -> Self {
        let mut steps: Vec<(Chord, Vec<Action>)> = Vec::new();
        for (spec, action) in DEFAULTS {
            let Ok(chord) = Chord::parse(spec) else {
                continue;
            };
            match steps.iter_mut().find(|(c, _)| *c == chord) {
                Some((_, actions)) => actions.push(*action),
                None => steps.push((chord, vec![*action])),
            }
        }
        Self {
            steps,
            results: Vec::new(),
            saved: None,
        }
    }

    fn done(&self) -> bool {
        self.results.len() >= self.steps.len()
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        if self.done() {
            return if key.code == KeyCode::Esc || key.code == KeyCode::Enter {
                Outcome::Close
            } else {
                Outcome::Pending
            };
        }
        // Esc means "nothing arrived"; Backspace redoes the previous step. Every
        // other key is the answer to the current step.
        let result = match key.code {
            KeyCode::Esc => Result::Missing,
            KeyCode::Backspace if key.modifiers == KeyModifiers::NONE => {
                self.results.pop();
                return Outcome::Pending;
            }
            _ => {
                let got = Chord::from_event(key);
                if got == self.steps[self.results.len()].0 {
                    Result::Ok
                } else {
                    Result::ArrivedAs(got)
                }
            }
        };
        self.results.push(result);
        if self.done() {
            Outcome::Finished(self.report())
        } else {
            Outcome::Pending
        }
    }

    /// Which terminal this is, as far as the environment tells.
    /// `TERM_PROGRAM` wins (tmux, ghostty), then T3's own variables, then `TERM`.
    pub fn terminal_name() -> String {
        if let Ok(p) = std::env::var("TERM_PROGRAM")
            && !p.is_empty()
        {
            return p;
        }
        if std::env::vars().any(|(k, _)| k.starts_with("T3CODE_")) {
            return "t3".into();
        }
        std::env::var("TERM").unwrap_or_else(|_| "unknown".into())
    }

    pub fn report(&self) -> String {
        let mut out = format!(
            "# sanitty key check: {}\n\nTERM={}, TERM_PROGRAM={}\n\n| Key | Action | Result |\n|---|---|---|\n",
            Self::terminal_name(),
            std::env::var("TERM").unwrap_or_default(),
            std::env::var("TERM_PROGRAM").unwrap_or_default(),
        );
        for ((chord, actions), result) in self.steps.iter().zip(&self.results) {
            let names: Vec<&str> = actions.iter().map(|a| a.label()).collect();
            let result = match result {
                Result::Ok => "ok".to_string(),
                Result::ArrivedAs(c) => format!("arrives as {c}"),
                Result::Missing => "does not arrive".to_string(),
            };
            out.push_str(&format!("| {chord} | {} | {result} |\n", names.join(", ")));
        }
        out
    }

    pub fn render(&self, f: &mut Frame, area: Rect, theme: &karet_theme::Theme) {
        f.render_widget(Clear, area);
        let base = crate::ui::surface(theme);
        let muted = theme.style(karet_core::ThemeRole::Muted);
        let good = theme.style(karet_core::ThemeRole::DiagnosticHint);
        let warn = theme.style(karet_core::ThemeRole::DiagnosticWarning);
        let bad = theme.style(karet_core::ThemeRole::DiagnosticError);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(" Check Keys ")
            .style(base);
        let inner = block.inner(area);
        f.render_widget(block, area);

        let mut lines = Vec::new();
        if let Some((chord, actions)) = self.steps.get(self.results.len()) {
            lines.push(Line::from(vec![
                Span::raw(format!(
                    "  Step {} of {}: press  ",
                    self.results.len() + 1,
                    self.steps.len()
                )),
                Span::styled(
                    chord.to_string(),
                    base.add_modifier(Modifier::BOLD | Modifier::REVERSED),
                ),
                Span::styled(
                    format!(
                        "   ({})",
                        actions
                            .iter()
                            .map(|a| a.label())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    muted,
                ),
            ]));
            lines.push(Line::styled("  Nothing happens? Press Esc to mark it as not arriving. Backspace redoes the previous step.", muted));
        } else {
            match &self.saved {
                Some(p) => lines.push(Line::from(format!(
                    "  Done. Report saved to {}",
                    p.display()
                ))),
                None => lines.push(Line::from(
                    "  Done. The report could not be saved to ~/.config/sanitty/",
                )),
            }
            lines.push(Line::styled(
                "  It was also sent to the clipboard, if this terminal allows that (OSC 52).",
                muted,
            ));
            lines.push(Line::styled("  Esc or Enter closes this screen.", muted));
        }
        lines.push(Line::from(""));

        // Results so far, in columns.
        let col_w = 34usize;
        let cols = (inner.width as usize / col_w).max(1);
        let rows_avail = (inner.height as usize).saturating_sub(lines.len()).max(1);
        let entries: Vec<Line> = self
            .steps
            .iter()
            .zip(self.results.iter().map(Some).chain(std::iter::repeat(None)))
            .map(|((chord, _), result)| {
                let (mark, style, note) = match result {
                    Some(Result::Ok) => ("ok", good, String::new()),
                    Some(Result::ArrivedAs(c)) => ("as", warn, format!(" -> {c}")),
                    Some(Result::Missing) => ("--", bad, String::new()),
                    None => ("  ", muted, String::new()),
                };
                let text = format!("{mark} {chord}{note}");
                Line::styled(
                    format!(
                        "  {:<w$}",
                        text.chars().take(col_w - 3).collect::<String>(),
                        w = col_w - 2
                    ),
                    style,
                )
            })
            .collect();
        let per_col = entries.len().div_ceil(cols).min(rows_avail);
        for r in 0..per_col {
            let mut spans = Vec::new();
            for c in 0..cols {
                if let Some(l) = entries.get(c * per_col + r) {
                    spans.extend(l.spans.iter().cloned().map(|s| s.patch_style(l.style)));
                }
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines), inner);
    }
}
