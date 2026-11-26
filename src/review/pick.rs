//! The Review view's pickers: which source to review, then (for commits and
//! pull requests) which ones, from a list filtered by typing.

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
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

use super::git::Commit;
use super::git::Entry;
use super::git::PullRequest;
use super::git::Source;
use super::git::SourceInfo;

/// Rows a list shows at most.
const ROWS: usize = 15;

pub enum Picker {
    Sources {
        list: Vec<SourceInfo>,
        selected: usize,
    },
    Commits {
        list: Vec<Commit>,
        query: String,
        /// Position in the filtered list.
        selected: usize,
        /// Hashes picked with Space.
        marked: Vec<String>,
    },
    Prs {
        list: Vec<PullRequest>,
        query: String,
        selected: usize,
    },
}

pub enum Picked {
    Pending,
    Close,
    Status(String),
    /// Review this source; the note says what to know about it.
    Load(Source, Option<String>),
    /// Fetch the list for this entry (commits, pull requests).
    Fetch(Entry),
}

/// Whether every word of `query` is in one of `fields` (ignoring case).
fn matches(query: &str, fields: &[&str]) -> bool {
    let haystack = fields.join(" ").to_lowercase();
    query
        .to_lowercase()
        .split_whitespace()
        .all(|word| haystack.contains(word))
}

impl Picker {
    /// The positions in the full list that the query lets through.
    fn shown(&self) -> Vec<usize> {
        match self {
            Self::Sources { list, .. } => (0..list.len()).collect(),
            Self::Commits { list, query, .. } => list
                .iter()
                .enumerate()
                .filter(|(_, commit)| {
                    matches(query, &[&commit.short, &commit.author, &commit.subject])
                })
                .map(|(idx, _)| idx)
                .collect(),
            Self::Prs { list, query, .. } => list
                .iter()
                .enumerate()
                .filter(|(_, request)| {
                    let number = format!("#{}", request.number);
                    matches(
                        query,
                        &[
                            &number,
                            &request.title,
                            &request.author.login,
                            &request.branch,
                        ],
                    )
                })
                .map(|(idx, _)| idx)
                .collect(),
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Picked {
        let count = self.shown().len();
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return self.choose(),
            KeyCode::Char(' ') if matches!(self, Self::Commits { .. }) => {
                self.toggle_mark();
                return Picked::Pending;
            }
            _ => {}
        }
        let (selected, query) = match self {
            Self::Sources { selected, .. } => (selected, None),
            Self::Commits {
                selected, query, ..
            }
            | Self::Prs {
                selected, query, ..
            } => (selected, Some(query)),
        };
        match key.code {
            KeyCode::Esc => return Picked::Close,
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => *selected = (*selected + 1).min(count.saturating_sub(1)),
            KeyCode::PageUp => *selected = selected.saturating_sub(ROWS),
            KeyCode::PageDown => *selected = (*selected + ROWS).min(count.saturating_sub(1)),
            KeyCode::Backspace => {
                if let Some(query) = query {
                    crate::picker::erase(query, key);
                    *selected = 0;
                }
            }
            // Alt+letter is a shortcut, not typing.
            KeyCode::Char(chr) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                if let Some(query) = query {
                    query.push(chr);
                    *selected = 0;
                }
            }
            _ => {}
        }
        Picked::Pending
    }

    fn toggle_mark(&mut self) {
        let shown = self.shown();
        let Self::Commits {
            list,
            selected,
            marked,
            ..
        } = self
        else {
            return;
        };
        let Some(commit) = shown.get(*selected).and_then(|idx| list.get(*idx)) else {
            return;
        };
        if let Some(pos) = marked.iter().position(|hash| *hash == commit.hash) {
            marked.remove(pos);
        } else {
            marked.push(commit.hash.clone());
        }
        *selected = (*selected + 1).min(shown.len().saturating_sub(1));
    }

    fn choose(&self) -> Picked {
        let shown = self.shown();
        match self {
            Self::Sources { list, selected } => {
                let Some(info) = list.get(*selected) else {
                    return Picked::Pending;
                };
                if let Some(why) = &info.unavailable {
                    return Picked::Status(format!("{}: {why}", info.title));
                }
                match &info.entry {
                    Entry::Load(source) => Picked::Load(source.clone(), None),
                    entry => Picked::Fetch(entry.clone()),
                }
            }
            Self::Commits {
                list,
                selected,
                marked,
                ..
            } => {
                // The list is newest first: the oldest picked commit has the
                // highest position.
                let mut picked: Vec<usize> = list
                    .iter()
                    .enumerate()
                    .filter(|(_, commit)| marked.contains(&commit.hash))
                    .map(|(idx, _)| idx)
                    .collect();
                if picked.is_empty() {
                    picked.extend(shown.get(*selected));
                }
                let (Some(newest), Some(oldest)) = (picked.first(), picked.last()) else {
                    return Picked::Pending;
                };
                let (Some(newest_commit), Some(oldest_commit)) =
                    (list.get(*newest), list.get(*oldest))
                else {
                    return Picked::Pending;
                };
                let between = oldest - newest + 1;
                let skipped = between.saturating_sub(picked.len());
                let note = (skipped > 0).then(|| {
                    format!(
                        "includes {skipped} commit{} you did not pick (a diff cannot leave out the middle)",
                        if skipped == 1 { "" } else { "s" }
                    )
                });
                Picked::Load(
                    Source::Commits {
                        oldest: oldest_commit.hash.clone(),
                        newest: newest_commit.hash.clone(),
                    },
                    note,
                )
            }
            Self::Prs { list, selected, .. } => shown
                .get(*selected)
                .and_then(|idx| list.get(*idx))
                .map_or(Picked::Pending, |request| {
                    Picked::Load(
                        Source::PullRequest {
                            number: request.number,
                        },
                        None,
                    )
                }),
        }
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match self {
            Self::Sources { .. } => {
                vec![("Enter", "Choose"), ("Up/Down", "Select"), ("Esc", "Close")]
            }
            Self::Commits { .. } => vec![
                ("Type", "Filter"),
                ("Space", "Pick (several)"),
                ("Enter", "Review"),
                ("Esc", "Close"),
            ],
            Self::Prs { .. } => vec![("Type", "Filter"), ("Enter", "Review"), ("Esc", "Close")],
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        let shown = self.shown();
        let (title, query, selected) = match self {
            Self::Sources { selected, .. } => (" Review what? ".to_string(), None, *selected),
            Self::Commits {
                query,
                selected,
                marked,
                ..
            } => (
                if marked.is_empty() {
                    " Commits (Space picks several) ".to_string()
                } else {
                    format!(" Commits ({} picked) ", marked.len())
                },
                Some(query),
                *selected,
            ),
            Self::Prs {
                query, selected, ..
            } => (" Open pull requests ".to_string(), Some(query), *selected),
        };
        let mut lines: Vec<Line> = Vec::new();
        if let Some(query) = query {
            lines.push(Line::from(vec![
                Span::styled("> ", muted),
                Span::raw(query.clone()),
            ]));
        }
        let first = selected.saturating_sub(ROWS.saturating_sub(1));
        for (row, idx) in shown.iter().enumerate().skip(first).take(ROWS) {
            let mut line = self.row(*idx, muted);
            if row == selected {
                line = line.style(Style::default().add_modifier(Modifier::REVERSED));
            }
            lines.push(line);
        }
        if shown.is_empty() {
            lines.push(Line::styled("  Nothing matches", muted));
        }
        let wanted = lines
            .iter()
            .map(Line::width)
            .max()
            .unwrap_or(40)
            .saturating_add(2);
        let width = u16::try_from(wanted.max(50))
            .unwrap_or(u16::MAX)
            .min(area.width);
        let height = u16::try_from(lines.len().saturating_add(2))
            .unwrap_or(u16::MAX)
            .min(area.height);
        let rect = Rect {
            x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
            y: area.y.saturating_add(area.height / 6),
            width,
            height,
        };
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(title)
                    .style(base),
            ),
            rect,
        );
        if query.is_some() {
            let typed = query.map_or(0, |query| query.chars().count());
            let cursor_x = rect
                .x
                .saturating_add(3)
                .saturating_add(u16::try_from(typed).unwrap_or(u16::MAX));
            frame.set_cursor_position((cursor_x, rect.y.saturating_add(1)));
        }
    }

    fn row(&self, idx: usize, muted: Style) -> Line<'static> {
        match self {
            Self::Sources { list, .. } => list.get(idx).map_or_else(Line::default, |info| {
                let count = match (&info.unavailable, info.files) {
                    (Some(why), _) => format!("({why})"),
                    (None, Some(1)) => "1 file".to_string(),
                    (None, Some(files)) => format!("{files} files"),
                    (None, None) => String::new(),
                };
                Line::from(vec![
                    Span::raw(format!(" {:<16}", info.title)),
                    Span::styled(format!("{:<34}", info.detail), muted),
                    Span::raw(format!("{count} ")),
                ])
            }),
            Self::Commits { list, marked, .. } => {
                list.get(idx).map_or_else(Line::default, |commit| {
                    let mark = if marked.contains(&commit.hash) {
                        "[x]"
                    } else {
                        "[ ]"
                    };
                    Line::from(vec![
                        Span::raw(format!(" {mark} ")),
                        Span::raw(format!("{}  ", commit.short)),
                        Span::styled(format!("{:<15}", shorten(&commit.when, 15)), muted),
                        Span::styled(format!("{:<14}", shorten(&commit.author, 14)), muted),
                        Span::raw(format!("{} ", shorten(&commit.subject, 60))),
                    ])
                })
            }
            Self::Prs { list, .. } => list.get(idx).map_or_else(Line::default, |request| {
                let draft = if request.draft { " (draft)" } else { "" };
                Line::from(vec![
                    Span::raw(format!(" #{:<6}", request.number)),
                    Span::raw(format!("{}{draft}  ", shorten(&request.title, 60))),
                    Span::styled(
                        format!(
                            "{}  {} ",
                            request.author.login,
                            shorten(&request.branch, 30)
                        ),
                        muted,
                    ),
                ])
            }),
        }
    }
}

fn shorten(text: &str, max: usize) -> String {
    crate::picker::keep_start(text, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(hash: &str, subject: &str) -> Commit {
        Commit {
            hash: hash.repeat(8),
            short: hash.repeat(3),
            when: "today".into(),
            author: "robin".into(),
            subject: subject.into(),
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Picking the newest and the oldest of three reviews all three, and says
    /// the middle one comes along.
    #[test]
    fn picked_commits_become_one_range() -> Result<(), String> {
        let mut picker = Picker::Commits {
            list: vec![
                commit("c", "third"),
                commit("b", "second"),
                commit("a", "first"),
            ],
            query: String::new(),
            selected: 0,
            marked: Vec::new(),
        };
        picker.handle_key(&key(KeyCode::Char(' ')));
        picker.handle_key(&key(KeyCode::Down));
        picker.handle_key(&key(KeyCode::Char(' ')));
        let Picked::Load(Source::Commits { oldest, newest }, note) =
            picker.handle_key(&key(KeyCode::Enter))
        else {
            return Err("expected a commit range".into());
        };
        assert_eq!((oldest.as_str(), newest.as_str()), ("aaaaaaaa", "cccccccc"));
        assert!(note.is_some_and(|note| note.contains("1 commit you did not pick")));
        Ok(())
    }

    #[test]
    fn typing_filters_commits() {
        let mut picker = Picker::Commits {
            list: vec![commit("c", "fix the cart"), commit("b", "add login")],
            query: String::new(),
            selected: 0,
            marked: Vec::new(),
        };
        for chr in "login".chars() {
            picker.handle_key(&key(KeyCode::Char(chr)));
        }
        assert_eq!(picker.shown(), vec![1]);
    }
}
