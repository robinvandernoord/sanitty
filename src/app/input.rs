//! Keys and pastes: routing them to the overlay, the find bar or the editor, and running actions.

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use karet_core::EditCause;
use karet_core::LineCol;
use karet_core::Range;
use karet_editor::Motion;

use crate::allkeys;
use crate::allkeys::AllKeys;
use crate::doc::Doc;
use crate::explorer;
use crate::find;
use crate::find::Find;
use crate::keycheck;
use crate::keycheck::KeyCheck;
use crate::keymap::Action;
use crate::lsp::Nav;
use crate::picker;
use crate::picker::Item;
use crate::picker::Kind;
use crate::picker::Picker;
use crate::picker::Target;

use super::App;
use super::Overlay;
use super::Request;
use super::database::InView;
use super::file_name;
use super::parent_dir;
use super::save_keycheck;
use super::visible;

impl App {
    pub(super) fn on_paste(&mut self, text: &str) {
        if self.terminal_paste(text) {
            return;
        }
        if self.db_paste(text) {
            return;
        }
        if self.in_review() && matches!(self.overlay, Overlay::None) {
            if let Some(review) = &mut self.review {
                review.paste(text);
            }
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        match &mut self.overlay {
            Overlay::Rename { text: name, .. } => name.push_str(text.lines().next().unwrap_or("")),
            Overlay::Find(find) => {
                if let Some(doc) = visible(&mut self.docs, self.active, self.home) {
                    find.paste(&text, doc);
                }
            }
            Overlay::None => {
                if let Some(doc) = self.doc() {
                    if doc.multi() {
                        doc.paste_each(&text);
                    } else {
                        doc.type_text(&text);
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn on_key(&mut self, key: KeyEvent) {
        // Overlays get the key first; an unhandled global action still works
        // from inside them (Alt+1 closes the tree, Alt+N swaps pickers).
        // The key check sees every key raw, Ctrl+C included.
        if self.key_check_key(&key) {
            return;
        }
        // A message lasts until the next key, so the problem under the caret
        // shows again; a pending "press again" lasts exactly one key.
        self.status.clear();
        self.armed = self.confirm.take();
        let action = self.keymap.lookup(&key);
        if self.terminal_key(&key, action) {
            return;
        }
        if self.review_key(&key, action) {
            return;
        }
        if self.db_key(&key, action) {
            return;
        }
        if self.db_overlay_key(&key, action) {
            return;
        }
        if self.completion_key(&key, action) {
            return;
        }
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
        if self.overlay_key(&key, action) {
            return;
        }
        if let Some(action) = action {
            self.run(action);
            return;
        }
        self.edit_key(key);
    }

    /// The key check overlay, which sees every key raw. True if it was open.
    fn key_check_key(&mut self, key: &KeyEvent) -> bool {
        let Overlay::KeyCheck(check) = &mut self.overlay else {
            return false;
        };
        match check.handle_key(key) {
            keycheck::Outcome::Pending => {}
            keycheck::Outcome::Close => self.overlay = Overlay::None,
            keycheck::Outcome::Progress(report) => save_keycheck(check, &report),
            keycheck::Outcome::Finished(report) => {
                save_keycheck(check, &report);
                self.set_clipboard(report);
            }
        }
        true
    }

    /// Give the key to the open overlay. True if the key was used up; false
    /// if it should still run as a global action or reach the editor.
    fn overlay_key(&mut self, key: &KeyEvent, action: Option<Action>) -> bool {
        match self.overlay {
            Overlay::None => false,
            Overlay::Explorer(_) => {
                self.explorer_overlay_key(key, action);
                true
            }
            Overlay::Picker(_) => {
                self.picker_overlay_key(key, action);
                true
            }
            Overlay::Rename { .. } => {
                self.rename_overlay_key(key);
                true
            }
            Overlay::AllKeys(_) => {
                self.all_keys_overlay_key(key);
                true
            }
            Overlay::Find(_) => {
                self.find_overlay_key(key, action);
                true
            }
            Overlay::KeyCheck(_) | Overlay::DbResult(_) | Overlay::DbConfirm(_) => true,
            // Any key closes the documentation popup; only Esc is used up by it.
            Overlay::Doc(_) => {
                self.overlay = Overlay::None;
                key.code == KeyCode::Esc
            }
        }
    }

    fn explorer_overlay_key(&mut self, key: &KeyEvent, action: Option<Action>) {
        if matches!(action, Some(Action::ToggleExplorer)) {
            self.close_overlay();
            return;
        }
        // Keys that open another view or search still work over the tree
        // (it is also the home screen).
        if let Some(action) = action.filter(|action| leaves_overlay(*action)) {
            self.close_overlay();
            self.run(action);
            return;
        }
        let Overlay::Explorer(explorer) = &mut self.overlay else {
            return;
        };
        match explorer.handle_key(key) {
            explorer::Outcome::Pending => {}
            explorer::Outcome::Close => self.close_overlay(),
            explorer::Outcome::Open(path) => {
                self.close_overlay();
                self.open(&path, None);
            }
        }
    }

    fn picker_overlay_key(&mut self, key: &KeyEvent, action: Option<Action>) {
        if let Some(action) = action.filter(|action| leaves_overlay(*action)) {
            self.overlay = Overlay::None;
            self.run(action);
            return;
        }
        let Overlay::Picker(picker) = &mut self.overlay else {
            return;
        };
        match picker.handle_key(key) {
            picker::Outcome::Pending => {
                if picker.take_changed() {
                    self.launcher_search();
                }
            }
            picker::Outcome::Cancel => self.overlay = Overlay::None,
            picker::Outcome::Pick(target) => {
                self.overlay = Overlay::None;
                self.pick(target);
            }
        }
    }

    fn rename_overlay_key(&mut self, key: &KeyEvent) {
        let Overlay::Rename { path, pos, text } = &mut self.overlay else {
            return;
        };
        match key.code {
            KeyCode::Esc => self.overlay = Overlay::None,
            KeyCode::Enter => {
                let (path, pos, name) = (path.clone(), *pos, text.clone());
                self.overlay = Overlay::None;
                self.request_rename(path, pos, name);
            }
            KeyCode::Backspace => crate::picker::erase(text, key),
            KeyCode::Char(chr)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                text.push(chr);
            }
            _ => {}
        }
    }

    fn all_keys_overlay_key(&mut self, key: &KeyEvent) {
        let Overlay::AllKeys(keys) = &mut self.overlay else {
            return;
        };
        match keys.handle_key(key) {
            allkeys::Outcome::Pending => {}
            allkeys::Outcome::Close => self.overlay = Overlay::None,
            allkeys::Outcome::Run(action) => {
                self.overlay = Overlay::None;
                self.run(action);
            }
        }
    }

    fn find_overlay_key(&mut self, key: &KeyEvent, action: Option<Action>) {
        let Overlay::Find(find) = &mut self.overlay else {
            return;
        };
        let Some(doc) = visible(&mut self.docs, self.active, self.home) else {
            self.overlay = Overlay::None;
            return;
        };
        match find.handle_key(key, doc, &mut self.status) {
            find::Outcome::Handled => {}
            find::Outcome::Close => self.close_overlay(),
            // Not a find key: global actions still work, typing does not.
            // One that goes to another file or view closes the bar first:
            // its matches belong to this file.
            find::Outcome::Ignored => {
                if let Some(action) = action {
                    if leaves_overlay(action) {
                        self.close_overlay();
                    }
                    self.run(action);
                }
            }
        }
    }

    pub(super) fn edit_key(&mut self, key: KeyEvent) {
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
        if let Some(motion) = motion {
            doc.state.move_carets(&doc.buffer, motion, shift);
            return;
        }
        match key.code {
            KeyCode::Esc => doc.state.collapse_to_primary(),
            KeyCode::Enter => doc.newline(),
            KeyCode::Tab => doc.type_text(&" ".repeat(crate::settings::get().editor.tab_size)),
            // Ctrl or Alt: the word before the caret.
            KeyCode::Backspace => doc.backspace(ctrl || alt),
            KeyCode::Delete => doc.delete(ctrl),
            KeyCode::Char(chr) if !ctrl && !alt => {
                let mut utf8 = [0u8; 4];
                doc.type_text(chr.encode_utf8(&mut utf8));
            }
            _ => {}
        }
    }

    pub(super) fn run(&mut self, action: Action) {
        // In the Database view, editing and SQL help work, going elsewhere
        // leaves it, and code-only features say they do not apply.
        if self.in_db() {
            match Self::in_db_view(action) {
                InView::Keep => {}
                InView::Leave => self.leave_database(),
                InView::Unavailable => {
                    self.status = format!("{} does not work in the Database view", action.label());
                    return;
                }
            }
        }
        match action {
            Action::None => {}
            Action::Quit => self.quit_or_warn(),
            Action::Save => self.save(),
            Action::Launcher => self.open_launcher(Kind::All, ""),
            Action::GotoFile => self.open_launcher(Kind::File, ""),
            Action::GotoSymbol => self.open_launcher(Kind::Symbol, ""),
            Action::GotoLine => self.open_launcher(Kind::All, ":"),
            Action::SearchInFiles => self.search_in_files(),
            Action::AllKeys => {
                let keys = AllKeys::new(|key_action| self.display_key(key_action));
                self.overlay = Overlay::AllKeys(keys);
            }
            Action::CheckKeys => self.overlay = Overlay::KeyCheck(KeyCheck::new()),
            Action::Find | Action::Replace => self.open_find_bar(action == Action::Replace),
            Action::FindNext | Action::FindPrev => self.find_step(action == Action::FindNext),
            Action::RecentFiles => self.show_recent_files(),
            Action::Review => {
                self.leave_database();
                self.toggle_review();
            }
            Action::Database => self.toggle_database(),
            Action::RunSql => self.run_query(),
            Action::ToggleExplorer => self.open_tree(),
            Action::GotoDefinition => self.request_nav(Nav::Definition),
            Action::QuickDoc => self.request_hover(true),
            Action::Problems => self.open_launcher(Kind::Problem, ""),
            Action::NextProblem | Action::PrevProblem => {
                self.step_problem(action == Action::NextProblem);
            }
            Action::GotoImplementation => self.request_nav(Nav::Implementation),
            Action::FindUsages => self.request_nav(Nav::Usages),
            Action::Rename => self.start_rename_overlay(),
            Action::DuplicateLine => self.with_doc(Doc::duplicate_line),
            Action::DeleteLine => self.with_doc(Doc::delete_line),
            Action::AddCaretAbove
            | Action::AddCaretBelow
            | Action::AddNextOccurrence
            | Action::RemoveLastCaret
            | Action::SelectAllOccurrences
            | Action::CaretsAtLineEnds => self.caret_action(action),
            Action::ToggleComment => self.with_doc(Doc::toggle_comment),
            Action::Reformat => self.reformat(),
            Action::Complete => self.start_completion(),
            Action::Undo => self.with_doc(Doc::undo),
            Action::Redo => self.with_doc(Doc::redo),
            Action::SelectAll => self.with_doc(|doc| doc.state.select_all(&doc.buffer)),
            Action::Copy => self.copy_or_leave(),
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
            Action::Paste => self.paste_clipboard(),
            Action::CloseFile => self.close_active_file(),
            Action::PreviousFile => self.open_previous_file(),
            Action::NavBack => {
                if let Some((path, pos)) = self.back.pop() {
                    if let Some(here) = self.here() {
                        self.forward.push(here);
                    }
                    self.open(&path, Some(pos));
                }
            }
            Action::NavForward => {
                if let Some((path, pos)) = self.forward.pop() {
                    if let Some(here) = self.here() {
                        self.back.push(here);
                    }
                    self.open(&path, Some(pos));
                }
            }
            Action::Shell => self.toggle_terminal(),
        }
    }

    /// Adding and removing carets (multi-caret editing).
    fn caret_action(&mut self, action: Action) {
        match action {
            Action::AddCaretAbove => self.with_doc(|doc| doc.state.add_caret_above(&doc.buffer)),
            Action::AddCaretBelow => self.with_doc(|doc| doc.state.add_caret_below(&doc.buffer)),
            Action::AddNextOccurrence => {
                self.with_doc(|doc| doc.state.add_next_occurrence(&doc.buffer));
            }
            Action::RemoveLastCaret => {
                let undone = self
                    .doc()
                    .is_some_and(|doc| doc.state.undo_caret_operation(&doc.buffer));
                if !undone && self.cur().is_some() {
                    self.status = "No caret to remove".into();
                }
            }
            Action::SelectAllOccurrences => {
                self.with_doc(|doc| doc.state.select_all_occurrences(&doc.buffer));
            }
            Action::CaretsAtLineEnds => {
                self.with_doc(|doc| doc.state.add_cursors_to_line_ends(&doc.buffer));
            }
            _ => {}
        }
    }

    fn quit_or_warn(&mut self) {
        // The Database view's query is saved, not warned about.
        self.save_scratch();
        let dirty: Vec<String> = self
            .docs
            .iter()
            .filter(|doc| doc.buffer.is_dirty())
            .map(|doc| self.rel(&doc.path).display().to_string())
            .collect();
        // A program still running in the Terminal view would be stopped.
        let running = self.shell.as_ref().and_then(crate::shell::Shell::running);
        if (dirty.is_empty() && running.is_none()) || self.armed == Some(Action::Quit) {
            self.request = Some(Request::Quit);
            return;
        }
        let mut parts = Vec::new();
        if !dirty.is_empty() {
            parts.push(format!("Unsaved: {}", dirty.join(", ")));
        }
        if let Some(program) = &running {
            parts.push(format!("Still running in the terminal: {program}"));
        }
        let what = match (dirty.is_empty(), running.is_none()) {
            (false, true) => "discard",
            (true, false) => "stop it",
            _ => "discard and stop it",
        };
        self.status = format!("{}. Press quit again to {what}.", parts.join(". "));
        self.confirm = Some(Action::Quit);
    }

    fn search_in_files(&mut self) {
        let seed = self
            .docs
            .get(self.active)
            .and_then(Doc::selected_text)
            .filter(|text| !text.contains('\n'))
            .unwrap_or_default();
        self.open_launcher(Kind::Text, &seed);
    }

    fn open_find_bar(&mut self, replace: bool) {
        if let Overlay::Find(find) = &mut self.overlay {
            // Already open: Alt+R adds the replace field, Ctrl+F refocuses the query.
            if replace && find.replace.is_none() {
                find.replace = Some(String::new());
            }
            find.in_replace = replace;
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

    fn find_step(&mut self, forward: bool) {
        let Some(doc) = visible(&mut self.docs, self.active, self.home) else {
            return;
        };
        match &mut self.overlay {
            Overlay::Find(find) => find.step(doc, forward),
            _ => {
                if let Some(find) = &mut self.last_find
                    && !find.jump_from_caret(doc, forward)
                {
                    self.status = format!("\"{}\" not found", find.query);
                }
            }
        }
    }

    fn show_recent_files(&mut self) {
        let items = self
            .recent
            .iter()
            .map(|path| Item {
                label: file_name(path),
                detail: parent_dir(self.rel(path)),
                target: Target::File(path.clone()),
                kind: Kind::File,
                keywords: "",
            })
            .collect();
        let mut picker = Picker::new("Recent Files", items, true);
        // The current file is first; preselect the one before it. On
        // the home screen nothing is current, so the latest is first.
        picker.select(usize::from(!self.home));
        self.overlay = Overlay::Picker(picker);
    }

    fn start_rename_overlay(&mut self) {
        if let Some(doc) = self.cur() {
            let text = doc.word_at_caret().unwrap_or_default();
            self.overlay = Overlay::Rename {
                path: doc.path.clone(),
                pos: doc.caret_utf16(),
                text,
            };
        }
    }

    /// Ctrl+C: copy a selection; else leave the file for the home screen; on
    /// the home screen, quit, as a terminal program should. Copy Line covers
    /// `PyCharm`'s line copy.
    fn copy_or_leave(&mut self) {
        match self.cur().and_then(Doc::selected_text) {
            Some(text) => {
                self.set_clipboard(text);
            }
            None if self.cur().is_some() => {
                // Home, with the tree open on the file just left.
                self.open_tree();
                self.home = true;
                self.status = format!(
                    "{} again quits; {} brings your files back",
                    self.display_key(Action::Quit),
                    self.display_key(Action::RecentFiles)
                );
            }
            None => self.run(Action::Quit),
        }
    }

    fn paste_clipboard(&mut self) {
        let text = self.clipboard.clone();
        let Some(doc) = self.doc() else { return };
        if doc.multi() {
            doc.paste_each(&text);
            return;
        }
        let caret = doc.caret();
        // A whole-line copy pastes above the current line, as in PyCharm.
        if text.ends_with('\n') && doc.state.selection_range().is_none_or(Range::is_empty) {
            doc.state.place_caret(LineCol::new(caret.line, 0));
            doc.type_text(&text);
            let lines = u32::try_from(text.matches('\n').count()).unwrap_or(u32::MAX);
            let below = LineCol::new(caret.line.saturating_add(lines), caret.col);
            doc.state.place_caret(doc.buffer.clamp(below));
        } else {
            let base = doc.buffer.version();
            let edit =
                karet_editor::editing::insert(caret, doc.state.selection_range(), base, &text);
            doc.apply(edit, EditCause::Paste);
        }
    }

    fn close_active_file(&mut self) {
        if self.home {
            return;
        }
        let Some(doc) = self.docs.get(self.active) else {
            return;
        };
        if doc.buffer.is_dirty() && self.armed != Some(Action::CloseFile) {
            self.status = "Unsaved changes: close again to discard".into();
            self.confirm = Some(Action::CloseFile);
            return;
        }
        let closed = self.docs.remove(self.active);
        // The servers forget it too.
        if let Some(lang) = closed.lsp_lang {
            for (_, server) in self.lsp.running(lang) {
                server.did_close(closed.path.clone());
            }
        }
        self.closed_versions
            .insert(closed.path.clone(), closed.lsp_version);
        let closed = closed.path;
        self.status.clear();
        // Fall back to the most recently used file that is still open; with
        // none left, the home screen (the Database view's query file stays
        // open, but is not one of yours).
        if let Some(idx) = self
            .recent
            .iter()
            .filter(|path| **path != closed)
            .find_map(|path| self.docs.iter().position(|doc| doc.path == *path))
        {
            self.active = idx;
        } else {
            self.active = 0;
            self.home = !self.docs.is_empty();
        }
    }

    fn open_previous_file(&mut self) {
        let current = self.cur().map(|doc| doc.path.clone());
        match self
            .recent
            .iter()
            .find(|path| Some(*path) != current.as_ref())
            .cloned()
        {
            Some(path) => self.open(&path, None),
            None => self.status = "No previous file".into(),
        }
    }
}

/// Actions that close a popup (picker, tree) and run instead of going to it.
fn leaves_overlay(action: Action) -> bool {
    matches!(
        action,
        Action::Launcher
            | Action::GotoFile
            | Action::GotoSymbol
            | Action::GotoLine
            | Action::SearchInFiles
            | Action::RecentFiles
            | Action::Review
            | Action::Database
            | Action::Shell
            | Action::AllKeys
            | Action::Quit
    )
}
