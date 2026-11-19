//! Application state, input dispatch and rendering.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::Event as TermEvent;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use karet_core::EditCause;
use karet_core::LineCol;
use karet_core::Location;
use karet_core::Range;
use karet_core::Severity;
use karet_core::TextEdit;
use karet_core::ThemeRole;
use karet_core::VirtualText;
use karet_editor::Editor;
use karet_editor::Motion;
use karet_lsp::LspClient;
use karet_theme::Theme;
use ratatui::Frame;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc::UnboundedSender;

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use crate::Event;
use crate::allkeys;
use crate::allkeys::AllKeys;
use crate::doc::Doc;
use crate::explorer;
use crate::explorer::Explorer;
use crate::find;
use crate::find::Find;
use crate::keycheck;
use crate::keycheck::KeyCheck;
use crate::keymap::Action;
use crate::keymap::Keymap;
use crate::lsp;
use crate::lsp::Nav;
use crate::picker;
use crate::picker::Item;
use crate::picker::Kind;
use crate::picker::Picker;
use crate::picker::Target;

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
    /// Launcher search generation; bumping it cancels older searches.
    search_gen: Arc<AtomicU64>,
    pub quit: bool,
    pub shell_requested: bool,
    /// Whether the terminal speaks the kitty keyboard protocol.
    pub enhanced_keys: bool,
}

impl App {
    pub fn new(root: PathBuf, tx: UnboundedSender<Event>) -> Self {
        let (keymap, problems) = Keymap::load();
        let theme = crate::config_dir()
            .map(|d| d.join("theme.json"))
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|j| Theme::load_vscode(&j).ok())
            .unwrap_or_else(Theme::dark);
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
            confirm: None,
            armed: None,
            home: false,
            tree: None,
            type_hint: None,
            rested: None,
            hover_asked: None,
            problems: std::collections::HashMap::new(),
            search_gen: Arc::new(AtomicU64::new(0)),
            quit: false,
            shell_requested: false,
            enhanced_keys: false,
        }
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

    fn rel<'a>(&self, p: &'a Path) -> &'a Path {
        p.strip_prefix(&self.root).unwrap_or(p)
    }

    // ---------------------------------------------------------------- events

    pub fn handle(&mut self, ev: Event) {
        match ev {
            Event::Term(TermEvent::Key(key)) if key.kind != KeyEventKind::Release => {
                self.on_key(key)
            }
            Event::Term(TermEvent::Paste(text)) => self.on_paste(&text),
            Event::Term(_) => {}
            Event::Files(files) => {
                self.files = files;
                let items = self.file_items();
                if let Overlay::Picker(p) = &mut self.overlay
                    && p.is_launcher()
                {
                    p.set_kind_items(Kind::File, items);
                }
            }
            Event::Status(s) => self.status = s,
            Event::LspReady(key, client) => self.on_lsp_ready(key, client),
            Event::LspFailed(key, msg) => {
                self.status = msg.clone();
                self.lsp.failed(key, msg);
            }
            Event::Diagnostics(path, diags) => {
                if let Some(doc) = self.docs.iter_mut().find(|d| d.path == path) {
                    doc.set_diagnostics(&diags);
                }
                if diags.is_empty() {
                    self.problems.remove(&path);
                } else {
                    self.problems.insert(path, diags);
                }
            }
            Event::Locations(nav, locs) => self.on_locations(nav, locs),
            Event::Rename(edit) => self.on_rename(edit),
            Event::Launcher(generation, kind, items) => {
                if generation == self.search_gen.load(Ordering::Relaxed)
                    && let Overlay::Picker(p) = &mut self.overlay
                    && p.is_launcher()
                {
                    p.set_kind_items(kind, items);
                }
            }
            Event::Tick => {
                self.sync_lsp();
                self.auto_hover();
            }
            Event::Semantic(path, version, tokens) => {
                if let Some(doc) = self.docs.iter_mut().find(|d| d.path == path)
                    && doc.lsp_version == version
                {
                    doc.semantic = tokens
                        .into_iter()
                        .map(|t| {
                            let start = doc.buffer.utf16_to_line_col(t.start.line, t.start.col);
                            let end = doc
                                .buffer
                                .utf16_to_line_col(t.start.line, t.start.col + t.length);
                            karet_core::Decoration::new(
                                Range { start, end },
                                karet_core::DecorationKind::TextStyle,
                                None,
                            )
                            .with_fg(t.token)
                        })
                        .collect();
                }
            }
            Event::Hover(path, pos, text, popup) => {
                if popup {
                    match text {
                        Some(t) if self.here() == Some((path, pos)) => {
                            self.overlay = Overlay::Doc(t)
                        }
                        Some(_) => {}
                        None => self.status = "No documentation here".into(),
                    }
                } else {
                    let word = self
                        .docs
                        .iter()
                        .find(|d| d.path == path)
                        .and_then(Doc::word_at_caret);
                    self.type_hint = text.map(|t| (path, pos, hover_summary(&t, word.as_deref())));
                }
            }
        }
    }

    fn on_paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        match &mut self.overlay {
            Overlay::Rename { text: t, .. } => t.push_str(text.lines().next().unwrap_or("")),
            Overlay::Find(find) => {
                if let Some(doc) = visible(&mut self.docs, self.active, self.home) {
                    find.paste(&text, doc);
                }
            }
            Overlay::None => {
                if let Some(doc) = self.doc() {
                    doc.type_text(&text);
                }
            }
            _ => {}
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Overlays get the key first; an unhandled global action still works
        // from inside them (Alt+1 closes the tree, Ctrl+Shift+N swaps pickers).
        // The key check sees every key raw, Ctrl+C included.
        if let Overlay::KeyCheck(kc) = &mut self.overlay {
            match kc.handle_key(&key) {
                keycheck::Outcome::Pending => {}
                keycheck::Outcome::Close => self.overlay = Overlay::None,
                keycheck::Outcome::Finished(report) => {
                    let name = KeyCheck::terminal_name().replace(['/', ' '], "-");
                    let path = crate::config_dir().map(|d| d.join(format!("keycheck-{name}.md")));
                    if let Some(p) = &path
                        && p.parent()
                            .is_some_and(|d| std::fs::create_dir_all(d).is_ok())
                        && std::fs::write(p, &report).is_ok()
                    {
                        kc.saved = Some(p.clone());
                    }
                    self.set_clipboard(report);
                }
            }
            return;
        }
        // A message lasts until the next key, so the problem under the caret
        // shows again; a pending "press again" lasts exactly one key.
        self.status.clear();
        self.armed = self.confirm.take();
        let action = self.keymap.lookup(&key);
        // Ctrl+C in a popup backs out of it, as in any terminal program;
        // the tree on the home screen is part of that screen, so there it quits.
        if action == Some(Action::Copy) && !matches!(self.overlay, Overlay::None) {
            if self.cur().is_none() && matches!(self.overlay, Overlay::Explorer(_)) {
                self.run(Action::Quit);
            } else {
                self.close_overlay();
            }
            return;
        }
        match &mut self.overlay {
            Overlay::None => {}
            Overlay::Explorer(e) => {
                if matches!(action, Some(Action::ToggleExplorer)) {
                    self.close_overlay();
                    return;
                }
                match e.handle_key(&key) {
                    explorer::Outcome::Pending => {}
                    explorer::Outcome::Close => self.close_overlay(),
                    explorer::Outcome::Open(p) => {
                        self.close_overlay();
                        self.open(&p, None);
                    }
                }
                return;
            }
            Overlay::Picker(p) => {
                if matches!(
                    action,
                    Some(
                        Action::Launcher
                            | Action::GotoFile
                            | Action::GotoSymbol
                            | Action::GotoLine
                            | Action::SearchInFiles
                            | Action::RecentFiles
                            | Action::AllKeys
                            | Action::Quit
                    )
                ) {
                    self.overlay = Overlay::None;
                    self.run(action.unwrap_or(Action::None));
                    return;
                }
                match p.handle_key(&key) {
                    picker::Outcome::Pending => {
                        if p.take_changed() {
                            self.launcher_search();
                        }
                    }
                    picker::Outcome::Cancel => self.overlay = Overlay::None,
                    picker::Outcome::Pick(t) => {
                        self.overlay = Overlay::None;
                        self.pick(t);
                    }
                }
                return;
            }
            Overlay::Rename { path, pos, text } => {
                match key.code {
                    KeyCode::Esc => self.overlay = Overlay::None,
                    KeyCode::Enter => {
                        let (path, pos, name) = (path.clone(), *pos, text.clone());
                        self.overlay = Overlay::None;
                        self.request_rename(path, pos, name);
                    }
                    KeyCode::Backspace => {
                        text.pop();
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        text.push(c)
                    }
                    _ => {}
                }
                return;
            }
            Overlay::AllKeys(k) => {
                match k.handle_key(&key) {
                    allkeys::Outcome::Pending => {}
                    allkeys::Outcome::Close => self.overlay = Overlay::None,
                    allkeys::Outcome::Run(a) => {
                        self.overlay = Overlay::None;
                        self.run(a);
                    }
                }
                return;
            }
            Overlay::Find(find) => {
                let Some(doc) = visible(&mut self.docs, self.active, self.home) else {
                    self.overlay = Overlay::None;
                    return;
                };
                match find.handle_key(&key, doc, &mut self.status) {
                    find::Outcome::Handled => return,
                    find::Outcome::Close => {
                        self.close_overlay();
                        return;
                    }
                    // Not a find key: global actions still work, typing does not.
                    find::Outcome::Ignored => {
                        if let Some(a) = action {
                            self.run(a);
                        }
                        return;
                    }
                }
            }
            Overlay::KeyCheck(_) => return,
            // Any key closes the documentation popup; only Esc is used up by it.
            Overlay::Doc(_) => {
                self.overlay = Overlay::None;
                if key.code == KeyCode::Esc {
                    return;
                }
            }
        }
        if let Some(a) = action {
            self.run(a);
            return;
        }
        self.edit_key(key);
    }

    fn edit_key(&mut self, key: KeyEvent) {
        let Some(doc) = self.doc() else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let motion = match key.code {
            KeyCode::Left if ctrl => Some(Motion::WordLeft),
            KeyCode::Right if ctrl => Some(Motion::WordRight),
            KeyCode::Home if ctrl => Some(Motion::DocStart),
            KeyCode::End if ctrl => Some(Motion::DocEnd),
            KeyCode::Left => Some(Motion::Left),
            KeyCode::Right => Some(Motion::Right),
            KeyCode::Up => Some(Motion::Up),
            KeyCode::Down => Some(Motion::Down),
            KeyCode::Home => Some(Motion::LineStart),
            KeyCode::End => Some(Motion::LineEnd),
            KeyCode::PageUp => Some(Motion::PageUp),
            KeyCode::PageDown => Some(Motion::PageDown),
            _ => None,
        };
        if let Some(m) = motion {
            doc.state.move_carets(&doc.buffer, m, shift);
            return;
        }
        match key.code {
            KeyCode::Esc => doc.state.collapse_to_primary(),
            KeyCode::Enter => doc.newline(),
            KeyCode::Tab => doc.type_text("    "),
            KeyCode::Backspace => doc.backspace(ctrl),
            KeyCode::Delete => doc.delete(ctrl),
            KeyCode::Char(c) if !ctrl && !alt => {
                let mut b = [0u8; 4];
                doc.type_text(c.encode_utf8(&mut b));
            }
            _ => {}
        }
        if let Some(doc) = self.doc()
            && doc.state.selection_range().is_some_and(|r| r.is_empty())
        {
            doc.state.clear_selection();
        }
    }

    // --------------------------------------------------------------- actions

    fn run(&mut self, action: Action) {
        match action {
            Action::None => {}
            Action::Quit => {
                let dirty: Vec<String> = self
                    .docs
                    .iter()
                    .filter(|d| d.buffer.is_dirty())
                    .map(|d| self.rel(&d.path).display().to_string())
                    .collect();
                if dirty.is_empty() || self.armed == Some(Action::Quit) {
                    self.quit = true;
                } else {
                    self.status = format!(
                        "Unsaved: {}. Press quit again to discard.",
                        dirty.join(", ")
                    );
                    self.confirm = Some(Action::Quit);
                }
            }
            Action::Save => self.save(),
            Action::Launcher => self.open_launcher(Kind::All, ""),
            Action::GotoFile => self.open_launcher(Kind::File, ""),
            Action::GotoSymbol => self.open_launcher(Kind::Symbol, ""),
            Action::GotoLine => self.open_launcher(Kind::All, ":"),
            Action::SearchInFiles => {
                let seed = self
                    .docs
                    .get(self.active)
                    .and_then(Doc::selected_text)
                    .filter(|t| !t.contains('\n'))
                    .unwrap_or_default();
                self.open_launcher(Kind::Text, &seed);
            }
            Action::AllKeys => {
                let keys = AllKeys::new(|a| self.display_key(a));
                self.overlay = Overlay::AllKeys(keys);
            }
            Action::CheckKeys => self.overlay = Overlay::KeyCheck(KeyCheck::new()),
            Action::Find | Action::Replace => {
                let replace = action == Action::Replace;
                if let Overlay::Find(f) = &mut self.overlay {
                    // Already open: Ctrl+R adds the replace field, Ctrl+F refocuses the query.
                    if replace && f.replace.is_none() {
                        f.replace = Some(String::new());
                    }
                    f.in_replace = replace;
                    return;
                }
                let Some(doc) = visible(&mut self.docs, self.active, self.home) else {
                    return;
                };
                let mut find = Find::open(doc, replace, self.last_find.as_ref());
                find.search(doc);
                find.show(doc);
                self.overlay = Overlay::Find(find);
            }
            Action::FindNext | Action::FindPrev => {
                let forward = action == Action::FindNext;
                let Some(doc) = visible(&mut self.docs, self.active, self.home) else {
                    return;
                };
                match &mut self.overlay {
                    Overlay::Find(f) => f.step(doc, forward),
                    _ => {
                        if let Some(f) = &mut self.last_find
                            && !f.jump_from_caret(doc, forward)
                        {
                            self.status = format!("\"{}\" not found", f.query);
                        }
                    }
                }
            }
            Action::RecentFiles => {
                let items = self
                    .recent
                    .iter()
                    .map(|p| Item {
                        label: file_name(p),
                        detail: parent_dir(self.rel(p)),
                        target: Target::File(p.clone()),
                        kind: Kind::File,
                        keywords: "",
                    })
                    .collect();
                let mut p = Picker::new("Recent Files", items, true);
                // The current file is first; preselect the one before it. On
                // the home screen nothing is current, so the latest is first.
                p.select(usize::from(!self.home));
                self.overlay = Overlay::Picker(p);
            }
            Action::ToggleExplorer => {
                let reveal = self.cur().map(|d| d.path.clone());
                self.open_tree(reveal);
            }
            Action::GotoDefinition => self.request_nav(Nav::Definition),
            Action::QuickDoc => self.request_hover(true),
            Action::Problems => self.open_launcher(Kind::Problem, ""),
            Action::NextProblem | Action::PrevProblem => {
                self.step_problem(action == Action::NextProblem)
            }
            Action::GotoImplementation => self.request_nav(Nav::Implementation),
            Action::FindUsages => self.request_nav(Nav::Usages),
            Action::Rename => {
                if let Some(doc) = self.cur() {
                    let text = doc.word_at_caret().unwrap_or_default();
                    self.overlay = Overlay::Rename {
                        path: doc.path.clone(),
                        pos: doc.caret_utf16(),
                        text,
                    };
                }
            }
            Action::DuplicateLine => self.with_doc(Doc::duplicate_line),
            Action::DeleteLine => self.with_doc(Doc::delete_line),
            Action::ToggleComment => self.with_doc(Doc::toggle_comment),
            Action::Undo => self.with_doc(Doc::undo),
            Action::Redo => self.with_doc(Doc::redo),
            Action::SelectAll => self.with_doc(|d| d.state.select_all(&d.buffer)),
            // Ctrl+C copies a selection; with nothing selected it quits, as a
            // terminal program should. Copy Line covers PyCharm's line copy.
            // Ctrl+C: copy a selection; else leave the file for the home
            // screen; on the home screen, quit.
            Action::Copy => match self.cur().and_then(Doc::selected_text) {
                Some(text) => self.set_clipboard(text),
                None if self.cur().is_some() => {
                    // Home, with the tree open on the file just left.
                    let left = self.cur().map(|d| d.path.clone());
                    self.home = true;
                    self.open_tree(left);
                    self.status = format!(
                        "{} again quits; {} brings your files back",
                        self.display_key(Action::Quit),
                        self.display_key(Action::RecentFiles)
                    );
                }
                None => self.run(Action::Quit),
            },
            Action::CopyLine => {
                if let Some(doc) = self.cur() {
                    let text = doc.current_line_text();
                    self.set_clipboard(text);
                    self.status = "Copied line".into();
                }
            }
            Action::Cut => {
                if let Some(text) = self.doc().map(Doc::cut) {
                    self.set_clipboard(text);
                }
            }
            Action::Paste => {
                let text = self.clipboard.clone();
                if let Some(doc) = self.doc() {
                    let caret = doc.caret();
                    // A whole-line copy pastes above the current line, as in PyCharm.
                    if text.ends_with('\n')
                        && doc.state.selection_range().is_none_or(|r| r.is_empty())
                    {
                        doc.state.place_caret(LineCol::new(caret.line, 0));
                        doc.type_text(&text);
                        let c =
                            LineCol::new(caret.line + text.matches('\n').count() as u32, caret.col);
                        doc.state.place_caret(doc.buffer.clamp(c));
                    } else {
                        let base = doc.buffer.version();
                        let edit = karet_editor::editing::insert(
                            caret,
                            doc.state.selection_range(),
                            base,
                            &text,
                        );
                        doc.apply(edit, EditCause::Paste);
                    }
                }
            }
            Action::CloseFile => {
                if !self.home && self.active < self.docs.len() {
                    if self.docs[self.active].buffer.is_dirty()
                        && self.armed != Some(Action::CloseFile)
                    {
                        self.status = "Unsaved changes: close again to discard".into();
                        self.confirm = Some(Action::CloseFile);
                        return;
                    }
                    let closed = self.docs.remove(self.active).path;
                    self.status.clear();
                    // Fall back to the most recently used file that is still open.
                    self.active = self
                        .recent
                        .iter()
                        .filter(|p| **p != closed)
                        .find_map(|p| self.docs.iter().position(|d| d.path == *p))
                        .unwrap_or(0);
                }
            }
            Action::PreviousFile => {
                let current = self.cur().map(|d| d.path.clone());
                match self
                    .recent
                    .iter()
                    .find(|p| Some(*p) != current.as_ref())
                    .cloned()
                {
                    Some(p) => self.open(&p, None),
                    None => self.status = "No previous file".into(),
                }
            }
            Action::NavBack => {
                if let Some((p, pos)) = self.back.pop() {
                    if let Some(here) = self.here() {
                        self.forward.push(here);
                    }
                    self.open(&p, Some(pos));
                }
            }
            Action::NavForward => {
                if let Some((p, pos)) = self.forward.pop() {
                    if let Some(here) = self.here() {
                        self.back.push(here);
                    }
                    self.open(&p, Some(pos));
                }
            }
            Action::Shell => self.shell_requested = true,
        }
    }

    fn with_doc(&mut self, f: impl FnOnce(&mut Doc)) {
        if let Some(d) = self.doc() {
            f(d);
        }
    }

    fn here(&self) -> Option<(PathBuf, LineCol)> {
        self.docs
            .get(self.active)
            .map(|d| (d.path.clone(), d.caret()))
    }

    fn set_clipboard(&mut self, text: String) {
        crate::osc52_copy(&text);
        self.clipboard = text;
    }

    fn save(&mut self) {
        let client = self
            .docs
            .get(self.active)
            .and_then(|d| d.lsp_lang)
            .and_then(|l| self.lsp.client(l));
        let Some(doc) = self.doc() else { return };
        match doc.save() {
            Ok(()) => {
                let path = doc.path.clone();
                self.status = format!("Saved {}", self.rel(&path).display());
                self.sync_lsp();
                if let Some(c) = client {
                    lsp::did_save(c, path, self.tx.clone());
                }
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    /// F8 / Shift+F8: the next problem in this file, else the first one in the
    /// next file with problems (files in path order, wrapping around).
    fn step_problem(&mut self, forward: bool) {
        let key = |p: &LineCol| (p.line, p.col);
        let current = self.cur().map(|d| d.path.clone());
        if let Some(doc) = self.doc() {
            let c = doc.caret();
            let mut starts: Vec<LineCol> = doc
                .diagnostics
                .iter()
                .filter(|d| counts(d.severity))
                .map(|d| d.range.start)
                .collect();
            starts.sort_by_key(key);
            let target = if forward {
                starts.iter().find(|p| key(p) > key(&c))
            } else {
                starts.iter().rev().find(|p| key(p) < key(&c))
            };
            if let Some(p) = target.copied() {
                doc.goto(p);
                return;
            }
        }
        let mut files: Vec<&PathBuf> = self
            .problems
            .iter()
            .filter(|(_, d)| d.iter().any(|d| counts(d.severity)))
            .map(|(p, _)| p)
            .collect();
        files.sort();
        if files.is_empty() {
            self.status = "No problems".into();
            return;
        }
        let pos = current
            .as_ref()
            .map_or(0, |c| files.partition_point(|p| *p <= c));
        let next = if forward {
            files[pos % files.len()]
        } else {
            let here = current
                .as_ref()
                .and_then(|c| files.iter().position(|p| *p == c))
                .unwrap_or(pos);
            files[(here + files.len() - 1) % files.len()]
        };
        let next = next.clone();
        let mut starts: Vec<LineCol> = self.problems[&next]
            .iter()
            .filter(|d| counts(d.severity))
            .map(|d| d.range.start)
            .collect();
        starts.sort_by_key(key);
        let first = if forward {
            starts.first()
        } else {
            starts.last()
        };
        if let Some(p) = first.copied() {
            // Ranges here are still UTF-16; `jump` converts.
            self.jump(&next, p);
        }
    }

    /// Launcher items for every reported problem, errors first.
    fn problem_items(&self) -> Vec<Item> {
        let mut all: Vec<(&PathBuf, &karet_core::Diagnostic)> = self
            .problems
            .iter()
            .flat_map(|(p, ds)| {
                ds.iter()
                    .filter(|d| counts(d.severity))
                    .map(move |d| (p, d))
            })
            .collect();
        all.sort_by(|a, b| {
            a.1.severity
                .cmp(&b.1.severity)
                .then(a.0.cmp(b.0))
                .then(a.1.range.start.line.cmp(&b.1.range.start.line))
        });
        all.into_iter()
            .map(|(p, d)| Item {
                label: format!("{}: {}", severity_word(d.severity), one_line(&d.message)),
                detail: format!("{}:{}", self.rel(p).display(), d.range.start.line + 1),
                target: Target::Location(p.clone(), d.range.start),
                kind: Kind::Problem,
                keywords: "",
            })
            .collect()
    }

    /// Close the current popup; a closing find bar is kept for F3.
    /// The find bar is kept for F3, the tree to reopen where it was left.
    fn close_overlay(&mut self) {
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Find(f) => self.last_find = Some(f),
            Overlay::Explorer(e) => self.tree = Some(e),
            _ => {}
        }
    }

    /// Open the tree as it was left, refreshed, with `reveal` selected.
    fn open_tree(&mut self, reveal: Option<PathBuf>) {
        let mut tree = self
            .tree
            .take()
            .unwrap_or_else(|| Explorer::new(&self.root, None));
        tree.reveal(reveal.as_deref());
        self.overlay = Overlay::Explorer(tree);
    }

    fn open_launcher(&mut self, tab: Kind, query: &str) {
        let mut items = self.file_items();
        items.extend(
            Action::ALL
                .iter()
                .filter(|(a, ..)| *a != Action::Launcher)
                .map(|(a, label, _)| Item {
                    label: (*label).to_string(),
                    detail: self.display_key(*a),
                    target: Target::Action(*a),
                    kind: Kind::Action,
                    keywords: a.keywords(),
                }),
        );
        items.extend(self.problem_items());
        self.overlay = Overlay::Picker(Picker::launcher(items, tab, query));
        self.launcher_search();
    }

    /// Start (or cancel) the async parts of a launcher query.
    fn launcher_search(&mut self) {
        let Overlay::Picker(p) = &mut self.overlay else {
            return;
        };
        let (scope, query) = p.scope();
        let query = query.to_string();
        let generation = self.search_gen.fetch_add(1, Ordering::Relaxed) + 1;
        let min_text = if scope == Kind::All { 3 } else { 2 };
        if matches!(scope, Kind::All | Kind::Text) && query.chars().count() >= min_text {
            crate::search::text(
                self.root.clone(),
                query.clone(),
                generation,
                self.search_gen.clone(),
                self.tx.clone(),
            );
        } else {
            p.set_kind_items(Kind::Text, Vec::new());
        }
        let client = self
            .docs
            .get(self.active)
            .and_then(|d| d.lsp_lang)
            .and_then(|l| self.lsp.client(l))
            .or_else(|| self.lsp.any_client());
        if matches!(scope, Kind::All | Kind::Symbol)
            && !query.is_empty()
            && let Some(c) = client
        {
            crate::search::symbols(c, self.root.clone(), query, generation, self.tx.clone());
        } else {
            p.set_kind_items(Kind::Symbol, Vec::new());
        }
    }

    /// Files for the launcher: recently used first, then the project index.
    fn file_items(&self) -> Vec<Item> {
        let item = |p: &PathBuf| Item {
            label: file_name(p),
            detail: parent_dir(self.rel(p)),
            target: Target::File(p.clone()),
            kind: Kind::File,
            keywords: "",
        };
        let mut items: Vec<Item> = self.recent.iter().map(item).collect();
        items.extend(
            self.files
                .iter()
                .filter(|p| !self.recent.contains(p))
                .map(item),
        );
        items
    }

    fn pick(&mut self, t: Target) {
        match t {
            Target::File(p) => self.open(&p, None),
            Target::Location(p, pos) => self.jump(&p, pos),
            Target::Action(a) => self.run(a),
            Target::Line(line, col) => {
                if let Some(here) = self.here() {
                    self.back.push(here);
                    self.forward.clear();
                }
                if let Some(doc) = self.doc() {
                    doc.goto(LineCol::new(line.saturating_sub(1), col.saturating_sub(1)));
                }
            }
        }
    }

    /// Open (or switch to) a file, optionally placing the caret. `pos` is in chars.
    pub fn open(&mut self, path: &Path, pos: Option<LineCol>) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.home = false;
        if let Some(i) = self.docs.iter().position(|d| d.path == path) {
            self.active = i;
        } else {
            match Doc::open(&path) {
                Ok(doc) => {
                    let mut doc = doc;
                    if let Some(diags) = self.problems.get(&path) {
                        doc.set_diagnostics(diags);
                    }
                    let lang = doc.lsp_lang;
                    let (text, version) = (doc.buffer.text(), doc.lsp_version);
                    self.docs.push(doc);
                    self.active = self.docs.len() - 1;
                    if let Some(lang) = lang {
                        match self.lsp.client(lang) {
                            Some(c) => {
                                lsp::did_open(c, path.clone(), lang, version, text, self.tx.clone())
                            }
                            None => self.lsp.ensure(lang, &self.root, &self.tx),
                        }
                    }
                }
                Err(e) => {
                    self.status = format!("Cannot open {}: {e}", path.display());
                    return;
                }
            }
        }
        if let Some(pos) = pos
            && let Some(doc) = self.doc()
        {
            doc.goto(pos);
        }
        self.recent.retain(|p| *p != path);
        self.recent.insert(0, path);
        self.recent.truncate(50);
    }

    /// Jump to an LSP location (UTF-16 columns), recording where we came from.
    fn jump(&mut self, path: &Path, pos16: LineCol) {
        if let Some(here) = self.here() {
            self.back.push(here);
            self.forward.clear();
        }
        self.open(path, None);
        if let Some(doc) = self.doc()
            && doc.path.as_path()
                == std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        {
            let pos = doc.buffer.utf16_to_line_col(pos16.line, pos16.col);
            doc.goto(pos);
        }
    }

    // ------------------------------------------------------------------- LSP

    fn on_lsp_ready(&mut self, key: &'static str, client: Arc<LspClient>) {
        self.lsp.ready(key, client.clone());
        for doc in &mut self.docs {
            if let Some(lang) = doc.lsp_lang.filter(|l| lsp::Hub::is_key(l, key)) {
                doc.lsp_synced = doc.buffer.version();
                lsp::did_open(
                    client.clone(),
                    doc.path.clone(),
                    lang,
                    doc.lsp_version,
                    doc.buffer.text(),
                    self.tx.clone(),
                );
            }
        }
    }

    /// Send full text for every document edited since the last sync.
    fn sync_lsp(&mut self) {
        for i in 0..self.docs.len() {
            let doc = &self.docs[i];
            if doc.buffer.version() == doc.lsp_synced {
                continue;
            }
            let Some(client) = doc.lsp_lang.and_then(|l| self.lsp.client(l)) else {
                continue;
            };
            let doc = &mut self.docs[i];
            doc.lsp_version += 1;
            doc.lsp_synced = doc.buffer.version();
            lsp::did_change(
                client,
                doc.path.clone(),
                doc.lsp_version,
                doc.buffer.text(),
                self.tx.clone(),
            );
        }
    }

    /// Look up the type of the name the caret has rested on for a tick.
    fn auto_hover(&mut self) {
        let here = self.here();
        let rested = here.is_some() && self.rested == here;
        self.rested.clone_from(&here);
        if !rested || self.hover_asked == here || !matches!(self.overlay, Overlay::None) {
            return;
        }
        if self
            .docs
            .get(self.active)
            .and_then(Doc::word_at_caret)
            .is_none()
        {
            return;
        }
        self.request_hover(false);
    }

    fn request_hover(&mut self, popup: bool) {
        self.sync_lsp();
        let Some(doc) = self.cur() else {
            return;
        };
        let Some(client) = doc.lsp_lang.and_then(|l| self.lsp.client(l)) else {
            if popup {
                self.status = "No language server for this file".into();
            }
            return;
        };
        let (path, caret, caret16) = (doc.path.clone(), doc.caret(), doc.caret_utf16());
        self.hover_asked = Some((path.clone(), caret));
        lsp::hover(client, path, caret, caret16, popup, self.tx.clone());
    }

    fn request_nav(&mut self, nav: Nav) {
        self.sync_lsp();
        let Some(doc) = self.cur() else {
            return;
        };
        // Style variables are found by searching the project, no server needed.
        let c = doc.caret();
        let line = doc.buffer.line(c.line as usize).unwrap_or_default();
        if let Some(name) = crate::stylenav::variable_at(&doc.path, &line, c.col as usize) {
            let open = self
                .docs
                .iter()
                .map(|d| (d.path.clone(), d.buffer.text()))
                .collect();
            self.status = format!("Searching for {name}...");
            crate::stylenav::search(self.root.clone(), name, nav, open, self.tx.clone());
            return;
        }
        let Some(lang) = doc.lsp_lang else {
            self.status = "No language server for this file type".into();
            return;
        };
        let Some(client) = self.lsp.client(lang) else {
            self.status = format!(
                "Language server for {lang} is not running ({})",
                self.lsp.status()
            );
            return;
        };
        let (path, caret16) = (doc.path.clone(), doc.caret_utf16());
        self.status = format!("{nav:?}...");
        lsp::navigate(client, nav, path, caret16, self.tx.clone());
    }

    fn on_locations(&mut self, nav: Nav, locs: Vec<Location>) {
        self.status.clear();
        let here = self
            .docs
            .get(self.active)
            .map(|d| (d.path.clone(), d.caret_utf16()));
        // PyCharm: Ctrl+B on a declaration shows its usages instead.
        if let (Nav::Definition, [only], Some((path, caret))) = (nav, locs.as_slice(), &here)
            && only.path == *path
            && contains(only.range, *caret)
        {
            self.request_nav(Nav::Usages);
            return;
        }
        match locs.as_slice() {
            [] => self.status = format!("{nav:?}: nothing found"),
            [only] if !matches!(nav, Nav::Usages) => {
                self.jump(&only.path.clone(), only.range.start)
            }
            _ => {
                let items = locs
                    .iter()
                    .map(|l| {
                        let (label, detail) = lsp::describe(l, &self.root);
                        Item {
                            label,
                            detail,
                            target: Target::Location(l.path.clone(), l.range.start),
                            kind: Kind::File,
                            keywords: "",
                        }
                    })
                    .collect();
                let title = match nav {
                    Nav::Usages => "Usages",
                    Nav::Implementation => "Implementations",
                    Nav::Definition => "Declarations",
                };
                self.overlay = Overlay::Picker(Picker::new(title, items, true));
            }
        }
    }

    fn request_rename(&mut self, path: PathBuf, pos: LineCol, name: String) {
        let Some(client) = self
            .docs
            .get(self.active)
            .and_then(|d| d.lsp_lang)
            .and_then(|l| self.lsp.client(l))
        else {
            self.status = "No language server for rename".into();
            return;
        };
        self.sync_lsp();
        lsp::rename(client, path, pos, name, self.tx.clone());
    }

    fn on_rename(&mut self, edit: lsp::Edit) {
        let here = self.here();
        let mut batches = edit.changes;
        let mut skipped = 0;
        for change in edit.document_changes {
            match change {
                karet_core::DocumentChange::Edit(e) => batches.push((e.path, e.edits)),
                _ => skipped += 1,
            }
        }
        let mut files = 0;
        for (path, edits) in batches {
            self.open(&path, None);
            if let Some(doc) = self.doc() {
                let edits: Vec<TextEdit> = edits
                    .into_iter()
                    .map(|e| TextEdit {
                        range: doc.range_from_utf16(e.range),
                        new_text: e.new_text,
                    })
                    .collect();
                if doc.apply_edits(edits) {
                    files += 1;
                }
            }
        }
        if let Some((path, _)) = here
            && let Some(i) = self.docs.iter().position(|d| d.path == path)
        {
            self.active = i;
        }
        self.status = format!("Renamed in {files} file(s); save with Ctrl+S");
        if skipped > 0 {
            self.status.push_str(&format!(
                " ({skipped} file create/rename/delete ops skipped)"
            ));
        }
    }

    // ---------------------------------------------------------------- render

    pub fn render(&mut self, f: &mut Frame) {
        let area = f.area();
        f.render_widget(Block::new().style(crate::ui::surface(&self.theme)), area);
        let find_rows = match &self.overlay {
            Overlay::Find(f) => 1 + u16::from(f.replace.is_some()),
            _ => 0,
        };
        let [top, body, find_area, status, hints] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(find_rows),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        self.render_top(f, top);
        let mut decorations = match &self.overlay {
            Overlay::Find(f) => f.decorations(),
            _ => Vec::new(),
        };
        if let Some(doc) = self.cur() {
            decorations.extend(doc.semantic.iter().cloned());
            decorations.extend(doc.extra_colours.iter().cloned());
        }
        let mut inline = self
            .docs
            .get(self.active)
            .map(problem_texts)
            .unwrap_or_default();
        // A swatch cell in front of each colour value; painted in the colour
        // itself once the editor has placed it.
        let swatch_from = inline.len();
        if let Some(doc) = self.docs.get(self.active) {
            inline.extend(doc.swatches.iter().map(|(at, _)| {
                VirtualText::inline(at.line, at.col, "\u{25a0} ", ThemeRole::Muted)
            }));
        }
        if let Some(doc) = visible(&mut self.docs, self.active, self.home) {
            // Frame a jump target a third of the way down, as PyCharm does.
            if let Some(line) = doc.frame_line.take() {
                doc.state
                    .scroll_to_line(line.saturating_sub(u32::from(body.height / 3)));
            }
            let mut editor = Editor::new(&doc.buffer)
                .theme(&self.theme)
                .focused(matches!(self.overlay, Overlay::None))
                .cell_caret(true)
                .diagnostics(&doc.diagnostics)
                .decorations(&decorations)
                .virtual_text(&inline);
            if let Some(h) = &doc.highlights {
                editor = editor.highlights(h);
            }
            f.render_stateful_widget(editor, body, &mut doc.state);
            for (i, (_, [r, g, b, _])) in doc.swatches.iter().enumerate() {
                if let Some(rect) = doc.state.virtual_text_rect(swatch_from + i)
                    && let Some(cell) = f.buffer_mut().cell_mut((rect.x, rect.y))
                {
                    cell.set_fg(ratatui::style::Color::Rgb(*r, *g, *b));
                }
            }
        } else {
            self.render_welcome(f, body);
        }
        self.render_status(f, status);
        self.render_hints(f, hints);
        let theme = &self.theme;
        match &mut self.overlay {
            Overlay::None => {}
            Overlay::Picker(p) => {
                if p.root.is_none() {
                    p.root = Some(self.root.clone());
                }
                p.render(f, body, theme);
            }
            Overlay::Explorer(e) => e.render(f, body, theme),
            Overlay::Rename { text, .. } => {
                let w = 60.min(body.width);
                let rect = Rect {
                    x: body.x + (body.width - w) / 2,
                    y: body.y + 2,
                    width: w,
                    height: 3,
                };
                f.render_widget(Clear, rect);
                let block = Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(" Rename (Enter to apply) ")
                    .style(crate::ui::surface(theme));
                let inner = block.inner(rect);
                f.render_widget(Paragraph::new(text.as_str()).block(block), rect);
                f.set_cursor_position((inner.x + text.chars().count() as u16, inner.y));
            }
            Overlay::Find(find) => render_find(f, find_area, find, theme),
            Overlay::AllKeys(k) => k.render(f, body, theme),
            Overlay::KeyCheck(kc) => kc.render(f, body, theme),
            Overlay::Doc(text) => {
                let caret = self
                    .docs
                    .get(self.active)
                    .and_then(|d| d.state.primary_caret_cell(body, &d.buffer, &[]));
                render_doc(f, body, caret, text, theme);
            }
        }
    }

    /// The nano-style line of keys for whatever has focus right now.
    fn hints(&self) -> Vec<(String, &'static str)> {
        let k = |a: Action| self.display_key(a);
        let s = |key: &str, label: &'static str| (key.to_string(), label);
        match &self.overlay {
            Overlay::Doc(_) => vec![s("Esc", "Close"), s("Any key", "Close and continue")],
            Overlay::KeyCheck(_) => vec![
                s("Esc", "Did not arrive"),
                s("Backspace", "Redo previous step"),
            ],
            Overlay::AllKeys(_) => vec![s("Type", "Filter"), s("Enter", "Run"), s("Esc", "Close")],
            Overlay::Picker(p) if p.is_launcher() => vec![
                s("Enter", "Open"),
                s("Tab", "Next filter"),
                s(">", "Actions"),
                s("@", "Symbols"),
                s("/", "Text"),
                s(":", "Line"),
                s("Esc", "Close"),
            ],
            Overlay::Picker(_) => vec![s("Enter", "Open"), s("Up/Down", "Move"), s("Esc", "Close")],
            Overlay::Explorer(_) => vec![
                s("Type", "Search"),
                s("Up/Down", "Next match"),
                s("Right", "Expand"),
                s("Left", "Collapse"),
                s("Enter", "Open"),
                s("Esc", "Close"),
            ],
            Overlay::Rename { .. } => vec![s("Enter", "Rename"), s("Esc", "Cancel")],
            Overlay::Find(find) => {
                let prev = if self.enhanced_keys {
                    "Shift+Enter"
                } else {
                    "Up"
                };
                let mut v = if find.in_replace {
                    vec![
                        s("Enter", "Replace"),
                        s("Alt+A", "Replace all"),
                        s("Tab", "Find field"),
                    ]
                } else {
                    vec![s("Enter", "Next"), s(prev, "Previous")]
                };
                v.push(s("Alt+C", if find.case { "Case: on" } else { "Case" }));
                v.push(s("Alt+W", if find.word { "Words: on" } else { "Words" }));
                v.push(s("Alt+X", if find.regex { "Regex: on" } else { "Regex" }));
                if find.replace.is_none() {
                    v.push((k(Action::Replace), "Replace"));
                } else if !find.in_replace {
                    v.push(s("Tab", "Replace field"));
                }
                v.push(s("Esc", "Close"));
                v
            }
            Overlay::None if self.cur().is_none() => vec![
                (k(Action::Launcher), "Search everywhere"),
                (k(Action::RecentFiles), "Recent"),
                (k(Action::ToggleExplorer), "Tree"),
                (k(Action::Shell), "Shell"),
                (k(Action::Quit), "Quit"),
                (k(Action::AllKeys), "All keys"),
            ],
            Overlay::None => {
                let mut v = vec![(k(Action::Launcher), "Search")];
                let any_problems = self
                    .problems
                    .values()
                    .any(|ds| ds.iter().any(|d| counts(d.severity)))
                    || self
                        .docs
                        .get(self.active)
                        .is_some_and(|d| d.diagnostics.iter().any(|d| counts(d.severity)));
                if any_problems {
                    v.push((k(Action::NextProblem), "Next problem"));
                }
                v.extend([
                    (k(Action::Find), "Find"),
                    (k(Action::GotoDefinition), "Definition"),
                    (k(Action::FindUsages), "Usages"),
                    (k(Action::RecentFiles), "Recent"),
                    (k(Action::ToggleExplorer), "Tree"),
                    (k(Action::Save), "Save"),
                    (k(Action::AllKeys), "All keys"),
                ]);
                v
            }
        }
    }

    fn render_hints(&self, f: &mut Frame, area: Rect) {
        let base = crate::ui::surface(&self.theme);
        // Plain text, keys in the keyword colour: readable without shouting.
        let key_style = self.theme.token_style(karet_core::TokenId::KEYWORD);
        let label_style = self
            .theme
            .token_style(karet_core::StandardToken::Punctuation.id());
        let mut hints: Vec<(String, &str)> = self
            .hints()
            .into_iter()
            .filter(|(k, _)| !k.is_empty())
            .collect();
        // Each hint renders as "key label" plus a three-space gap, after a one-space margin.
        let width = |h: &[(String, &str)]| {
            1 + h
                .iter()
                .map(|(k, l)| k.chars().count() + l.chars().count() + 4)
                .sum::<usize>()
        };
        // Drop hints from the end, but keep the last one (All keys / Close).
        while hints.len() > 1 && width(&hints) > area.width as usize {
            hints.remove(hints.len() - 2);
        }
        let mut spans = vec![Span::raw(" ")];
        for (key, label) in hints {
            spans.push(Span::styled(key, key_style));
            spans.push(Span::styled(format!(" {label}   "), label_style));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).style(base), area);
    }

    /// The top bar: what you are looking at. File, unsaved mark, its problems,
    /// files elsewhere with problems, language server.
    fn render_top(&self, f: &mut Frame, area: Rect) {
        let style = crate::ui::bar(&self.theme);
        let err = self.theme.style(ThemeRole::DiagnosticError);
        let warn = self.theme.style(ThemeRole::DiagnosticWarning);
        let ok = self.theme.token_style(karet_core::TokenId::STRING);
        let muted = self.theme.style(ThemeRole::Muted);
        let mut left: Vec<Span> = Vec::new();
        let current = self.cur();
        match current {
            Some(doc) => {
                let dirty = if doc.buffer.is_dirty() { " *" } else { "" };
                left.push(Span::styled(
                    format!(" {}{dirty}   ", self.rel(&doc.path).display()),
                    style.add_modifier(Modifier::BOLD),
                ));
                if let Some(column) = doc.table_column() {
                    left.push(Span::styled(format!("{column}   "), muted));
                }
                // Files something checks: a language server, or the CSV checker.
                if doc.lsp_lang.is_some() || doc.table.is_some() {
                    let n = |sev| doc.diagnostics.iter().filter(|d| d.severity == sev).count();
                    let (errs, warns) = (n(Severity::Error), n(Severity::Warning));
                    if errs + warns == 0 {
                        left.push(Span::styled("no problems   ", ok));
                    }
                    if errs > 0 {
                        left.push(Span::styled(
                            format!("{errs} {}  ", plural(errs, "error")),
                            err.add_modifier(Modifier::BOLD),
                        ));
                    }
                    if warns > 0 {
                        left.push(Span::styled(
                            format!("{warns} {}  ", plural(warns, "warning")),
                            warn,
                        ));
                    }
                    left.push(Span::raw(" "));
                }
            }
            None => left.push(Span::styled(
                format!(" {}   ", self.root.display()),
                style.add_modifier(Modifier::BOLD),
            )),
        }
        // Other files: those with errors, else those with warnings.
        let others = |sev: Severity| {
            self.problems
                .iter()
                .filter(|(p, ds)| {
                    current.is_none_or(|d| d.path != **p) && ds.iter().any(|d| d.severity == sev)
                })
                .count()
        };
        let (other_errs, other_warns) = (others(Severity::Error), others(Severity::Warning));
        if other_errs > 0 {
            left.push(Span::styled("|  ", muted));
            left.push(Span::styled(
                format!(
                    "{other_errs} other {} with errors",
                    plural(other_errs, "file")
                ),
                err,
            ));
        } else if other_warns > 0 {
            left.push(Span::styled("|  ", muted));
            left.push(Span::styled(
                format!(
                    "{other_warns} other {} with warnings",
                    plural(other_warns, "file")
                ),
                warn,
            ));
        }
        let lsp = self.lsp.status();
        let right = if lsp.is_empty() {
            String::new()
        } else {
            format!("{lsp} ")
        };
        let used: usize = left.iter().map(|s| s.content.chars().count()).sum();
        let pad = (area.width as usize).saturating_sub(used + right.chars().count());
        left.push(Span::raw(" ".repeat(pad)));
        left.push(Span::styled(right, muted));
        f.render_widget(Paragraph::new(Line::from(left)).style(style), area);
    }

    /// The bottom status line: what is happening. The message (or the problem
    /// under the caret) and the caret position.
    fn render_status(&self, f: &mut Frame, area: Rect) {
        let style = crate::ui::bar(&self.theme);
        let mut message = self.status.clone();
        let mut is_type = false;
        let mut right = String::new();
        if let Some(doc) = self.cur() {
            let c = doc.caret();
            if message.is_empty()
                && let Some(d) = doc
                    .diagnostics
                    .iter()
                    .filter(|d| d.range.start.line <= c.line && c.line <= d.range.end.line)
                    .min_by_key(|d| d.severity)
            {
                message = one_line(&d.message);
            }
            if message.is_empty()
                && let Some((p, pos, hint)) = &self.type_hint
                && *p == doc.path
                && *pos == c
            {
                message = hint.clone();
                is_type = true;
            }
            right = format!("{}:{} ", c.line + 1, c.col + 1);
        }
        let room = (area.width as usize).saturating_sub(right.chars().count() + 1);
        let message: String = format!(" {message}").chars().take(room).collect();
        let pad =
            (area.width as usize).saturating_sub(message.chars().count() + right.chars().count());
        // A type from hover is coloured like code; anything else is plain.
        let mut spans = if is_type {
            crate::sig::spans(&message, &self.theme, style)
        } else {
            vec![Span::raw(message)]
        };
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::raw(right));
        f.render_widget(Paragraph::new(Line::from(spans)).style(style), area);
    }

    fn render_welcome(&self, f: &mut Frame, area: Rect) {
        let muted = self.theme.style(ThemeRole::Muted);
        let home = dirs::home_dir().unwrap_or_default();
        let root = match self.root.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => self.root.display().to_string(),
        };
        let mut lines = vec![Line::from(format!("sanitty  {root}")), Line::from("")];
        for (a, label) in [
            (Action::Launcher, "Search everywhere"),
            (Action::RecentFiles, "Recent files"),
            (Action::ToggleExplorer, "Project tree"),
            (Action::AllKeys, "All keys"),
            (Action::Shell, "Shell"),
            (Action::Quit, "Quit"),
        ] {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>12}  ", self.display_key(a)), muted),
                Span::raw(format!("{label:<17}")),
            ]));
        }
        let h = lines.len() as u16;
        let rect = Rect {
            y: area.y + area.height.saturating_sub(h) / 3,
            height: h.min(area.height),
            ..area
        };
        f.render_widget(Paragraph::new(lines).centered(), rect);
    }

    /// One key per action, for hints and lists: the first binding that this
    /// terminal can actually deliver. Quit shows Ctrl+C, the terminal habit.
    fn display_key(&self, action: Action) -> String {
        if action == Action::Quit
            && self
                .keymap
                .chords_for(Action::Copy)
                .iter()
                .any(|c| c.to_string() == "Ctrl+C")
        {
            return "Ctrl+C".into();
        }
        // Recent Files preselects the previous file, so Enter switches to it.
        if action == Action::PreviousFile && self.keymap.chords_for(action).is_empty() {
            return format!("{} Enter", self.display_key(Action::RecentFiles));
        }
        let chords = self.keymap.chords_for(action);
        if let Some(c) = chords
            .iter()
            .find(|c| self.enhanced_keys || c.survives_legacy())
        {
            return c.to_string();
        }
        // No key this terminal can send: show the way in through the launcher.
        let launcher = self
            .keymap
            .chords_for(Action::Launcher)
            .into_iter()
            .find(|c| self.enhanced_keys || c.survives_legacy());
        match (launcher, action) {
            (None, _) | (_, Action::Launcher) => {
                chords.first().map(ToString::to_string).unwrap_or_default()
            }
            (Some(l), Action::GotoSymbol) => format!("{l} @"),
            (Some(l), Action::SearchInFiles) => format!("{l} /"),
            (Some(l), Action::GotoLine) => format!("{l} :"),
            (Some(l), _) => format!("{l} >"),
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

/// Problem messages at the end of their line: errors everywhere, and whatever
/// is on the caret's line, so you can read what an underline means.
fn problem_texts(doc: &Doc) -> Vec<VirtualText> {
    let caret_line = doc.caret().line;
    let mut best: std::collections::BTreeMap<u32, &karet_core::Diagnostic> =
        std::collections::BTreeMap::new();
    for d in &doc.diagnostics {
        let line = d.range.start.line;
        if d.severity != Severity::Error && line != caret_line {
            continue;
        }
        // The most severe diagnostic on a line wins (Error sorts first).
        if best.get(&line).is_none_or(|b| d.severity < b.severity) {
            best.insert(line, d);
        }
    }
    best.into_iter()
        .map(|(line, d)| {
            let role = match d.severity {
                Severity::Error => ThemeRole::DiagnosticError,
                Severity::Warning => ThemeRole::DiagnosticWarning,
                Severity::Information => ThemeRole::DiagnosticInfo,
                _ => ThemeRole::DiagnosticHint,
            };
            VirtualText::end_of_line(line, format!("   {}", one_line(&d.message)), role, 0)
        })
        .collect()
}

/// One line for the bottom bar from hover markdown: the first line of the first
/// code block (else the first line), with the name prefixed when it is only a type.
fn hover_summary(text: &str, word: Option<&str>) -> String {
    let mut in_code = false;
    let mut code_line = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code && !line.trim().is_empty() {
            code_line = Some(line.trim());
            break;
        }
    }
    let line = code_line
        .or_else(|| text.lines().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("");
    match word {
        Some(w) if !line.contains(w) => format!("{w}: {line}"),
        _ => line.to_string(),
    }
}

/// The quick-documentation popup, under the caret (or above it near the bottom).
fn render_doc(f: &mut Frame, area: Rect, caret: Option<(u16, u16)>, text: &str, theme: &Theme) {
    let base = crate::ui::surface(theme);
    let mut depth = 0usize;
    let max_w = 80.min(area.width.saturating_sub(2)) as usize;
    let inner_w = max_w.saturating_sub(4).max(10);
    // Markdown, lightly: fenced code in the code colour, prose wrapped, rules dropped.
    let mut lines: Vec<Line> = Vec::new();
    // Plain-text hovers (ty sends these) have no fences: the signature is
    // everything up to the first divider or blank line.
    let fenced = text.contains("```");
    // Code that arrives without any indentation (ty) gets indented by bracket
    // depth; code that is indented already (TypeScript) is left alone.
    let flat_code = {
        let mut inside = !fenced;
        let mut indented = false;
        for l in text.lines() {
            if l.trim_start().starts_with("```") {
                inside = !inside;
            } else if !fenced && l.trim().is_empty() {
                break;
            } else if inside && l.starts_with([' ', '\t']) {
                indented = true;
            }
        }
        !indented
    };
    let mut in_code = !fenced;
    for raw in text.lines() {
        if !fenced && in_code && raw.trim().is_empty() {
            in_code = false;
        }
        if raw.trim_start().starts_with("```") {
            in_code = !in_code;
            // A gap between a code block and the prose after it.
            if !in_code {
                lines.push(Line::from(""));
            }
            continue;
        }
        // A rule (`---`, or a row of dashes ty puts under the signature).
        let t = raw.trim();
        if t.len() >= 3 && t.chars().all(|c| c == '-') {
            if !fenced {
                in_code = false;
            }
            lines.push(Line::from(""));
            continue;
        }
        if in_code {
            // ty puts each parameter on its own line without indenting it;
            // indent by bracket depth so the signature reads as one. Fenced
            // code (Markdown hovers) comes indented already.
            let opens_first = raw.trim_start().starts_with([')', ']', '}']);
            let indent = "    ".repeat(depth.saturating_sub(usize::from(opens_first)));
            let line = if flat_code {
                format!("{indent}{}", raw.trim_start())
            } else {
                raw.to_string()
            };
            for c in raw.chars() {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            let line: String = line.chars().take(inner_w).collect();
            lines.push(Line::from(crate::sig::spans(&line, theme, base)));
            continue;
        }
        // Prose: inline Markdown, wrapped by words.
        let style_of = |s: crate::markdown::Style| match s {
            crate::markdown::Style::Plain => base,
            crate::markdown::Style::Bold => base.add_modifier(Modifier::BOLD),
            crate::markdown::Style::Italic => base.add_modifier(Modifier::ITALIC),
            crate::markdown::Style::Code => theme.token_style(karet_core::TokenId::FUNCTION),
            crate::markdown::Style::Link => base.add_modifier(Modifier::UNDERLINED),
        };
        let mut current: Vec<Span> = Vec::new();
        let mut width = 0usize;
        for (word, style, space) in crate::markdown::words(raw) {
            let n = word.chars().count();
            if width > 0 && width + n + usize::from(space) > inner_w {
                lines.push(Line::from(std::mem::take(&mut current)));
                width = 0;
            }
            if space && width > 0 {
                current.push(Span::raw(" "));
                width += 1;
            }
            width += n;
            current.push(Span::styled(word, style_of(style)));
        }
        lines.push(Line::from(current));
    }
    // No double gaps (a blank line after a fence), none at the end.
    lines.dedup_by(|a, b| a.width() == 0 && b.width() == 0);
    while lines.last().is_some_and(|l| l.width() == 0) {
        lines.pop();
    }
    let w = (lines.iter().map(Line::width).max().unwrap_or(10) + 4).min(max_w) as u16;
    let h = (lines.len() as u16 + 2)
        .min(area.height.saturating_sub(1))
        .max(3);
    let (cx, cy) = caret.unwrap_or((area.x + 4, area.y));
    let below = cy + 1 + h <= area.y + area.height;
    let y = if below {
        cy + 1
    } else {
        cy.saturating_sub(h).max(area.y)
    };
    let x = cx.min(area.x + area.width.saturating_sub(w));
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .padding(ratatui::widgets::Padding::horizontal(1))
        .style(base);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// The document on screen, borrowing only the fields it needs, so it can be
/// used alongside other parts of `App` (the find bar, the status line).
fn visible(docs: &mut [Doc], active: usize, home: bool) -> Option<&mut Doc> {
    if home { None } else { docs.get_mut(active) }
}

/// Severities that count as problems (hints are too quiet to step through).
fn counts(severity: Severity) -> bool {
    severity <= Severity::Information
}

fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        _ => "info",
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        word.to_string()
    } else {
        format!("{word}s")
    }
}

/// A multi-line diagnostic on one line: "mismatched types: expected u32, found &str".
fn one_line(message: &str) -> String {
    let mut parts = message.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = parts.next().unwrap_or("").to_string();
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() {
        first
    } else {
        format!("{first}: {}", rest.join("; "))
    }
}

/// The find bar: query (and replace) fields with the match count.
fn render_find(f: &mut Frame, area: Rect, find: &Find, theme: &Theme) {
    let base = crate::ui::bar(theme);
    let muted = theme.style(ThemeRole::Muted);
    let count = if find.invalid {
        "invalid regex".to_string()
    } else if find.query.is_empty() {
        String::new()
    } else if find.matches.is_empty() {
        "no matches".to_string()
    } else {
        format!(
            "{} of {}",
            find.current.map_or(0, |i| i + 1),
            find.matches.len()
        )
    };
    let flags: String = [(find.case, "Aa"), (find.word, "W"), (find.regex, ".*")]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, f)| format!(" [{f}]"))
        .collect();
    let find_line = Line::from(vec![
        Span::raw(" Find:    "),
        Span::raw(find.query.clone()),
        Span::styled(format!("   {count}{flags}"), muted),
    ]);
    f.render_widget(
        Paragraph::new(find_line).style(base),
        Rect { height: 1, ..area },
    );
    let (cursor_row, cursor_col) = if let Some(r) = &find.replace {
        let row = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" Replace: "),
                Span::raw(r.clone()),
            ]))
            .style(base),
            row,
        );
        if find.in_replace {
            (row.y, r.chars().count())
        } else {
            (area.y, find.query.chars().count())
        }
    } else {
        (area.y, find.query.chars().count())
    };
    f.set_cursor_position((area.x + 10 + cursor_col as u16, cursor_row));
}

fn contains(r: Range, p: LineCol) -> bool {
    (r.start.line, r.start.col) <= (p.line, p.col) && (p.line, p.col) <= (r.end.line, r.end.col)
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn parent_dir(rel: &Path) -> String {
    rel.parent()
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}
