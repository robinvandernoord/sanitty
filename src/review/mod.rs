//! The Review view (Alt+9): the changed files, their diff, comments on lines,
//! and the whole review copied as Markdown. Read-only: nothing is edited here.
//!
//! Loading a diff runs `git`, which the app does off its loop (see
//! [`git::load`]); this module keeps the view's state, handles its keys and
//! draws it.

mod colour;
pub mod git;
mod rows;
pub mod state;

use std::fmt::Write as _;
use std::path::PathBuf;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use karet_core::ThemeRole;
use karet_diff::Cell;
use karet_diff::DiffLine;
use karet_diff::FileDiff;
use karet_diff::LineKind;
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

use git::Loaded;
use git::Source;
use git::SourceInfo;
use rows::Row;
use state::Comment;
use state::Kind;
use state::Saved;
use state::Side;

/// Below this width the diff is one column unless `\` says otherwise.
const SPLIT_MIN_WIDTH: u16 = 120;

/// What the app should do after a key.
pub enum Outcome {
    /// Nothing more.
    Pending,
    /// Show a message in the status line.
    Status(String),
    /// Put this Markdown on the clipboard, then show the message.
    Copy(String, String),
    /// Load this source (off the app loop) and hand it to [`Review::loaded`].
    Load(Source),
    /// Find the sources (off the app loop) and hand them to [`Review::pick`].
    Sources,
    /// Leave the view (the picker was closed before anything was loaded).
    Leave,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Files,
    Diff,
}

/// The comment box.
struct Editing {
    kind: Kind,
    text: String,
    /// A new comment (side, first and last line, first line's text), or the
    /// index of the comment being edited.
    target: Target,
}

enum Target {
    New {
        side: Side,
        start: u32,
        end: u32,
        anchor: String,
    },
    Existing(usize),
}

pub struct Review {
    root: PathBuf,
    saved: Saved,
    /// The diff on screen, once loaded.
    loaded: Option<Loaded>,
    /// Waiting for a diff or the source list.
    loading: bool,
    /// Why the last load failed.
    error: Option<String>,
    /// The source list, while it is open.
    picker: Option<(Vec<SourceInfo>, usize)>,
    file: usize,
    file_scroll: usize,
    focus: Focus,
    rows: Vec<Row>,
    /// The split mode `rows` were built for.
    rows_split: Option<bool>,
    cursor: usize,
    /// The other end of a range selection (Shift+Up/Down).
    mark: Option<usize>,
    scroll: usize,
    /// `\` overrides the automatic choice; `None` follows the width.
    split: Option<bool>,
    /// Rows visible in the diff at the last draw, for paging.
    page: usize,
    editing: Option<Editing>,
    /// `x` was pressed once; the next `x` clears everything.
    confirm_clear: bool,
}

impl Review {
    pub fn new(root: PathBuf) -> Self {
        let saved = state::load(&root);
        Self {
            root,
            saved,
            loaded: None,
            loading: false,
            error: None,
            picker: None,
            file: 0,
            file_scroll: 0,
            focus: Focus::Diff,
            rows: Vec::new(),
            rows_split: None,
            cursor: 0,
            mark: None,
            scroll: 0,
            split: None,
            page: 20,
            editing: None,
            confirm_clear: false,
        }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// What to do when the view opens: load the last source again (it may
    /// have changed), or ask which one the first time.
    pub fn open(&mut self) -> Outcome {
        self.loading = true;
        match self.saved.last_source {
            Some(source) => Outcome::Load(source),
            None => Outcome::Sources,
        }
    }

    /// The sources arrived: show the picker.
    pub fn pick(&mut self, sources: Vec<SourceInfo>) {
        self.loading = false;
        let current = self.loaded.as_ref().map(|loaded| loaded.source);
        let selected = sources
            .iter()
            .position(|info| Some(info.source) == current)
            .unwrap_or(0);
        self.picker = Some((sources, selected));
    }

    /// A diff arrived (or failed).
    pub fn loaded(&mut self, result: Result<Loaded, String>) {
        self.loading = false;
        match result {
            Ok(mut loaded) => {
                let same = self
                    .loaded
                    .as_ref()
                    .is_some_and(|old| old.source == loaded.source);
                let path = self.current_file().map(|file| file.path.clone());
                // Comments move with their lines.
                let comments = &mut self.saved.source_mut(loaded.source).comments;
                for comment in comments.iter_mut() {
                    if let Some(file) = loaded.files.iter().find(|file| file.path == comment.path)
                        && let Some(moved) = state::reanchor(comment, file)
                    {
                        *comment = moved;
                    }
                }
                loaded
                    .files
                    .sort_by(|left, right| left.path.cmp(&right.path));
                self.saved.last_source = Some(loaded.source);
                self.loaded = Some(loaded);
                self.error = None;
                self.file = path
                    .filter(|_| same)
                    .and_then(|path| self.files().iter().position(|file| file.path == path))
                    .unwrap_or(0);
                self.reset_rows();
                self.persist();
            }
            Err(err) => self.error = Some(err),
        }
    }

    fn files(&self) -> &[FileDiff] {
        self.loaded
            .as_ref()
            .map_or(&[], |loaded| loaded.files.as_slice())
    }

    fn current_file(&self) -> Option<&FileDiff> {
        self.files().get(self.file)
    }

    fn source(&self) -> Option<Source> {
        self.loaded.as_ref().map(|loaded| loaded.source)
    }

    fn comments(&self) -> &[Comment] {
        self.source()
            .map_or(&[], |source| self.saved.source(source).comments.as_slice())
    }

    fn persist(&mut self) -> Option<String> {
        state::save(&self.root, &self.saved)
            .err()
            .map(|err| format!("Could not save the review: {err}"))
    }

    fn reset_rows(&mut self) {
        self.rows_split = None;
        self.cursor = 0;
        self.scroll = 0;
        self.mark = None;
    }

    /// Lay out the current file again (after a comment changed, or the
    /// split mode).
    fn rebuild(&mut self, split: bool) {
        let rows = match self.current_file() {
            Some(file) => {
                let mine: Vec<(usize, &Comment)> = self
                    .comments()
                    .iter()
                    .enumerate()
                    .filter(|(_, comment)| comment.path == file.path)
                    .collect();
                rows::build(file, &mine, split)
            }
            None => Vec::new(),
        };
        self.rows = rows;
        self.rows_split = Some(split);
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    fn refresh(&mut self) {
        if let Some(split) = self.rows_split {
            self.rebuild(split);
        }
    }

    // ------------------------------------------------------------------ keys

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        if self.picker.is_some() {
            return self.picker_key(key);
        }
        if self.editing.is_some() {
            return self.editing_key(key);
        }
        let clear = std::mem::take(&mut self.confirm_clear);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Char('s') => {
                self.loading = true;
                return Outcome::Sources;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Files => Focus::Diff,
                    Focus::Diff => Focus::Files,
                };
            }
            KeyCode::Char(']') => self.select_file(self.file.saturating_add(1)),
            KeyCode::Char('[') => self.select_file(self.file.saturating_sub(1)),
            KeyCode::Char('\\') => {
                let now = self.rows_split.unwrap_or(true);
                self.split = Some(!now);
            }
            KeyCode::Char('v') => return self.toggle_viewed(),
            KeyCode::Char('y') => return self.copy(),
            KeyCode::Char('x') => return self.clear(clear),
            KeyCode::Char('c') => return self.start_comment(),
            KeyCode::Char('d') => return self.delete_comment(),
            KeyCode::Enter if self.focus == Focus::Files => self.focus = Focus::Diff,
            KeyCode::Enter => return self.edit_comment(),
            KeyCode::Esc => self.mark = None,
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End => {
                self.move_by(key.code, shift);
            }
            _ => {}
        }
        Outcome::Pending
    }

    fn picker_key(&mut self, key: &KeyEvent) -> Outcome {
        let Some((sources, selected)) = &mut self.picker else {
            return Outcome::Pending;
        };
        match key.code {
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => *selected = (*selected + 1).min(sources.len().saturating_sub(1)),
            KeyCode::Esc => {
                self.picker = None;
                if self.loaded.is_none() {
                    return Outcome::Leave;
                }
            }
            KeyCode::Enter => {
                let Some(info) = sources.get(*selected) else {
                    return Outcome::Pending;
                };
                if let Some(why) = &info.unavailable {
                    return Outcome::Status(format!("{}: {why}", info.title));
                }
                let source = info.source;
                self.picker = None;
                self.loading = true;
                return Outcome::Load(source);
            }
            _ => {}
        }
        Outcome::Pending
    }

    fn editing_key(&mut self, key: &KeyEvent) -> Outcome {
        let Some(editing) = &mut self.editing else {
            return Outcome::Pending;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.editing = None,
            KeyCode::Tab => editing.kind = editing.kind.next(),
            KeyCode::Backspace => crate::picker::erase(&mut editing.text, key),
            KeyCode::Char('u') if ctrl => editing.text.clear(),
            KeyCode::Char(chr) if !ctrl => editing.text.push(chr),
            KeyCode::Enter => return self.save_comment(),
            _ => {}
        }
        Outcome::Pending
    }

    /// Typed text (a paste) goes into the comment box when it is open.
    pub fn paste(&mut self, text: &str) {
        if let Some(editing) = &mut self.editing {
            editing.text.push_str(&text.replace(['\r', '\n'], " "));
        }
    }

    fn select_file(&mut self, idx: usize) {
        let last = self.files().len().saturating_sub(1);
        let idx = idx.min(last);
        if idx != self.file {
            self.file = idx;
            self.reset_rows();
        }
    }

    fn move_by(&mut self, code: KeyCode, extend: bool) {
        if self.focus == Focus::Files {
            let target = match code {
                KeyCode::Up => self.file.saturating_sub(1),
                KeyCode::Down => self.file.saturating_add(1),
                KeyCode::PageUp | KeyCode::Home => 0,
                _ => usize::MAX,
            };
            self.select_file(target);
            return;
        }
        if extend && self.mark.is_none() {
            self.mark = Some(self.cursor);
        } else if !extend {
            self.mark = None;
        }
        let last = self.rows.len().saturating_sub(1);
        self.cursor = match code {
            KeyCode::Up => self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor.saturating_add(1),
            KeyCode::PageUp => self.cursor.saturating_sub(self.page),
            KeyCode::PageDown => self.cursor.saturating_add(self.page),
            KeyCode::Home => 0,
            _ => last,
        }
        .min(last);
    }

    fn toggle_viewed(&mut self) -> Outcome {
        let (Some(source), Some(file)) = (self.source(), self.current_file()) else {
            return Outcome::Pending;
        };
        let (path, print) = (file.path.clone(), state::fingerprint(file));
        let viewed = &mut self.saved.source_mut(source).viewed;
        if viewed.get(&path) == Some(&print) {
            viewed.remove(&path);
        } else {
            viewed.insert(path, print);
            // Viewed: on to the next file that is not.
            let next = self
                .files()
                .iter()
                .enumerate()
                .skip(self.file.saturating_add(1))
                .find(|(_, file)| !self.is_viewed(file))
                .map(|(idx, _)| idx);
            if let Some(next) = next {
                self.select_file(next);
            }
        }
        self.persist().map_or(Outcome::Pending, Outcome::Status)
    }

    fn is_viewed(&self, file: &FileDiff) -> bool {
        self.source().is_some_and(|source| {
            self.saved.source(source).viewed.get(&file.path) == Some(&state::fingerprint(file))
        })
    }

    fn copy(&self) -> Outcome {
        let Some(loaded) = &self.loaded else {
            return Outcome::Pending;
        };
        let comments = self.comments();
        if comments.is_empty() {
            return Outcome::Status("No comments to copy yet (c adds one)".into());
        }
        let text = state::export(&loaded.detail, &loaded.files, comments);
        let mut files: Vec<&str> = comments
            .iter()
            .map(|comment| comment.path.as_str())
            .collect();
        files.dedup();
        files.sort_unstable();
        files.dedup();
        let status = format!(
            "Copied review: {} in {} (Markdown). x clears them.",
            plural(comments.len(), "comment"),
            plural(files.len(), "file")
        );
        Outcome::Copy(text, status)
    }

    fn clear(&mut self, confirmed: bool) -> Outcome {
        let Some(source) = self.source() else {
            return Outcome::Pending;
        };
        let count = self.comments().len();
        if count == 0 {
            return Outcome::Status("No comments to clear".into());
        }
        if !confirmed {
            self.confirm_clear = true;
            return Outcome::Status(format!(
                "x again deletes all {} of this review",
                plural(count, "comment")
            ));
        }
        let state = self.saved.source_mut(source);
        state.comments.clear();
        state.viewed.clear();
        self.refresh();
        self.persist()
            .map_or_else(|| Outcome::Status("Review cleared".into()), Outcome::Status)
    }

    fn comment_at(&self, row: usize) -> Option<usize> {
        match self.rows.get(row) {
            Some(Row::Comment(idx)) => Some(*idx),
            _ => None,
        }
    }

    fn start_comment(&mut self) -> Outcome {
        if self.focus == Focus::Files {
            self.focus = Focus::Diff;
        }
        if self.comment_at(self.cursor).is_some() {
            return self.edit_comment();
        }
        let Some((side, _, _)) = self.rows.get(self.cursor).and_then(Row::anchor) else {
            return Outcome::Status("Put the cursor on a line to comment on it".into());
        };
        let (from, until) = match self.mark {
            Some(mark) => (mark.min(self.cursor), mark.max(self.cursor)),
            None => (self.cursor, self.cursor),
        };
        let lines: Vec<(u32, &str)> = self
            .rows
            .get(from..=until)
            .unwrap_or_default()
            .iter()
            .filter_map(Row::anchor)
            .filter(|(line_side, _, _)| *line_side == side)
            .map(|(_, lineno, text)| (lineno, text))
            .collect();
        let (Some(first), Some(last)) = (
            lines.iter().min_by_key(|line| line.0),
            lines.iter().max_by_key(|line| line.0),
        ) else {
            return Outcome::Pending;
        };
        self.editing = Some(Editing {
            kind: Kind::Issue,
            text: String::new(),
            target: Target::New {
                side,
                start: first.0,
                end: last.0,
                anchor: first.1.to_string(),
            },
        });
        self.mark = None;
        Outcome::Pending
    }

    fn edit_comment(&mut self) -> Outcome {
        let Some(idx) = self.comment_at(self.cursor) else {
            return Outcome::Pending;
        };
        let Some(comment) = self.comments().get(idx) else {
            return Outcome::Pending;
        };
        self.editing = Some(Editing {
            kind: comment.kind,
            text: comment.body.clone(),
            target: Target::Existing(idx),
        });
        Outcome::Pending
    }

    fn delete_comment(&mut self) -> Outcome {
        let (Some(idx), Some(source)) = (self.comment_at(self.cursor), self.source()) else {
            return Outcome::Pending;
        };
        let comments = &mut self.saved.source_mut(source).comments;
        if idx < comments.len() {
            comments.remove(idx);
        }
        self.refresh();
        self.persist().map_or_else(
            || Outcome::Status("Comment deleted".into()),
            Outcome::Status,
        )
    }

    fn save_comment(&mut self) -> Outcome {
        let (Some(editing), Some(source), Some(path)) = (
            self.editing.take(),
            self.source(),
            self.current_file().map(|file| file.path.clone()),
        ) else {
            return Outcome::Pending;
        };
        let body = editing.text.trim().to_string();
        if body.is_empty() {
            return Outcome::Status("Empty comment not saved".into());
        }
        let comments = &mut self.saved.source_mut(source).comments;
        match editing.target {
            Target::Existing(idx) => {
                if let Some(comment) = comments.get_mut(idx) {
                    comment.kind = editing.kind;
                    comment.body = body;
                }
            }
            Target::New {
                side,
                start,
                end,
                anchor,
            } => comments.push(Comment {
                path,
                side,
                start,
                end,
                anchor,
                kind: editing.kind,
                body,
            }),
        }
        self.refresh();
        self.persist().map_or(Outcome::Pending, Outcome::Status)
    }

    /// The keys to show in the hint bar.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if self.picker.is_some() {
            return vec![("Enter", "Review"), ("Up/Down", "Select"), ("Esc", "Close")];
        }
        if self.editing.is_some() {
            return vec![("Enter", "Save"), ("Tab", "Type"), ("Esc", "Cancel")];
        }
        let mut hints = vec![("c", "Comment")];
        if self.comment_at(self.cursor).is_some() {
            hints.push(("Enter", "Edit"));
            hints.push(("d", "Delete"));
        }
        hints.extend([
            ("v", "Viewed"),
            ("\\", "Split/inline"),
            ("] [", "File"),
            ("Tab", "Files/diff"),
            ("y", "Copy review"),
            ("s", "Source"),
        ]);
        hints
    }

    // --------------------------------------------------------------- drawing

    pub fn render(&mut self, frame: &mut Frame, top: Rect, body: Rect, theme: &Theme) {
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        self.render_title(frame, top, theme);
        if self.loaded.is_none() {
            let text = match (&self.error, self.loading) {
                (Some(err), _) => format!("  {err}  (s picks a source)"),
                (None, true) => "  Loading...".into(),
                (None, false) => String::new(),
            };
            frame.render_widget(Paragraph::new(Line::styled(text, muted)).style(base), body);
            self.render_picker(frame, body, theme);
            return;
        }
        let names_width = self
            .files()
            .iter()
            .map(|file| file.path.chars().count())
            .max()
            .unwrap_or(10);
        let files_width = to_u16(names_width.saturating_add(14))
            .clamp(20, 44)
            .min(body.width / 3);
        let files_area = Rect {
            width: files_width,
            ..body
        };
        let diff_area = Rect {
            x: body.x.saturating_add(files_width),
            width: body.width.saturating_sub(files_width),
            ..body
        };
        self.render_files(frame, files_area, theme);
        self.render_diff(frame, diff_area, theme);
        self.render_picker(frame, body, theme);
    }

    fn render_title(&self, frame: &mut Frame, top: Rect, theme: &Theme) {
        let bar = crate::styles::bar(theme);
        let mut text = String::from(" Review");
        if let Some(loaded) = &self.loaded {
            let (added, removed) = loaded.files.iter().fold((0, 0), |(add, del), file| {
                let (file_add, file_del) = counts(file);
                (add + file_add, del + file_del)
            });
            let _ = write!(
                text,
                "   {}   {}  +{added} -{removed}   {}",
                loaded.detail,
                plural(loaded.files.len(), "file"),
                plural(self.comments().len(), "comment")
            );
        }
        if self.loading {
            text.push_str("   (loading...)");
        }
        frame.render_widget(Paragraph::new(text).style(bar), top);
    }

    fn render_files(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        // The diff colours are background tints; a mark needs the bright version.
        let added = theme
            .style(ThemeRole::DiffAdded)
            .fg
            .map_or(muted, |colour| Style::default().fg(brighter(colour, 4)));
        let border = if self.focus == Focus::Files {
            base
        } else {
            muted
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(" files ")
            .style(base);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let height = usize::from(inner.height);
        if self.file < self.file_scroll {
            self.file_scroll = self.file;
        } else if height > 0 && self.file >= self.file_scroll + height {
            self.file_scroll = self.file + 1 - height;
        }
        let width = usize::from(inner.width);
        let lines: Vec<Line> = self
            .files()
            .iter()
            .enumerate()
            .skip(self.file_scroll)
            .take(height)
            .map(|(idx, file)| {
                let (add, del) = counts(file);
                let notes = self
                    .comments()
                    .iter()
                    .filter(|comment| comment.path == file.path)
                    .count();
                let mark = if self.is_viewed(file) {
                    "\u{2713}"
                } else {
                    " "
                };
                let stats = if notes > 0 {
                    format!(" +{add} -{del} {notes}")
                } else {
                    format!(" +{add} -{del}")
                };
                let room = width.saturating_sub(stats.chars().count() + 1);
                let name = crate::picker::keep_end(&file.path, room);
                let pad = room.saturating_sub(name.chars().count());
                let line = Line::from(vec![
                    Span::styled(mark, added),
                    Span::raw(name),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(stats, muted),
                ]);
                if idx == self.file {
                    as_selected(line, theme)
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }

    fn render_diff(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let split = self.split.unwrap_or(area.width >= SPLIT_MIN_WIDTH);
        if self.rows_split != Some(split) {
            self.rebuild(split);
        }
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        let border = if self.focus == Focus::Diff {
            base
        } else {
            muted
        };
        let title = self
            .current_file()
            .map_or_else(String::new, |file| match &file.old_path {
                Some(old) => format!(" {old} -> {} ", file.path),
                None => format!(" {} ", file.path),
            });
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(title)
            .title(Line::styled(if split { " split " } else { " inline " }, muted).right_aligned())
            .style(base);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let height = usize::from(inner.height);
        self.page = height.saturating_sub(2).max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if height > 0 && self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
        if self.files().is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled("  Nothing changed", muted)),
                inner,
            );
            return;
        }
        let selected = match self.mark {
            Some(mark) => (mark.min(self.cursor), mark.max(self.cursor)),
            None => (self.cursor, self.cursor),
        };
        let width = usize::from(inner.width);
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(height)
            .map(|(idx, row)| {
                let line = self.row_line(row, width, theme);
                let chosen = self.focus == Focus::Diff && idx >= selected.0 && idx <= selected.1;
                if chosen {
                    as_selected(line, theme)
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
        self.render_editing(frame, inner, theme);
    }

    fn row_line(&self, row: &Row, width: usize, theme: &Theme) -> Line<'static> {
        let muted = theme.style(ThemeRole::Muted);
        let colours = self
            .loaded
            .as_ref()
            .zip(self.current_file())
            .and_then(|(loaded, file)| loaded.colours.get(&file.path));
        match row {
            Row::Hunk(header) => Line::styled(fit(header, width), muted),
            Row::Note(text) => Line::styled(fit(&format!("  {text}"), width), muted),
            Row::Comment(idx) => {
                let Some(comment) = self.comments().get(*idx) else {
                    return Line::raw("");
                };
                let role = if comment.kind == Kind::Issue {
                    ThemeRole::DiagnosticWarning
                } else {
                    ThemeRole::DiagnosticInfo
                };
                let text = format!(
                    "  \u{25b8} {}  {}  ({})",
                    comment.kind.word(),
                    comment.body,
                    comment.lines_label()
                );
                Line::styled(fit(&text, width), theme.style(role))
            }
            Row::Line(line) => {
                Line::from(diff_spans(&line_cell(line), width, (None, colours), theme))
            }
            Row::Pair { left, right } => {
                let half = width.saturating_sub(1) / 2;
                let pair = match (left, right) {
                    (Some(old), Some(new))
                        if old.kind == LineKind::Remove && new.kind == LineKind::Add =>
                    {
                        Some(karet_diff::compute_highlights(&old.content, &new.content))
                    }
                    _ => None,
                };
                let mut spans = match left {
                    Some(cell) => diff_spans(
                        cell,
                        half,
                        (
                            pair.as_ref().map(|pair| pair.old_segments.as_slice()),
                            colours,
                        ),
                        theme,
                    ),
                    None => vec![Span::raw(" ".repeat(half))],
                };
                spans.push(Span::styled("\u{2502}", muted));
                match right {
                    Some(cell) => spans.extend(diff_spans(
                        cell,
                        width.saturating_sub(half + 1),
                        (
                            pair.as_ref().map(|pair| pair.new_segments.as_slice()),
                            colours,
                        ),
                        theme,
                    )),
                    None => spans.push(Span::raw(" ".repeat(width.saturating_sub(half + 1)))),
                }
                Line::from(spans)
            }
        }
    }

    fn render_editing(&self, frame: &mut Frame, inner: Rect, theme: &Theme) {
        let Some(editing) = &self.editing else { return };
        let label = match &editing.target {
            Target::New { start, end, .. } if start == end => format!("line {start}"),
            Target::New { start, end, .. } => format!("lines {start}-{end}"),
            Target::Existing(idx) => self
                .comments()
                .get(*idx)
                .map_or_else(String::new, Comment::lines_label),
        };
        let row = to_u16(self.cursor.saturating_sub(self.scroll).saturating_add(1));
        let height = 3;
        let mut y = inner.y.saturating_add(row);
        if y.saturating_add(height) > inner.y.saturating_add(inner.height) {
            y = inner.y.saturating_add(row.saturating_sub(height + 1));
        }
        let rect = Rect {
            x: inner.x.saturating_add(2),
            y,
            width: inner.width.saturating_sub(4).min(90),
            height,
        };
        let base = crate::styles::surface(theme);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(format!(" {} (Tab: type) - {label} ", editing.kind.word()))
            .title_bottom(" Enter save   Esc cancel ")
            .style(base);
        let text_area = block.inner(rect);
        frame.render_widget(Clear, rect);
        frame.render_widget(block, rect);
        let room = usize::from(text_area.width).saturating_sub(1);
        let shown: String = {
            let count = editing.text.chars().count();
            editing
                .text
                .chars()
                .skip(count.saturating_sub(room))
                .collect()
        };
        let cursor_x = text_area.x.saturating_add(to_u16(shown.chars().count()));
        frame.render_widget(Paragraph::new(shown), text_area);
        frame.set_cursor_position((cursor_x, text_area.y));
    }

    fn render_picker(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let Some((sources, selected)) = &self.picker else {
            return;
        };
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        let lines: Vec<Line> = sources
            .iter()
            .enumerate()
            .map(|(idx, info)| {
                let count = match (&info.unavailable, info.files) {
                    (Some(why), _) => format!("({why})"),
                    (None, Some(files)) => plural(files, "file"),
                    (None, None) => String::new(),
                };
                let line = Line::from(vec![
                    Span::raw(format!(" {:<14}", info.title)),
                    Span::styled(format!("{:<30}", info.detail), muted),
                    Span::raw(format!("{count} ")),
                ]);
                if idx == *selected {
                    line.style(Style::default().add_modifier(Modifier::REVERSED))
                } else {
                    line
                }
            })
            .collect();
        let width = lines
            .iter()
            .map(Line::width)
            .max()
            .unwrap_or(40)
            .saturating_add(2);
        let width = to_u16(width).min(area.width);
        let height = to_u16(lines.len().saturating_add(2)).min(area.height);
        let rect = Rect {
            x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
            y: area.y.saturating_add(area.height / 4),
            width,
            height,
        };
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(" Review what? ")
                    .style(base),
            ),
            rect,
        );
    }
}

/// A diff line as a side-by-side cell.
fn line_cell(line: &DiffLine) -> Cell {
    let lineno = match line.kind {
        LineKind::Remove => line.old_lineno,
        _ => line.new_lineno,
    };
    Cell {
        lineno: lineno.unwrap_or(0),
        content: line.content.clone(),
        kind: line.kind,
        hunk_line_idx: 0,
    }
}

/// One side of a row: line number, sign and text, `width` wide, in syntax
/// colours from `colours`, with the changed words marked when `segments`
/// says which they are.
///
/// The theme's diff colours are backgrounds (as in editor themes): a changed
/// line keeps its syntax colours on a tinted background, its changed words on
/// a stronger tint, and the sign in a bright version of the tint.
fn diff_spans(
    cell: &Cell,
    width: usize,
    (segments, colours): (Option<&[karet_diff::Segment]>, Option<&colour::FileColours>),
    theme: &Theme,
) -> Vec<Span<'static>> {
    let muted = theme.style(ThemeRole::Muted);
    let surface = crate::styles::surface(theme);
    let tint = match cell.kind {
        LineKind::Add => theme.style(ThemeRole::DiffAdded).fg,
        LineKind::Remove => theme.style(ThemeRole::DiffRemoved).fg,
        LineKind::Context => None,
    };
    let sign = match cell.kind {
        LineKind::Add => '+',
        LineKind::Remove => '-',
        LineKind::Context => ' ',
    };
    let line_style = tint.map_or(surface, |colour| surface.bg(colour));
    let word_bg = tint.map(|colour| brighter(colour, 1));
    let sign_style = tint.map_or(muted, |colour| line_style.fg(brighter(colour, 4)));
    let number = format!("{:>4} ", cell.lineno);
    let room = width.saturating_sub(number.chars().count() + 2);
    let mut spans = vec![
        Span::styled(number, muted),
        Span::styled(format!("{sign} "), sign_style),
    ];
    let runs = colours.map_or(&[][..], |colours| {
        colours.line(cell.kind != LineKind::Remove, cell.lineno)
    });
    // Which characters are changed words, in the line's character order.
    let changed: Vec<bool> = segments.map_or_else(Vec::new, |segments| {
        segments
            .iter()
            .flat_map(|segment| std::iter::repeat_n(segment.changed, segment.text.chars().count()))
            .collect()
    });
    // Each character's style; neighbours with the same style become one span.
    let mut used = 0;
    let mut current: Option<(Style, String)> = None;
    for (idx, (byte, chr)) in cell.content.char_indices().enumerate() {
        let token = runs
            .iter()
            .find(|(start, end, _)| byte >= *start && byte < *end)
            .and_then(|(_, _, token)| theme.token_style(*token).fg);
        let mut style = token.map_or(line_style, |colour| line_style.fg(colour));
        if changed.get(idx).copied().unwrap_or(false)
            && let Some(stronger) = word_bg
        {
            style = style.bg(stronger);
        }
        let text = if chr == '\t' {
            "    ".to_string()
        } else {
            chr.to_string()
        };
        let count = text.chars().count();
        if used + count > room {
            break;
        }
        used += count;
        match &mut current {
            Some((run_style, run)) if *run_style == style => run.push_str(&text),
            _ => {
                if let Some((run_style, run)) = current.take() {
                    spans.push(Span::styled(run, run_style));
                }
                current = Some((style, text));
            }
        }
    }
    if let Some((run_style, run)) = current {
        spans.push(Span::styled(run, run_style));
    }
    spans.push(Span::styled(
        " ".repeat(room.saturating_sub(used)),
        line_style,
    ));
    spans
}

/// `colour` made brighter by `steps` (each roughly doubles its distance from
/// black), for the stronger diff tints.
fn brighter(colour: ratatui::style::Color, steps: u8) -> ratatui::style::Color {
    match colour {
        ratatui::style::Color::Rgb(red, green, blue) => {
            let lift = |part: u8| {
                (0..steps).fold(part, |value, _| value.saturating_mul(2).saturating_add(8))
            };
            ratatui::style::Color::Rgb(lift(red), lift(green), lift(blue))
        }
        other => other,
    }
}

/// `line` as the selected row: every part on the selection background (over
/// any diff tint), so the row stands out across its whole width.
fn as_selected<'a>(line: Line<'a>, theme: &Theme) -> Line<'a> {
    let Some(colour) = theme.style(ThemeRole::Selection).fg else {
        return line.style(Style::default().add_modifier(Modifier::REVERSED));
    };
    let spans = line
        .spans
        .into_iter()
        .map(|span| {
            let style = span.style.bg(colour);
            span.style(style)
        })
        .collect::<Vec<_>>();
    Line::from(spans).style(line.style.bg(colour))
}

/// `text` cut or padded to `width` characters.
fn fit(text: &str, width: usize) -> String {
    let cut: String = text.chars().take(width).collect();
    let pad = width.saturating_sub(cut.chars().count());
    format!("{cut}{}", " ".repeat(pad))
}

/// Added and removed lines in a file.
fn counts(file: &FileDiff) -> (usize, usize) {
    let lines = file.hunks.iter().flat_map(|hunk| &hunk.lines);
    lines.fold((0, 0), |(add, del), line| match line.kind {
        LineKind::Add => (add + 1, del),
        LineKind::Remove => (add, del + 1),
        LineKind::Context => (add, del),
    })
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        format!("1 {word}")
    } else {
        format!("{count} {word}s")
    }
}

fn to_u16(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}
