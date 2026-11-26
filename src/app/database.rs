//! The Database view in the app: switching to it and back, handing it keys,
//! its query editor (an open file the app edits like any other), and
//! running SQL off the app loop. Also Run SQL from a file in the code view,
//! whose result shows in a popup.

use std::path::PathBuf;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;

use super::App;
use super::Overlay;
use super::Screen;
use crate::Event;
use crate::database::Database;
use crate::database::Focus;
use crate::database::Outcome;
use crate::database::confirm::Answer;
use crate::database::conn;
use crate::database::driver;
use crate::database::grid::Grid;
use crate::doc::Doc;
use crate::keymap::Action;

/// What an action does in the Database view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InDb {
    Keep,
    /// A code feature that means nothing here: says so, stays.
    Unavailable,
    /// Goes back to the code, then runs.
    Leave,
}

impl App {
    /// Alt+8: open the Database view, or go back to the code.
    pub(super) fn toggle_database(&mut self) {
        if self.in_db() {
            self.leave_database();
            return;
        }
        self.close_overlay();
        self.completion = None;
        self.screen = Screen::Database;
        // Where the code was, to come back to.
        self.db_return = Some((self.cur().map(|doc| doc.path.clone()), self.home));
        let root = self.root.clone();
        let view = self.database.get_or_insert_with(|| Database::new(root));
        let outcome = view.open();
        self.db_outcome(outcome);
        self.show_scratch();
    }

    /// Back to the code: the query is saved, the file that was on screen
    /// comes back.
    pub(super) fn leave_database(&mut self) {
        if !self.in_db() {
            return;
        }
        self.screen = Screen::Code;
        self.save_scratch();
        if let Some((path, home)) = self.db_return.take() {
            match path.and_then(|path| self.docs.iter().position(|doc| doc.path == path)) {
                Some(idx) => {
                    self.active = idx;
                    self.home = home;
                }
                // No file was on screen (or it was closed since): the home
                // screen, not the query file.
                None => self.home = true,
            }
        }
    }

    /// What an action does in the Database view: editing and SQL help work
    /// there (in the query editor), going somewhere else leaves the view,
    /// and code features that mean nothing for SQL say so. Every action is
    /// listed, so a new one needs a decision.
    pub(super) fn in_db_view(action: Action) -> InDb {
        match action {
            Action::Database
            | Action::RunSql
            | Action::Quit
            | Action::AllKeys
            | Action::CheckKeys
            | Action::Shell
            | Action::Save
            | Action::Undo
            | Action::Redo
            | Action::Copy
            | Action::CopyLine
            | Action::Cut
            | Action::Paste
            | Action::SelectAll
            | Action::DuplicateLine
            | Action::DeleteLine
            | Action::AddCaretAbove
            | Action::AddCaretBelow
            | Action::AddNextOccurrence
            | Action::RemoveLastCaret
            | Action::SelectAllOccurrences
            | Action::CaretsAtLineEnds
            | Action::ToggleComment
            | Action::Find
            | Action::Replace
            | Action::FindNext
            | Action::FindPrev
            | Action::QuickDoc
            | Action::Complete
            | Action::None => InDb::Keep,
            Action::GotoDefinition
            | Action::GotoImplementation
            | Action::FindUsages
            | Action::Rename
            | Action::Reformat
            | Action::NextProblem
            | Action::PrevProblem
            | Action::CloseFile => InDb::Unavailable,
            Action::Launcher
            | Action::GotoFile
            | Action::GotoSymbol
            | Action::GotoLine
            | Action::SearchInFiles
            | Action::RecentFiles
            | Action::Review
            | Action::PreviousFile
            | Action::ToggleExplorer
            | Action::Problems
            | Action::NavBack
            | Action::NavForward => InDb::Leave,
        }
    }

    /// Keys while the Database view is on screen. True when it used the key;
    /// false lets it run as an action (one that leaves the view does so) or
    /// reach the query editor.
    pub(super) fn db_key(&mut self, key: &KeyEvent, action: Option<Action>) -> bool {
        if !self.in_db() || !matches!(self.overlay, Overlay::None) {
            return false;
        }
        let editing = self.on_scratch();
        let Some(view) = &mut self.database else {
            return false;
        };
        // The view's popups take every key.
        if view.has_popup() {
            if let Some(outcome) = view.handle_key(key) {
                self.db_outcome(outcome);
            }
            return true;
        }
        let editing = editing && view.focus == Focus::Query;
        if let Some(action) = action {
            if action == Action::RunSql {
                self.run_query();
                return true;
            }
            if editing {
                // Ctrl+C without a selection leaves, as everywhere.
                if action == Action::Copy && self.cur().and_then(Doc::selected_text).is_none() {
                    self.leave_database();
                    return true;
                }
                return false;
            }
            return match action {
                // Nothing is selected in the tree or the grid: Ctrl+C leaves,
                // as everywhere (`y` copies a value).
                Action::Copy => {
                    self.leave_database();
                    true
                }
                // Editing keys mean nothing outside the query editor.
                _ if Self::in_db_view(action) == InDb::Keep => !matches!(
                    action,
                    Action::Database
                        | Action::Quit
                        | Action::AllKeys
                        | Action::CheckKeys
                        | Action::Shell
                ),
                _ => false,
            };
        }
        match view.handle_key(key) {
            Some(outcome) => {
                self.db_outcome(outcome);
                true
            }
            None => !editing,
        }
    }

    pub(super) fn db_paste(&mut self, text: &str) -> bool {
        if !self.in_db() || !matches!(self.overlay, Overlay::None) {
            return false;
        }
        let on_scratch = self.on_scratch();
        let Some(view) = &mut self.database else {
            return false;
        };
        if view.focus == Focus::Query && !view.has_popup() && on_scratch {
            return false;
        }
        view.paste(text);
        true
    }

    /// Whether the file on screen is the query editor's.
    fn on_scratch(&self) -> bool {
        self.db_scratch.is_some() && self.cur().map(|doc| &doc.path) == self.db_scratch.as_ref()
    }

    /// Point the editor at the query file, when there is one.
    fn show_scratch(&mut self) {
        let Some(path) = self.db_scratch.clone() else {
            return;
        };
        if let Some(idx) = self.docs.iter().position(|doc| doc.path == path) {
            self.active = idx;
            self.home = false;
        }
    }

    /// Save the query file, if it changed.
    pub(super) fn save_scratch(&mut self) {
        let Some(path) = self.db_scratch.clone() else {
            return;
        };
        if let Some(doc) = self.docs.iter_mut().find(|doc| doc.path == path)
            && doc.buffer.is_dirty()
            && let Err(err) = doc.save()
        {
            self.status = format!("Saving the query failed: {err}");
        }
    }

    /// The query file is ready: open it (not as a recent file) and show it.
    fn open_scratch(&mut self, path: PathBuf) {
        self.save_scratch();
        if !self.docs.iter().any(|doc| doc.path == path) {
            match Doc::open(&path) {
                Ok(doc) => self.docs.push(doc),
                Err(err) => {
                    self.status = format!("Opening the query failed: {err}");
                    return;
                }
            }
        }
        self.db_scratch = Some(path);
        if self.in_db() {
            self.show_scratch();
        }
    }

    /// Alt+Enter: the selection, or the statement under the caret.
    fn statement_here(&self) -> Option<String> {
        let doc = self.cur()?;
        if let Some(text) = doc.selected_text().filter(|text| !text.trim().is_empty()) {
            return Some(text);
        }
        let text = doc.buffer.text();
        let caret = doc.buffer.line_col_to_byte(doc.caret()).ok()?.0;
        let engine = self
            .database
            .as_ref()
            .and_then(Database::engine)
            .unwrap_or(crate::database::conn::Engine::Postgres);
        let range = crate::database::sql::statement_at(engine, &text, caret)?;
        text.get(range).map(str::to_string)
    }

    /// Run SQL from the query editor (in the view) or from a file (in the
    /// code, with the result in a popup).
    pub(super) fn run_query(&mut self) {
        let in_view = self.in_db();
        if in_view && !self.on_scratch() {
            self.status = "No query editor yet: connect first".into();
            return;
        }
        let Some(sql) = self.statement_here() else {
            self.status = "No SQL under the caret".into();
            return;
        };
        if in_view {
            self.save_scratch();
        }
        let connected = self.database.as_ref().is_some_and(Database::is_connected);
        let Some(view) = self.database.as_mut().filter(|_| connected) else {
            self.status = format!(
                "No database connection: {} opens the Database view",
                self.display_key(Action::Database)
            );
            return;
        };
        let outcome = view.ask(sql, !in_view);
        if !in_view && let Some(confirm) = view.take_confirm() {
            self.overlay = Overlay::DbConfirm(confirm);
        }
        self.db_outcome(outcome);
    }

    /// Keys for the result and confirm popups over the code. True when one
    /// was open.
    pub(super) fn db_overlay_key(&mut self, key: &KeyEvent, action: Option<Action>) -> bool {
        match &mut self.overlay {
            Overlay::DbResult(grid) => {
                if grid.handle_key(key) {
                    return true;
                }
                match key.code {
                    KeyCode::Esc => self.overlay = Overlay::None,
                    KeyCode::Char('y') => {
                        if let Some(value) = grid.cell() {
                            let text = value.as_deref().unwrap_or("").to_string();
                            self.set_clipboard(text);
                            self.status = "Copied the value".into();
                        }
                    }
                    _ if action == Some(Action::Copy) => self.overlay = Overlay::None,
                    _ if action.is_some() => {
                        // Any other action closes the result and runs.
                        self.overlay = Overlay::None;
                        return false;
                    }
                    _ => {}
                }
                true
            }
            Overlay::DbConfirm(confirm) => {
                match confirm.handle_key(key) {
                    Answer::Run => {
                        let sql = confirm.sql.clone();
                        self.overlay = Overlay::None;
                        if let Some(view) = &mut self.database {
                            let outcome = view.confirmed(sql);
                            self.db_outcome(outcome);
                        }
                    }
                    Answer::Cancel => {
                        self.overlay = Overlay::None;
                        self.status = "Not run".into();
                    }
                    Answer::NotSeen => {
                        self.status = "Scroll down to see all of the SQL first".into();
                    }
                    Answer::Pending if action == Some(Action::Copy) => {
                        self.overlay = Overlay::None;
                        self.status = "Not run".into();
                    }
                    Answer::Pending => {}
                }
                true
            }
            _ => false,
        }
    }

    pub(super) fn db_event(&mut self, event: Event) {
        let Some(view) = &mut self.database else {
            return;
        };
        let outcome = match event {
            Event::DbFound(found, problems) => view.found(found, problems),
            Event::DbConnected(generation, result) => view.connected(generation, result),
            Event::DbSchema(generation, result) => view.schema_loaded(generation, result),
            Event::DbResult(generation, result) => view.finished(generation, result),
            _ => return,
        };
        self.db_outcome(outcome);
    }

    pub(super) fn db_outcome(&mut self, outcome: Outcome) {
        let tx = self.tx.clone();
        match outcome {
            Outcome::Pending => {}
            Outcome::Status(text) => self.status = text,
            Outcome::Copy(text, status) => {
                self.set_clipboard(text);
                self.status = status;
            }
            Outcome::Discover => {
                let Some(root) = self.database.as_ref().map(|view| view.root().to_path_buf())
                else {
                    return;
                };
                tokio::task::spawn_blocking(move || {
                    let (found, problems) = conn::discover(&root);
                    let _ = tx.send(Event::DbFound(found, problems));
                });
            }
            Outcome::Connect(conn, generation) => {
                tokio::spawn(async move {
                    let result = match driver::connect(&conn).await {
                        Ok(view) => driver::schema(&view).await.map(|schema| (view, schema)),
                        Err(err) => Err(err),
                    };
                    let _ = tx.send(Event::DbConnected(generation, result));
                });
            }
            Outcome::Refresh(generation) => {
                let Some(handle) = self.database.as_ref().and_then(Database::handle) else {
                    return;
                };
                tokio::spawn(async move {
                    let _ = tx.send(Event::DbSchema(generation, driver::schema(&handle).await));
                });
            }
            Outcome::ExecuteOneRow(sql, generation) => {
                let Some(handle) = self.database.as_ref().and_then(Database::handle) else {
                    return;
                };
                tokio::spawn(async move {
                    let result = driver::update_one_row(&handle, &sql).await;
                    let _ = tx.send(Event::DbResult(generation, result));
                });
            }
            Outcome::Execute(sql, generation) => {
                let Some(handle) = self.database.as_ref().and_then(Database::handle) else {
                    return;
                };
                tokio::spawn(async move {
                    let _ = tx.send(Event::DbResult(
                        generation,
                        driver::run(&handle, &sql).await,
                    ));
                });
            }
            Outcome::Insert(text) => self.insert_in_query(&text),
            Outcome::Opened(path) => self.open_scratch(path),
            Outcome::Popup(grid) => self.show_result(grid),
        }
    }

    /// Put a statement at the end of the query and focus it, the caret on it.
    fn insert_in_query(&mut self, text: &str) {
        if let Some(view) = &mut self.database {
            view.focus = Focus::Query;
        }
        self.show_scratch();
        if !self.on_scratch() {
            return;
        }
        if let Some(doc) = self.doc() {
            doc.state
                .move_carets(&doc.buffer, karet_editor::Motion::DocEnd, false);
            let ends_with_newline =
                doc.buffer.text().ends_with('\n') || doc.buffer.text().is_empty();
            let lead = if ends_with_newline { "" } else { "\n" };
            doc.type_text(&format!("{lead}{text}"));
        }
    }

    fn show_result(&mut self, grid: Grid) {
        self.status = grid.summary();
        self.overlay = Overlay::DbResult(grid);
    }

    /// Whether the file on screen is SQL: the query editor, or a `.sql` file.
    pub(super) fn on_sql(&self) -> bool {
        self.on_scratch()
            || self.cur().is_some_and(|doc| {
                doc.path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("sql"))
            })
    }

    /// Alt+J in SQL: tables, columns and keywords from the schema. False
    /// when the file is not SQL (its language server completes it).
    pub(super) fn sql_completion(&mut self) -> bool {
        if !self.on_sql() {
            return false;
        }
        let Some(doc) = self.cur() else { return false };
        let Some((schema, engine)) = self.database.as_ref().and_then(Database::schema_info) else {
            self.status = format!(
                "SQL completion comes from a database's schema: {} connects",
                self.display_key(Action::Database)
            );
            return true;
        };
        let text = doc.buffer.text();
        let Ok(caret) = doc.buffer.line_col_to_byte(doc.caret()) else {
            return true;
        };
        let items = crate::database::assist::completions(schema, engine, &text, caret.0);
        let anchor = super::completion::word_start(doc);
        let typed = super::completion::prefix(doc, anchor);
        let path = doc.path.clone();
        self.completion_gen += 1;
        let mut popup = crate::complete::Completion::new(self.completion_gen, path, None, anchor);
        popup.set_items(items, false, &typed);
        if popup.filtered.is_empty() {
            self.status = "No suggestions".into();
            self.completion = None;
        } else {
            self.completion = Some(popup);
        }
        true
    }

    /// Alt+H (and the status line's type) in SQL: what the name under the
    /// caret is in the schema. False when the file is not SQL.
    pub(super) fn sql_hover(&mut self, popup: bool) -> bool {
        if !self.on_sql() {
            return false;
        }
        let Some(doc) = self.cur() else { return false };
        let Some((schema, engine)) = self.database.as_ref().and_then(Database::schema_info) else {
            if popup {
                self.status = format!(
                    "SQL documentation comes from a database's schema: {} connects",
                    self.display_key(Action::Database)
                );
            }
            return true;
        };
        let text = doc.buffer.text();
        let Ok(caret) = doc.buffer.line_col_to_byte(doc.caret()) else {
            return true;
        };
        let found = crate::database::assist::describe(schema, engine, &text, caret.0);
        let here = (doc.path.clone(), doc.caret());
        self.hover_asked = Some(here.clone());
        match found {
            Some((markdown, _)) if popup => self.overlay = Overlay::Doc(markdown),
            Some((_, line)) => self.type_hint = Some((here.0, here.1, line)),
            None if popup => self.status = "Not a table or column of this database".into(),
            None => self.type_hint = None,
        }
        true
    }
}
