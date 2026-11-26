//! The Review view (Alt+9): the changed files, their diff, comments on lines,
//! and the whole review copied as Markdown. Read-only: nothing is edited here.
//!
//! Loading a diff runs `git`, which the app does off its loop (see
//! [`git::load`]); this module keeps the view's state, handles its keys and
//! draws it.

mod colour;
pub mod git;
mod pick;
mod render;
mod rows;
pub mod state;

use std::path::PathBuf;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use karet_diff::FileDiff;

use crate::util::counted as plural;
use git::Choices;
use git::Entry;
use git::Loaded;
use git::Source;
use git::SourceInfo;
use pick::Picked;
use pick::Picker;
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
    /// Fetch the commits or pull requests to pick from (off the app loop)
    /// and hand them to [`Review::choices`].
    Fetch(Entry),
    /// Go back to the code (Ctrl+C, or the picker closed before anything
    /// was loaded).
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
    /// The state as last read from or written to disk, to merge with what
    /// another sanitty on the same repository wrote since.
    baseline: Saved,
    /// Something to say when the view opens (saved state that could not be
    /// read).
    message: Option<String>,
    /// The diff on screen, once loaded.
    loaded: Option<Loaded>,
    /// Waiting for a diff or the source list.
    loading: bool,
    /// Why the last load failed.
    error: Option<String>,
    /// The source list (or commit or PR list), while it is open.
    picker: Option<Picker>,
    /// Something to know about a source (commits included that were not
    /// picked), shown while that source is on screen.
    notice: Option<(Source, String)>,
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
    /// Numbers each request for sources, choices or a diff; an answer for
    /// an older one (or one whose list was closed meanwhile) is dropped.
    generation: u64,
}

impl Review {
    pub fn new(root: PathBuf) -> Self {
        let (saved, message) = state::load(&root);
        Self {
            root,
            baseline: saved.clone(),
            saved,
            message,
            loaded: None,
            loading: false,
            error: None,
            picker: None,
            notice: None,
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
            generation: 0,
        }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// A new request is going out: the number its answer must carry.
    pub fn request(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Whether an answer is for the latest request.
    pub fn is_current(&self, generation: u64) -> bool {
        generation == self.generation
    }

    /// What to do when the view opens: load the last source again (it may
    /// have changed), or ask which one the first time.
    pub fn open(&mut self) -> Outcome {
        self.loading = true;
        match self.saved.last_source.clone() {
            Some(source) => Outcome::Load(source),
            None => Outcome::Sources,
        }
    }

    /// The sources arrived: show the picker.
    pub fn pick(&mut self, sources: Vec<SourceInfo>) {
        self.loading = false;
        let current = self.source();
        let selected = sources
            .iter()
            .position(|info| matches!(&info.entry, Entry::Load(source) if Some(source) == current.as_ref()))
            .unwrap_or(0);
        self.picker = Some(Picker::Sources {
            list: sources,
            selected,
        });
    }

    /// Commits or pull requests to pick from arrived (or failed).
    pub fn choices(&mut self, result: Result<Choices, String>) -> Outcome {
        self.loading = false;
        match result {
            Ok(Choices::Commits(list)) => {
                self.picker = Some(Picker::Commits {
                    list,
                    query: String::new(),
                    selected: 0,
                    marked: Vec::new(),
                });
                Outcome::Pending
            }
            Ok(Choices::Prs(list)) if list.is_empty() => {
                Outcome::Status("No open pull requests".into())
            }
            Ok(Choices::Prs(list)) => {
                self.picker = Some(Picker::Prs {
                    list,
                    query: String::new(),
                    selected: 0,
                });
                Outcome::Pending
            }
            Err(err) => Outcome::Status(err),
        }
    }

    /// A diff arrived (or failed). An open comment box closes: its lines
    /// belong to the diff that was on screen. Says so, if one was open.
    pub fn loaded(&mut self, result: Result<Loaded, String>) -> Option<String> {
        self.loading = false;
        let closed = result.is_ok() && self.editing.take().is_some();
        let mut message = closed.then(|| "A new diff loaded; the comment box closed".to_string());
        match result {
            Ok(mut loaded) => {
                let same = self
                    .loaded
                    .as_ref()
                    .is_some_and(|old| old.source == loaded.source);
                let path = self.current_file().map(|file| file.path.clone());
                // Comments move with their lines.
                let comments = &mut self.saved.source_mut(&loaded.source).comments;
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
                self.saved.last_source = Some(loaded.source.clone());
                self.loaded = Some(loaded);
                self.error = None;
                self.file = path
                    .filter(|_| same)
                    .and_then(|path| self.files().iter().position(|file| file.path == path))
                    .unwrap_or(0);
                self.reset_rows();
                if let Some(problem) = self.persist() {
                    message = Some(problem);
                }
            }
            Err(err) => self.error = Some(err),
        }
        message
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
        self.loaded.as_ref().map(|loaded| loaded.source.clone())
    }

    fn comments(&self) -> &[Comment] {
        self.loaded
            .as_ref()
            .and_then(|loaded| self.saved.source(&loaded.source))
            .map_or(&[], |state| state.comments.as_slice())
    }

    /// Whether the comment box or a list is open (it takes the keys).
    pub fn has_popup(&self) -> bool {
        self.picker.is_some() || self.editing.is_some()
    }

    /// Something to say once, when the view opens.
    pub fn take_message(&mut self) -> Option<String> {
        self.message.take()
    }

    /// Save, merged with what another instance wrote meanwhile (its comments
    /// then show here too).
    fn persist(&mut self) -> Option<String> {
        match state::save(&self.root, &self.saved, &self.baseline) {
            Ok(written) => {
                let merged = written != self.saved;
                self.saved = written.clone();
                self.baseline = written;
                if merged {
                    self.refresh();
                }
                None
            }
            Err(err) => Some(format!("Could not save the review: {err}")),
        }
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
        // Ctrl+C backs out, as everywhere: out of the comment box or picker
        // (like Esc), else out of the view.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            if self.picker.is_none() && self.editing.is_none() {
                return Outcome::Leave;
            }
            return self.handle_key(&KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        }
        if self.picker.is_some() {
            return self.picker_key(key);
        }
        if self.editing.is_some() {
            return self.editing_key(key);
        }
        // The view's keys are plain letters: with Ctrl or Alt held they are
        // other programs' shortcuts (Alt+D, Ctrl+X), never delete or discard.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Outcome::Pending;
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
            KeyCode::Char('x') => return self.discard(clear),
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
        let Some(picker) = &mut self.picker else {
            return Outcome::Pending;
        };
        match picker.handle_key(key) {
            Picked::Pending => Outcome::Pending,
            Picked::Status(text) => Outcome::Status(text),
            Picked::Close => {
                self.picker = None;
                // A list still on its way is no longer wanted.
                self.generation += 1;
                self.loading = false;
                if self.loaded.is_none() {
                    return Outcome::Leave;
                }
                Outcome::Pending
            }
            Picked::Fetch(entry) => {
                self.loading = true;
                Outcome::Fetch(entry)
            }
            Picked::Load(source, note) => {
                self.picker = None;
                self.loading = true;
                self.notice = note.map(|note| (source.clone(), note));
                Outcome::Load(source)
            }
        }
    }

    fn editing_key(&mut self, key: &KeyEvent) -> Outcome {
        let Some(editing) = &mut self.editing else {
            return Outcome::Pending;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc => self.editing = None,
            KeyCode::Tab => editing.kind = editing.kind.next(),
            KeyCode::Backspace => crate::picker::erase(&mut editing.text, key),
            KeyCode::Char('u') if ctrl => editing.text.clear(),
            KeyCode::Char(chr) if !ctrl && !alt => editing.text.push(chr),
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
        let viewed = &mut self.saved.source_mut(&source).viewed;
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
        self.loaded
            .as_ref()
            .and_then(|loaded| self.saved.source(&loaded.source))
            .is_some_and(|state| state.viewed.get(&file.path) == Some(&state::fingerprint(file)))
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
        files.sort_unstable();
        files.dedup();
        let status = format!(
            "Copied review: {} in {} (Markdown). x discards the review.",
            plural(comments.len(), "comment"),
            plural(files.len(), "file")
        );
        Outcome::Copy(text, status)
    }

    /// `x` twice: throw this review away (its comments and viewed marks),
    /// forget it as the last source, and ask what to review next.
    fn discard(&mut self, confirmed: bool) -> Outcome {
        let Some(source) = self.source() else {
            return Outcome::Pending;
        };
        if !confirmed {
            self.confirm_clear = true;
            let count = self.comments().len();
            let what = if count == 0 {
                String::new()
            } else {
                format!(" and its {}", plural(count, "comment"))
            };
            return Outcome::Status(format!("x again discards this review{what}"));
        }
        match source {
            Source::WorkingTree | Source::Branch => {
                *self.saved.source_mut(&source) = state::SourceState::default();
            }
            other => {
                self.saved.others.remove(&other.key());
            }
        }
        self.saved.last_source = None;
        self.loaded = None;
        self.notice = None;
        self.rows.clear();
        self.reset_rows();
        if let Some(err) = self.persist() {
            return Outcome::Status(err);
        }
        self.loading = true;
        Outcome::Sources
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
        let comments = &mut self.saved.source_mut(&source).comments;
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
        let comments = &mut self.saved.source_mut(&source).comments;
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
}
