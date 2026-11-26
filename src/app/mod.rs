//! Application state and event handling. Input, navigation, language server
//! features and drawing live in the submodules.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use crossterm::event::Event as TermEvent;
use crossterm::event::KeyEventKind;
use karet_core::LineCol;
use karet_core::Range;
use karet_theme::Theme;
use tokio::sync::mpsc::UnboundedSender;

use crate::Event;
use crate::allkeys::AllKeys;
use crate::doc::Doc;
use crate::explorer::Explorer;
use crate::find::Find;
use crate::keycheck::KeyCheck;
use crate::keymap::Action;
use crate::keymap::Keymap;
use crate::lsp;
use crate::picker::Kind;
use crate::picker::Picker;

mod code;
mod completion;
mod database;
mod input;
mod navigate;
mod review;
mod view;

pub(crate) use view::doc_lines;

use code::counts;
use code::hover_summary;
use code::one_line;
use code::problem_texts;

/// Each open file's (instance, buffer version), by path.
type Snapshot = std::collections::HashMap<PathBuf, (u64, u64)>;

enum Overlay {
    None,
    Picker(Picker),
    Explorer(Explorer),
    Rename {
        path: PathBuf,
        pos: LineCol,
        text: String,
    },
    Find(Find),
    AllKeys(AllKeys),
    KeyCheck(KeyCheck),
    /// Quick documentation (Alt+H): the hover text under the caret.
    Doc(String),
    /// The result of Run SQL from a file, over the code.
    DbResult(crate::database::grid::Grid),
    /// SQL from a file that writes, shown before it runs.
    DbConfirm(String),
}

/// The full-screen view on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Screen {
    Code,
    Review,
    Database,
}

/// Something the app asks the main loop to do after handling an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Quit,
    /// Suspend into a shell (Alt+T) and come back.
    Shell,
}

pub struct App {
    pub root: PathBuf,
    docs: Vec<Doc>,
    active: usize,
    overlay: Overlay,
    status: String,
    keymap: Keymap,
    theme: Theme,
    files: Vec<PathBuf>,
    recent: Vec<PathBuf>,
    back: Vec<(PathBuf, LineCol)>,
    forward: Vec<(PathBuf, LineCol)>,
    clipboard: String,
    lsp: lsp::Hub,
    tx: UnboundedSender<Event>,
    /// Problems per file as the language servers last reported them, open or
    /// not, ranges in UTF-16 columns.
    problems: std::collections::HashMap<PathBuf, Vec<karet_core::Diagnostic>>,
    /// The same problems per server (key), which `problems` merges.
    problem_parts: std::collections::HashMap<
        PathBuf,
        std::collections::HashMap<&'static str, Vec<karet_core::Diagnostic>>,
    >,
    /// The project tree as it was last left (open folders, selection).
    tree: Option<Explorer>,
    /// Showing the home screen; open files stay loaded behind it.
    home: bool,
    /// The type of the name under the caret, for the bottom line: (file, caret, text).
    type_hint: Option<(PathBuf, LineCol, String)>,
    /// Where the caret was on the last tick, and where hover was last asked;
    /// a caret that rests for a tick gets its type looked up once.
    rested: Option<(PathBuf, LineCol)>,
    hover_asked: Option<(PathBuf, LineCol)>,
    /// An action that showed a "press again" warning (quit or close with unsaved
    /// changes); `armed` is what the previous key left, checked by this key.
    confirm: Option<Action>,
    armed: Option<Action>,
    /// The last find, for F3 / Shift+F3 with the bar closed and to prefill it.
    last_find: Option<Find>,
    /// The code completion popup, while open.
    completion: Option<crate::complete::Completion>,
    /// Bumped for every completion request; an older answer is dropped.
    completion_gen: u64,
    /// The newest document version each server reported problems for, per
    /// file.
    problem_versions: std::collections::HashMap<(PathBuf, &'static str), i32>,
    /// Each open file's (instance, buffer version) when the pending rename
    /// was asked for.
    /// Tagged with the request's id; an answer for another id is dropped.
    rename_snapshot: Option<(u64, Snapshot)>,
    /// Numbers rename requests.
    rename_id: u64,
    /// The last document version of each file closed, to continue from when
    /// it opens again.
    closed_versions: std::collections::HashMap<PathBuf, i32>,
    /// Launcher search generation; bumping it cancels older searches.
    search_gen: Arc<AtomicU64>,
    /// The Review view's state, kept while switching back to the code.
    review: Option<crate::review::Review>,
    /// Which view is on screen.
    screen: Screen,
    /// The Database view's state, kept while switching back to the code.
    database: Option<crate::database::Database>,
    /// The file (and home screen) to go back to when leaving it.
    db_return: Option<(Option<PathBuf>, bool)>,
    /// The query editor's file, once connected.
    db_scratch: Option<PathBuf>,
    /// Where the editor was last drawn, for popups at its caret.
    editor_area: ratatui::layout::Rect,
    /// What the main loop should do next, if anything.
    pub request: Option<Request>,
    /// Whether the terminal speaks the kitty keyboard protocol.
    pub enhanced_keys: bool,
}

impl App {
    pub fn new(root: PathBuf, tx: UnboundedSender<Event>) -> Self {
        let (keymap, problems) = Keymap::load();
        let theme = Theme::dark();
        Self {
            root,
            docs: Vec::new(),
            active: 0,
            overlay: Overlay::None,
            status: problems.join("; "),
            keymap,
            theme,
            files: Vec::new(),
            recent: Vec::new(),
            back: Vec::new(),
            forward: Vec::new(),
            clipboard: String::new(),
            lsp: lsp::Hub::default(),
            tx,
            last_find: None,
            completion: None,
            completion_gen: 0,
            confirm: None,
            armed: None,
            home: false,
            tree: None,
            type_hint: None,
            rested: None,
            hover_asked: None,
            problems: std::collections::HashMap::new(),
            problem_parts: std::collections::HashMap::new(),
            problem_versions: std::collections::HashMap::new(),
            closed_versions: std::collections::HashMap::new(),
            rename_snapshot: None,
            rename_id: 0,
            search_gen: Arc::new(AtomicU64::new(0)),
            request: None,
            review: None,
            screen: Screen::Code,
            database: None,
            db_return: None,
            db_scratch: None,
            editor_area: ratatui::layout::Rect::default(),
            enhanced_keys: false,
        }
    }

    fn in_review(&self) -> bool {
        self.screen == Screen::Review
    }

    fn in_db(&self) -> bool {
        self.screen == Screen::Database
    }

    fn doc(&mut self) -> Option<&mut Doc> {
        self.cur_mut()
    }

    /// The file on screen; none on the home screen, even with files open.
    fn cur(&self) -> Option<&Doc> {
        if self.home {
            None
        } else {
            self.docs.get(self.active)
        }
    }

    fn cur_mut(&mut self) -> Option<&mut Doc> {
        visible(&mut self.docs, self.active, self.home)
    }

    fn rel<'a>(&self, path: &'a Path) -> &'a Path {
        path.strip_prefix(&self.root).unwrap_or(path)
    }

    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Term(TermEvent::Key(key)) if key.kind != KeyEventKind::Release => {
                self.on_key(key);
                self.refresh_completion();
            }
            Event::Term(TermEvent::Paste(text)) => {
                self.on_paste(&text);
                self.refresh_completion();
            }
            Event::Completions(generation, answer) => self.on_completions(generation, answer),
            Event::ReviewSources(sources) => {
                if let Some(review) = &mut self.review {
                    review.pick(sources);
                }
            }
            Event::ReviewChoices(result) => {
                if let Some(review) = &mut self.review {
                    let outcome = review.choices(result);
                    self.review_outcome(outcome);
                }
            }
            Event::ReviewLoaded(result) => {
                if let Some(review) = &mut self.review {
                    review.loaded(result);
                }
            }
            event @ (Event::DbFound(..)
            | Event::DbConnected(..)
            | Event::DbSchema(..)
            | Event::DbResult(..)) => self.db_event(event),
            Event::CompletionResolved(list, index, item) => {
                self.on_completion_resolved(list, index, item);
            }
            Event::Term(_) => {}
            Event::Files(files) => {
                self.files = files;
                let items = self.file_items();
                if let Overlay::Picker(picker) = &mut self.overlay
                    && picker.is_launcher()
                {
                    picker.set_kind_items(Kind::File, items);
                }
            }
            Event::Status(text) => self.status = text,
            Event::LspReady(key, client) => self.on_lsp_ready(key, client),
            Event::LspFailed(key, msg) => {
                if !msg.is_empty() {
                    self.status.clone_from(&msg);
                }
                self.lsp.failed(key, msg);
            }
            Event::Diagnostics(key, path, version, related, diags) => {
                self.on_diagnostics(key, path, (version, related), diags);
            }
            Event::Locations(nav, locs) => self.on_locations(nav, locs),
            Event::Rename(id, edit) => self.on_rename(id, edit),
            Event::Formatted(path, instance, version, edits) => {
                self.on_formatted(&path, (instance, version), edits);
            }
            Event::Launcher(generation, kind, items) => {
                if generation == self.search_gen.load(Ordering::Relaxed)
                    && let Overlay::Picker(picker) = &mut self.overlay
                    && picker.is_launcher()
                {
                    picker.set_kind_items(kind, items);
                }
            }
            Event::Tick => {
                self.sync_lsp();
                self.auto_hover();
            }
            Event::Semantic(path, version, tokens) => {
                // Only for the text on screen: not after an edit the server
                // has not been sent yet.
                if let Some(doc) = self.docs.iter_mut().find(|doc| doc.path == path)
                    && doc.lsp_version == version
                    && doc.lsp_synced == doc.buffer.version()
                {
                    doc.semantic = tokens
                        .into_iter()
                        .filter_map(|token| {
                            // A token running past the end of any line is
                            // malformed; skip it.
                            let end_col = token.start.col.checked_add(token.length)?;
                            let start = doc
                                .buffer
                                .utf16_to_line_col(token.start.line, token.start.col);
                            let end = doc.buffer.utf16_to_line_col(token.start.line, end_col);
                            Some(
                                karet_core::Decoration::new(
                                    Range { start, end },
                                    karet_core::DecorationKind::TextStyle,
                                    None,
                                )
                                .with_fg(token.token),
                            )
                        })
                        .collect();
                }
            }
            Event::Hover(path, pos, text, popup) => self.on_hover(path, pos, text, popup),
        }
    }

    fn on_diagnostics(
        &mut self,
        key: &'static str,
        path: PathBuf,
        (version, related): (Option<i32>, bool),
        diags: Vec<karet_core::Diagnostic>,
    ) {
        // A report about an older version than one already shown is stale.
        // Problems a server mentions for another file in a pull answer carry
        // no version: they do not replace a versioned report for an open
        // file, which gets reports of its own.
        let open = self.docs.iter().any(|doc| doc.path == path);
        if related && open && self.problem_versions.contains_key(&(path.clone(), key)) {
            return;
        }
        if let Some(version) = version {
            let seen = self
                .problem_versions
                .entry((path.clone(), key))
                .or_insert(version);
            if version < *seen {
                return;
            }
            *seen = version;
        }
        // Each server replaces only its own report; the file shows all.
        let parts = self.problem_parts.entry(path.clone()).or_default();
        parts.insert(key, diags);
        let mut merged: Vec<karet_core::Diagnostic> = parts.values().flatten().cloned().collect();
        merged.sort_by_key(|diag| (diag.range.start, diag.severity));
        if let Some(doc) = self.docs.iter_mut().find(|doc| doc.path == path) {
            doc.set_diagnostics(&merged);
        }
        if merged.is_empty() {
            self.problems.remove(&path);
        } else {
            self.problems.insert(path, merged);
        }
    }

    fn on_hover(&mut self, path: PathBuf, pos: LineCol, text: Option<String>, popup: bool) {
        if popup {
            match text {
                Some(hover) if self.here() == Some((path, pos)) => {
                    self.overlay = Overlay::Doc(hover);
                }
                Some(_) => {}
                None => self.status = "No documentation here".into(),
            }
        } else {
            let word = self
                .docs
                .iter()
                .find(|doc| doc.path == path)
                .and_then(Doc::word_at_caret);
            self.type_hint = text.map(|hover| (path, pos, hover_summary(&hover, word.as_deref())));
        }
    }

    fn with_doc(&mut self, apply: impl FnOnce(&mut Doc)) {
        if let Some(doc) = self.doc() {
            apply(doc);
        }
    }

    fn here(&self) -> Option<(PathBuf, LineCol)> {
        self.docs
            .get(self.active)
            .map(|doc| (doc.path.clone(), doc.caret()))
    }

    fn set_clipboard(&mut self, text: String) -> crate::clipboard::Via {
        let via = crate::clipboard::copy(&text);
        self.clipboard = text;
        via
    }

    fn save(&mut self) {
        let servers = self
            .docs
            .get(self.active)
            .and_then(|doc| doc.lsp_lang)
            .map_or_else(Vec::new, |lang| self.lsp.running(lang));
        let Some(doc) = self.doc() else { return };
        match doc.save() {
            Ok(()) => {
                let path = doc.path.clone();
                self.status = format!("Saved {}", self.rel(&path).display());
                self.sync_lsp();
                let version = self.cur().map_or(0, |doc| doc.lsp_version);
                for server in servers {
                    lsp::did_save(server, path.clone(), version, self.tx.clone());
                }
            }
            Err(err) => self.status = format!("Save failed: {err}"),
        }
    }

    /// Start in Check Keys (`sanitty --check-keys`).
    pub fn check_keys(&mut self) {
        self.run(Action::CheckKeys);
    }

    /// Show the project tree, as when sanitty is started on a directory.
    pub fn show_tree(&mut self) {
        self.run(Action::ToggleExplorer);
    }
}

/// The document on screen, borrowing only the fields it needs, so it can be
/// used alongside other parts of `App` (the find bar, the status line).
fn visible(docs: &mut [Doc], active: usize, home: bool) -> Option<&mut Doc> {
    if home { None } else { docs.get_mut(active) }
}

fn plural(count: usize, word: &str) -> String {
    if count == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

fn parent_dir(rel: &Path) -> String {
    rel.parent()
        .map_or_else(String::new, |parent| parent.display().to_string())
}

/// Write the key check report so far to `~/.config/sanitty/keycheck-<terminal>.md`.
fn save_keycheck(check: &mut KeyCheck, report: &str) {
    let path = KeyCheck::report_path();
    if let Some(file) = &path
        && file
            .parent()
            .is_some_and(|dir| std::fs::create_dir_all(dir).is_ok())
        && std::fs::write(file, report).is_ok()
    {
        check.saved = Some(file.clone());
    }
}
