//! The Database view (Alt+8): connections found in docker compose and
//! `.sanitty/db.toml`, their schema, a query editor and a result grid.
//!
//! Finding connections, connecting and running SQL happen off the app loop
//! (see [`Outcome`]); this module keeps the view's state, handles its keys
//! and draws it. The query editor is an ordinary open file (a scratch
//! `.sql` file per connection, outside the project), which the app edits
//! and draws in the rectangle [`Database::render`] gives back.

pub mod assist;
pub mod confirm;
pub mod conn;
pub mod driver;
pub mod grid;
pub mod sql;

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;
use std::path::PathBuf;

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
use ratatui::widgets::Wrap;

use conn::Connection;
use driver::Origin;
use driver::Pool;
use driver::ResultSet;
use driver::Schema;
use grid::Grid;

/// What the app should do after a key or an answer.
pub enum Outcome {
    Pending,
    Status(String),
    /// Put this on the clipboard, then show the message.
    Copy(String, String),
    /// Find the connections (off the loop), then [`Database::found`].
    Discover,
    /// Connect (off the loop), then [`Database::connected`] with this
    /// generation.
    Connect(Connection, u64),
    /// Read the schema again (off the loop), then [`Database::schema_loaded`].
    Refresh(u64),
    /// Run SQL (off the loop), then [`Database::finished`] with this
    /// generation.
    Execute(String, u64),
    /// A cell edit's UPDATE (off the loop), in a transaction that is undone
    /// unless it changes exactly one row; then [`Database::finished`].
    ExecuteOneRow(String, u64),
    /// Put this text in the query editor and focus it.
    Insert(String),
    /// The query editor for this connection is ready: open its file.
    Opened(PathBuf),
    /// Show this result over the code (it was run from a `.sql` file).
    Popup(Grid),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Tree,
    Query,
    Grid,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Self::Tree => Self::Query,
            Self::Query => Self::Grid,
            Self::Grid => Self::Tree,
        }
    }
}

enum Phase {
    Finding,
    Connecting,
    Ready,
    /// Nothing to connect to, or connecting failed; the message says which.
    Idle(String),
}

enum Popup {
    Connections {
        selected: usize,
    },
    /// SQL that writes, shown before it runs.
    Confirm {
        confirm: confirm::Confirm,
        /// A cell edit: one row or nothing, and the grid's SQL again after.
        rerun: bool,
        /// Show the result over the code.
        to_popup: bool,
    },
    /// A new value for the cell under the grid's cursor.
    Edit {
        text: String,
        null: bool,
    },
}

/// Where a cell edit goes.
struct EditTarget {
    /// Quoted, with its schema.
    table: String,
    /// The table's own name for the column (a result may rename it).
    column: String,
    /// The row's primary key: (column, value).
    key: Vec<(String, Option<String>)>,
}

/// A row of the schema tree.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    Schema(String),
    Table(usize),
    Column(usize, usize),
}

#[derive(Default)]
struct Tree {
    /// Open schemas and tables, by name ("schema" and "schema.table").
    open: HashSet<String>,
    selected: usize,
    top: usize,
    page: usize,
    /// Typing after `/` narrows the tables; `Some` while it is shown.
    filter: Option<String>,
    filtering: bool,
}

/// What a run in flight is for.
#[derive(Clone)]
struct Pending {
    sql: String,
    rerun: bool,
    to_popup: bool,
}

pub struct Database {
    root: PathBuf,
    connections: Vec<Connection>,
    problems: Vec<String>,
    phase: Phase,
    current: Option<usize>,
    pool: Option<Pool>,
    schema: Option<Schema>,
    /// Bumped for every connect and run; an older answer is dropped.
    generation: u64,
    pending: Option<Pending>,
    tree: Tree,
    pub focus: Focus,
    grid: Option<Grid>,
    error: Option<String>,
    popup: Option<Popup>,
}

impl Database {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            connections: Vec::new(),
            problems: Vec::new(),
            phase: Phase::Finding,
            current: None,
            pool: None,
            schema: None,
            generation: 0,
            pending: None,
            tree: Tree::default(),
            focus: Focus::Tree,
            grid: None,
            error: None,
            popup: None,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The view opened: find connections the first time.
    pub fn open(&mut self) -> Outcome {
        match self.phase {
            Phase::Finding if self.connections.is_empty() => Outcome::Discover,
            _ => Outcome::Pending,
        }
    }

    pub fn is_connected(&self) -> bool {
        self.pool.is_some()
    }

    /// The query editor's file for the current connection.
    pub fn scratch(&self) -> Option<PathBuf> {
        let conn = self.connections.get(self.current?)?;
        scratch_file(&self.root, &conn.name)
    }

    /// Connections were found: connect to the one used last, or the only
    /// one; else let the user pick.
    pub fn found(&mut self, connections: Vec<Connection>, problems: Vec<String>) -> Outcome {
        self.connections = connections;
        self.problems = problems;
        if self.connections.is_empty() {
            let mut text = "No databases found in docker compose or .sanitty/db.toml".to_string();
            if let Some(problem) = self.problems.first() {
                text = format!("{text} ({problem})");
            }
            self.phase = Phase::Idle(text);
            return Outcome::Pending;
        }
        let last = last_connection(&self.root);
        let pick = self
            .connections
            .iter()
            .position(|conn| Some(&conn.name) == last.as_ref())
            .or_else(|| (self.connections.len() == 1).then_some(0));
        if let Some(idx) = pick {
            self.connect(idx)
        } else {
            self.phase = Phase::Idle("Pick a connection".into());
            self.popup = Some(Popup::Connections { selected: 0 });
            Outcome::Pending
        }
    }

    fn connect(&mut self, idx: usize) -> Outcome {
        let Some(conn) = self.connections.get(idx).cloned() else {
            return Outcome::Pending;
        };
        self.generation += 1;
        self.current = Some(idx);
        self.pool = None;
        self.schema = None;
        self.grid = None;
        self.error = None;
        self.pending = None;
        self.tree = Tree::default();
        self.phase = Phase::Connecting;
        save_last(&self.root, &conn.name);
        Outcome::Connect(conn, self.generation)
    }

    /// Connecting finished; the schema comes with it.
    pub fn connected(
        &mut self,
        generation: u64,
        result: Result<(Pool, Schema), String>,
    ) -> Outcome {
        if generation != self.generation {
            return Outcome::Pending;
        }
        match result {
            Ok((pool, schema)) => {
                self.pool = Some(pool);
                self.set_schema(schema);
                self.phase = Phase::Ready;
                match self.scratch() {
                    Some(path) => Outcome::Opened(path),
                    None => Outcome::Status("No state directory for the query editor".into()),
                }
            }
            Err(err) => {
                self.phase = Phase::Idle(format!("{err}  (c picks another connection)"));
                Outcome::Pending
            }
        }
    }

    pub fn schema_loaded(&mut self, generation: u64, result: Result<Schema, String>) -> Outcome {
        if generation != self.generation {
            return Outcome::Pending;
        }
        match result {
            Ok(schema) => {
                self.set_schema(schema);
                Outcome::Pending
            }
            Err(err) => Outcome::Status(format!("Reading the schema failed: {err}")),
        }
    }

    fn set_schema(&mut self, schema: Schema) {
        // The only schema, or the usual default, starts open.
        let names = schema_names(&schema);
        if self.tree.open.is_empty() {
            let conn = self.current.and_then(|idx| self.connections.get(idx));
            for name in &names {
                let usual = name == "public"
                    || name == "main"
                    || conn.is_some_and(|conn| conn.database == *name);
                if names.len() == 1 || usual {
                    self.tree.open.insert(name.clone());
                }
            }
        }
        self.schema = Some(schema);
        let count = self.nodes().len();
        self.tree.selected = self.tree.selected.min(count.saturating_sub(1));
    }

    /// Ask to run SQL: straight away when it only reads, else after
    /// showing it. `to_popup` shows the result over the code.
    pub fn ask(&mut self, sql: String, to_popup: bool) -> Outcome {
        if self.pool.is_none() {
            return Outcome::Status(format!(
                "No database connection: {} picks one",
                "the Database view"
            ));
        }
        if sql.trim().is_empty() {
            return Outcome::Status("No SQL under the caret".into());
        }
        if sql::reads_only(self.engine_or_default(), &sql) {
            self.execute(Pending {
                sql,
                rerun: false,
                to_popup,
            })
        } else {
            self.popup = Some(Popup::Confirm {
                confirm: confirm::Confirm::new(sql),
                rerun: false,
                to_popup,
            });
            Outcome::Pending
        }
    }

    /// A write was confirmed over the code (the popup there).
    pub fn confirmed(&mut self, sql: String) -> Outcome {
        self.execute(Pending {
            sql,
            rerun: false,
            to_popup: true,
        })
    }

    /// The write waiting for confirmation, for the popup over the code.
    pub fn take_confirm(&mut self) -> Option<confirm::Confirm> {
        match self.popup.take() {
            Some(Popup::Confirm {
                confirm,
                to_popup: true,
                ..
            }) => Some(confirm),
            other => {
                self.popup = other;
                None
            }
        }
    }

    fn execute(&mut self, pending: Pending) -> Outcome {
        self.generation += 1;
        let sql = pending.sql.clone();
        // A cell edit writes one row or nothing.
        let one_row = pending.rerun;
        self.pending = Some(pending);
        if one_row {
            Outcome::ExecuteOneRow(sql, self.generation)
        } else {
            Outcome::Execute(sql, self.generation)
        }
    }

    /// The schema and engine, once connected, for SQL completion.
    pub fn schema_info(&self) -> Option<(&Schema, conn::Engine)> {
        Some((self.schema.as_ref()?, self.engine()?))
    }

    pub fn has_popup(&self) -> bool {
        self.popup.is_some()
    }

    /// The open connection, to run something on off the loop.
    pub fn handle(&self) -> Option<Pool> {
        self.pool.clone()
    }

    pub fn running(&self) -> bool {
        self.pending.is_some()
    }

    /// The SQL ran (or failed).
    pub fn finished(&mut self, generation: u64, result: Result<ResultSet, String>) -> Outcome {
        if generation != self.generation {
            return Outcome::Pending;
        }
        let Some(pending) = self.pending.take() else {
            return Outcome::Pending;
        };
        let result = match result {
            Ok(result) => result,
            Err(err) => {
                if pending.to_popup {
                    return Outcome::Status(format!("SQL failed: {err}"));
                }
                // A cell edit that failed (or was undone): the rows stay.
                if pending.rerun {
                    return Outcome::Status(format!("Not written: {err}"));
                }
                self.error = Some(err);
                return Outcome::Pending;
            }
        };
        let changes_schema = !sql::reads_only(self.engine_or_default(), &pending.sql)
            && ["create", "alter", "drop", "rename"].iter().any(|word| {
                pending
                    .sql
                    .to_ascii_lowercase()
                    .split_whitespace()
                    .any(|part| part == *word)
            });
        if pending.to_popup {
            return Outcome::Popup(Grid::new(result, pending.sql));
        }
        if pending.rerun {
            // A cell was written: show the rows again, in place, when the
            // grid's query only reads. One that writes (`... RETURNING`)
            // never runs again by itself.
            let engine = self.engine_or_default();
            let again = self
                .grid
                .as_ref()
                .filter(|old| sql::reads_only(engine, &old.sql))
                .map(|old| Pending {
                    sql: old.sql.clone(),
                    rerun: false,
                    to_popup: false,
                });
            return match again {
                Some(again) => self.execute(again),
                None => Outcome::Status(format!(
                    "{}; the rows shown are from before (the query changes data, so r asks first)",
                    grid::summary(&result)
                )),
            };
        }
        self.error = None;
        let mut grid = Grid::new(result, pending.sql);
        if let Some(old) = &self.grid
            && old.sql == grid.sql
        {
            grid.keep_place(old);
        }
        self.grid = Some(grid);
        if changes_schema {
            Outcome::Refresh(self.generation)
        } else {
            Outcome::Pending
        }
    }

    pub fn paste(&mut self, text: &str) {
        match &mut self.popup {
            Some(Popup::Edit { text: value, null }) => {
                value.push_str(text);
                *null = false;
            }
            _ if self.tree.filtering => {
                if let Some(filter) = &mut self.tree.filter {
                    filter.push_str(text.lines().next().unwrap_or(""));
                }
            }
            _ => {}
        }
    }

    /// Keys while the view is on screen, the query editor's excepted
    /// (the app edits that file). `None` means not used here.
    pub fn handle_key(&mut self, key: &KeyEvent) -> Option<Outcome> {
        if self.popup.is_some() {
            return Some(self.popup_key(key));
        }
        let shift_tab = key.code == KeyCode::BackTab
            || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT));
        if shift_tab || (key.code == KeyCode::Tab && self.focus != Focus::Query) {
            self.focus = self.focus.next();
            if self.focus == Focus::Query && self.pool.is_none() {
                self.focus = self.focus.next();
            }
            return Some(Outcome::Pending);
        }
        // Ctrl and Alt chords that are not actions mean nothing here.
        if self.focus != Focus::Query
            && key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Some(Outcome::Pending);
        }
        if !matches!(self.phase, Phase::Ready) {
            return match key.code {
                KeyCode::Char('c') if !self.connections.is_empty() => {
                    self.open_connections();
                    Some(Outcome::Pending)
                }
                KeyCode::Char('r') => {
                    self.connections.clear();
                    self.phase = Phase::Finding;
                    Some(Outcome::Discover)
                }
                _ if self.focus == Focus::Query => None,
                _ => Some(Outcome::Pending),
            };
        }
        match self.focus {
            Focus::Query => None,
            Focus::Tree => Some(self.tree_key(key)),
            Focus::Grid => Some(self.grid_key(key)),
        }
    }

    fn open_connections(&mut self) {
        self.popup = Some(Popup::Connections {
            selected: self.current.unwrap_or(0),
        });
    }

    fn popup_key(&mut self, key: &KeyEvent) -> Outcome {
        let Some(popup) = &mut self.popup else {
            return Outcome::Pending;
        };
        match popup {
            Popup::Connections { selected } => match key.code {
                KeyCode::Esc => {
                    self.popup = None;
                    Outcome::Pending
                }
                KeyCode::Up => {
                    *selected = selected.saturating_sub(1);
                    Outcome::Pending
                }
                KeyCode::Down => {
                    *selected = (*selected + 1).min(self.connections.len().saturating_sub(1));
                    Outcome::Pending
                }
                KeyCode::Enter => {
                    let idx = *selected;
                    self.popup = None;
                    self.connect(idx)
                }
                _ => Outcome::Pending,
            },
            Popup::Confirm {
                confirm,
                rerun,
                to_popup,
            } => match confirm.handle_key(key) {
                confirm::Answer::Run => {
                    let pending = Pending {
                        sql: confirm.sql.clone(),
                        rerun: *rerun,
                        to_popup: *to_popup,
                    };
                    self.popup = None;
                    self.execute(pending)
                }
                confirm::Answer::Cancel => {
                    self.popup = None;
                    Outcome::Status("Not run".into())
                }
                confirm::Answer::NotSeen => {
                    Outcome::Status("Scroll down to see all of the SQL first".into())
                }
                confirm::Answer::Pending => Outcome::Pending,
            },
            Popup::Edit { text, null } => match key.code {
                KeyCode::Esc => {
                    self.popup = None;
                    Outcome::Pending
                }
                KeyCode::Tab => {
                    *null = !*null;
                    Outcome::Pending
                }
                KeyCode::Backspace => {
                    crate::picker::erase(text, key);
                    *null = false;
                    Outcome::Pending
                }
                KeyCode::Enter => {
                    let value = (!*null).then(|| text.clone());
                    self.popup = None;
                    self.write_cell(value.as_deref())
                }
                KeyCode::Char(chr)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    text.push(chr);
                    *null = false;
                    Outcome::Pending
                }
                _ => Outcome::Pending,
            },
        }
    }

    /// The current connection's engine, for reading SQL (Postgres's rules
    /// before connecting).
    fn engine_or_default(&self) -> conn::Engine {
        self.engine().unwrap_or(conn::Engine::Postgres)
    }

    pub fn engine(&self) -> Option<conn::Engine> {
        self.current
            .and_then(|idx| self.connections.get(idx))
            .map(|conn| conn.engine)
    }

    /// Where writing the grid's cursor cell goes: the quoted table, the
    /// column, and the row's primary key; or why it cannot be edited. Any
    /// result works (a whole table, some columns, a join) as long as the
    /// server says the column is a table's and that table's primary key is
    /// in the result too.
    fn edit_target(&self) -> Result<EditTarget, String> {
        let grid = self.grid.as_ref().ok_or("No rows")?;
        let engine = self.engine().ok_or("Not connected")?;
        let row = grid.result.rows.get(grid.row).ok_or("No row to edit")?;
        let name = grid.column_name().unwrap_or("This column");
        let origin = grid
            .result
            .origins
            .get(grid.col)
            .cloned()
            .flatten()
            .ok_or_else(|| format!("{name} is computed, not a column of a table"))?;
        let table = self
            .origin_table(&origin)
            .ok_or_else(|| format!("{} is not in the schema (r reads it again)", origin.table))?;
        if table.view {
            return Err(format!(
                "{} is a view; its rows cannot be edited",
                table.name
            ));
        }
        let key = table.primary_key();
        if key.is_empty() {
            return Err(format!(
                "{} has no primary key, so its rows cannot be told apart",
                table.name
            ));
        }
        // The same table twice (a self-join): the server names the table,
        // not which use of it a column comes from, so the row is ambiguous.
        let statement = sql::split(engine, &grid.sql)
            .last()
            .and_then(|range| grid.sql.get(range.clone()))
            .unwrap_or("");
        let uses = self.schema.as_ref().map_or(0, |schema| {
            assist::table_uses(schema, engine, statement, table)
        });
        if uses > 1 {
            return Err(format!(
                "{} appears more than once in the query, so which row to edit is unclear",
                table.name
            ));
        }
        let mut values = Vec::new();
        for column in &key {
            let mut found = grid.result.origins.iter().enumerate().filter(|(_, other)| {
                other.as_ref().is_some_and(|other| {
                    other.table == origin.table
                        && other.schema == origin.schema
                        && other.column == *column
                })
            });
            let Some((idx, _)) = found.next() else {
                return Err(format!(
                    "Add {}'s primary key ({}) to the query to edit its rows",
                    table.name,
                    key.join(", ")
                ));
            };
            if found.next().is_some() {
                return Err(format!(
                    "{}.{column} is in the result twice, so which row to edit is unclear",
                    table.name
                ));
            }
            // `WHERE key IS NULL` could match many rows (SQLite allows NULL
            // in a primary key that is not an integer).
            let Some(value) = row.get(idx).cloned().flatten() else {
                return Err(format!(
                    "{}.{column} is NULL in this row, so the row cannot be picked out",
                    table.name
                ));
            };
            values.push(((*column).to_string(), Some(value)));
        }
        Ok(EditTarget {
            table: sql::table_name(engine, &table.schema, &table.name),
            column: origin.column,
            key: values,
        })
    }

    /// The schema's table a result column comes from. Without a schema
    /// name from the server, the connection's default schema, else the
    /// only table of that name.
    fn origin_table(&self, origin: &Origin) -> Option<&driver::Table> {
        let tables = &self.schema.as_ref()?.tables;
        let conn = self.current.and_then(|idx| self.connections.get(idx))?;
        let default = match conn.engine {
            conn::Engine::Sqlite => "main",
            conn::Engine::MySql => conn.database.as_str(),
            conn::Engine::Postgres => "public",
        };
        let schema = if origin.schema.is_empty() {
            default
        } else {
            origin.schema.as_str()
        };
        tables
            .iter()
            .find(|table| table.name == origin.table && table.schema == schema)
            .or_else(|| {
                let mut same = tables.iter().filter(|table| table.name == origin.table);
                let first = same.next();
                if same.next().is_none() { first } else { None }
            })
    }

    /// The UPDATE for the cell under the grid's cursor, shown first.
    fn write_cell(&mut self, value: Option<&str>) -> Outcome {
        let (target, engine) = match (self.edit_target(), self.engine()) {
            (Ok(target), Some(engine)) => (target, engine),
            (Err(why), _) => return Outcome::Status(why),
            (_, None) => return Outcome::Pending,
        };
        let key: Vec<(&str, Option<&str>)> = target
            .key
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_deref()))
            .collect();
        let sql = sql::update_cell(engine, &target.table, (&target.column, value), &key);
        self.popup = Some(Popup::Confirm {
            confirm: confirm::Confirm::new(sql),
            rerun: true,
            to_popup: false,
        });
        Outcome::Pending
    }

    fn tree_key(&mut self, key: &KeyEvent) -> Outcome {
        if self.tree.filtering {
            let filter = self.tree.filter.get_or_insert_with(String::new);
            match key.code {
                KeyCode::Esc => {
                    self.tree.filter = None;
                    self.tree.filtering = false;
                }
                KeyCode::Enter | KeyCode::Down | KeyCode::Up => {
                    self.tree.filtering = false;
                    if key.code != KeyCode::Enter {
                        return self.tree_key(key);
                    }
                }
                KeyCode::Backspace => crate::picker::erase(filter, key),
                KeyCode::Char(chr) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    filter.push(chr);
                }
                _ => {}
            }
            self.tree.selected = 0;
            self.tree.top = 0;
            return Outcome::Pending;
        }
        let nodes = self.nodes();
        let last = nodes.len().saturating_sub(1);
        let node = nodes.get(self.tree.selected).cloned();
        match key.code {
            KeyCode::Up => self.tree.selected = self.tree.selected.saturating_sub(1),
            KeyCode::Down => self.tree.selected = (self.tree.selected + 1).min(last),
            KeyCode::PageUp => {
                self.tree.selected = self.tree.selected.saturating_sub(self.tree.page);
            }
            KeyCode::PageDown => {
                self.tree.selected = (self.tree.selected + self.tree.page).min(last);
            }
            KeyCode::Home => self.tree.selected = 0,
            KeyCode::End => self.tree.selected = last,
            KeyCode::Right => {
                if let Some(name) = node.as_ref().and_then(|node| self.node_key(node)) {
                    self.tree.open.insert(name);
                }
            }
            KeyCode::Left => self.collapse(node.as_ref(), &nodes),
            KeyCode::Enter => match &node {
                Some(Node::Table(idx)) => return self.open_table(*idx),
                Some(node @ Node::Schema(_)) => {
                    if let Some(name) = self.node_key(node)
                        && !self.tree.open.remove(&name)
                    {
                        self.tree.open.insert(name);
                    }
                }
                _ => {}
            },
            KeyCode::Char('s') => {
                if let Some(table) = node.as_ref().and_then(|node| self.node_table(node)) {
                    return Outcome::Insert(format!("select * from {table};"));
                }
            }
            KeyCode::Char('/') => {
                self.tree.filter = Some(String::new());
                self.tree.filtering = true;
            }
            KeyCode::Esc if self.tree.filter.is_some() => self.tree.filter = None,
            KeyCode::Char('c') => self.open_connections(),
            KeyCode::Char('r') => return Outcome::Refresh(self.generation),
            KeyCode::Char('y') => {
                if let Some(name) = node.as_ref().and_then(|node| self.node_name(node)) {
                    return Outcome::Copy(name.clone(), format!("Copied {name}"));
                }
            }
            _ => {}
        }
        Outcome::Pending
    }

    fn collapse(&mut self, node: Option<&Node>, nodes: &[Node]) {
        let Some(node) = node else { return };
        if let Some(name) = self.node_key(node)
            && self.tree.open.remove(&name)
        {
            return;
        }
        // Already closed (or a column): go to the parent.
        let parent = match node {
            Node::Column(table, _) => Some(Node::Table(*table)),
            Node::Table(idx) => self
                .table(*idx)
                .map(|table| Node::Schema(table.schema.clone())),
            Node::Schema(_) => None,
        };
        if let Some(parent) = parent
            && let Some(pos) = nodes.iter().position(|other| *other == parent)
        {
            self.tree.selected = pos;
        }
    }

    fn grid_key(&mut self, key: &KeyEvent) -> Outcome {
        let Some(grid) = &mut self.grid else {
            return Outcome::Pending;
        };
        if grid.handle_key(key) {
            return Outcome::Pending;
        }
        match key.code {
            KeyCode::Enter => {
                let current = grid.cell().cloned();
                if let Err(why) = self.edit_target() {
                    return Outcome::Status(why);
                }
                self.popup = Some(Popup::Edit {
                    text: current.clone().flatten().unwrap_or_default(),
                    null: matches!(current, Some(None)),
                });
                Outcome::Pending
            }
            KeyCode::Char('y') => match grid.cell() {
                Some(value) => {
                    let text = value.as_deref().unwrap_or("").to_string();
                    Outcome::Copy(text, "Copied the value".into())
                }
                None => Outcome::Pending,
            },
            // Run again, asking first when the query changes data.
            KeyCode::Char('r') => {
                let again = grid.sql.clone();
                self.ask(again, false)
            }
            _ => Outcome::Pending,
        }
    }

    fn open_table(&mut self, idx: usize) -> Outcome {
        let (Some(table), Some(engine)) = (self.table(idx), self.engine()) else {
            return Outcome::Pending;
        };
        let name = sql::table_name(engine, &table.schema, &table.name);
        let key: Vec<String> = table
            .primary_key()
            .into_iter()
            .map(str::to_string)
            .collect();
        let order = if key.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = key
                .iter()
                .map(|column| sql::ident(engine, column))
                .collect();
            format!(" ORDER BY {}", names.join(", "))
        };
        self.focus = Focus::Grid;
        self.execute(Pending {
            sql: format!(
                "SELECT * FROM {name}{order} LIMIT {}",
                crate::settings::get().database.table_rows
            ),
            rerun: false,
            to_popup: false,
        })
    }

    fn table(&self, idx: usize) -> Option<&driver::Table> {
        self.schema.as_ref()?.tables.get(idx)
    }

    fn node_key(&self, node: &Node) -> Option<String> {
        match node {
            Node::Schema(name) => Some(name.clone()),
            Node::Table(idx) => self
                .table(*idx)
                .map(|table| format!("{}.{}", table.schema, table.name)),
            Node::Column(..) => None,
        }
    }

    /// The quoted table a node belongs to.
    fn node_table(&self, node: &Node) -> Option<String> {
        let idx = match node {
            Node::Table(idx) | Node::Column(idx, _) => *idx,
            Node::Schema(_) => return None,
        };
        let table = self.table(idx)?;
        Some(sql::table_name(self.engine()?, &table.schema, &table.name))
    }

    fn node_name(&self, node: &Node) -> Option<String> {
        match node {
            Node::Schema(name) => Some(name.clone()),
            Node::Table(idx) => self.table(*idx).map(|table| table.name.clone()),
            Node::Column(table, column) => self
                .table(*table)
                .and_then(|table| table.columns.get(*column))
                .map(|column| column.name.clone()),
        }
    }

    /// The tree's rows: schemas (left out when there is only one), their
    /// tables when open, a table's columns when open. A filter shows the
    /// matching tables of every schema.
    fn nodes(&self) -> Vec<Node> {
        let Some(schema) = &self.schema else {
            return Vec::new();
        };
        let names = schema_names(schema);
        let flat = names.len() <= 1;
        let filter = self
            .tree
            .filter
            .as_deref()
            .filter(|text| !text.is_empty())
            .map(str::to_lowercase);
        let mut out = Vec::new();
        for name in &names {
            if !flat {
                out.push(Node::Schema(name.clone()));
            }
            if !flat && filter.is_none() && !self.tree.open.contains(name) {
                continue;
            }
            for (idx, table) in schema.tables.iter().enumerate() {
                if table.schema != *name
                    || filter
                        .as_ref()
                        .is_some_and(|text| !table.name.to_lowercase().contains(text))
                {
                    continue;
                }
                out.push(Node::Table(idx));
                if self
                    .tree
                    .open
                    .contains(&format!("{}.{}", table.schema, table.name))
                {
                    out.extend((0..table.columns.len()).map(|col| Node::Column(idx, col)));
                }
            }
        }
        if filter.is_some() && !flat {
            // Schemas without a match are left out.
            let keep: Vec<Node> = out
                .iter()
                .enumerate()
                .filter(|(pos, node)| match node {
                    Node::Schema(_) => matches!(out.get(pos + 1), Some(Node::Table(_))),
                    _ => true,
                })
                .map(|(_, node)| node.clone())
                .collect();
            return keep;
        }
        out
    }

    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match &self.popup {
            Some(Popup::Connections { .. }) => {
                return vec![("Enter", "Connect"), ("Up/Down", "Move"), ("Esc", "Close")];
            }
            Some(Popup::Confirm { confirm, .. }) => return confirm.hints(),
            Some(Popup::Edit { .. }) => {
                return vec![("Enter", "Write"), ("Tab", "NULL"), ("Esc", "Cancel")];
            }
            None => {}
        }
        if !matches!(self.phase, Phase::Ready) {
            return vec![("c", "Connections"), ("r", "Find again")];
        }
        match self.focus {
            Focus::Tree if self.tree.filtering => {
                vec![("Type", "Filter"), ("Enter", "Done"), ("Esc", "Clear")]
            }
            Focus::Tree => vec![
                ("Enter", "Open table"),
                ("Right/Left", "Expand"),
                ("s", "Select in query"),
                ("/", "Filter"),
                ("c", "Connections"),
                ("r", "Refresh"),
                ("Tab", "Next pane"),
            ],
            Focus::Query => vec![("Shift+Tab", "Next pane")],
            Focus::Grid => vec![
                ("Enter", "Edit cell"),
                ("y", "Copy value"),
                ("r", "Run again"),
                ("Tab", "Next pane"),
            ],
        }
    }

    /// Draw the view. Gives back where the query editor goes, when it is
    /// shown, for the app to draw the file there; the app then draws
    /// [`Database::render_popup`] over both.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        top: Rect,
        body: Rect,
        theme: &Theme,
    ) -> Option<Rect> {
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        self.render_title(frame, top, theme);
        let message = match &self.phase {
            Phase::Finding => Some("Looking for databases...".to_string()),
            Phase::Connecting => Some("Connecting...".to_string()),
            Phase::Idle(text) => Some(text.clone()),
            Phase::Ready => None,
        };
        if let Some(text) = message {
            frame.render_widget(
                Paragraph::new(Line::styled(format!("  {text}"), muted))
                    .style(base)
                    .wrap(Wrap { trim: false }),
                body,
            );
            return None;
        }
        let tree_width = (body.width / 4).clamp(24, 44).min(body.width / 2);
        let tree_area = Rect {
            width: tree_width,
            ..body
        };
        let right = Rect {
            x: body.x.saturating_add(tree_width),
            width: body.width.saturating_sub(tree_width),
            ..body
        };
        let query_height = (right.height * 2 / 5).max(5).min(right.height);
        let query_area = Rect {
            height: query_height,
            ..right
        };
        let grid_area = Rect {
            y: right.y.saturating_add(query_height),
            height: right.height.saturating_sub(query_height),
            ..right
        };
        self.render_tree(frame, tree_area, theme);
        let name = self
            .current
            .and_then(|idx| self.connections.get(idx))
            .map_or_else(String::new, |conn| conn.name.clone());
        let query_block = self
            .pane(Focus::Query, format!(" {name}.sql "), theme)
            .title(Line::styled(" Alt+Enter run ", muted).right_aligned());
        let query_inner = query_block.inner(query_area);
        frame.render_widget(query_block, query_area);
        self.render_grid(frame, grid_area, theme);
        Some(query_inner)
    }

    fn pane(&self, focus: Focus, title: String, theme: &Theme) -> Block<'static> {
        let base = crate::styles::surface(theme);
        let border = if self.focus == focus {
            base
        } else {
            theme.style(ThemeRole::Muted)
        };
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(title)
            .style(base)
    }

    fn render_title(&self, frame: &mut Frame, top: Rect, theme: &Theme) {
        let mut text = String::from(" Database");
        if let Some(conn) = self.current.and_then(|idx| self.connections.get(idx)) {
            let _ = write!(text, "   {} ({})", conn.name, conn.describe());
        }
        if let Some(schema) = &self.schema {
            let count = schema.tables.len();
            let _ = write!(
                text,
                "   {count} {}",
                if count == 1 { "table" } else { "tables" }
            );
        }
        if self.running() {
            text.push_str("   (running...)");
        }
        frame.render_widget(Paragraph::new(text).style(crate::styles::bar(theme)), top);
    }

    fn render_tree(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let muted = theme.style(ThemeRole::Muted);
        let title = match &self.tree.filter {
            Some(filter) => format!(" /{filter} "),
            None => " schema ".to_string(),
        };
        let block = self.pane(Focus::Tree, title, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let nodes = self.nodes();
        let height = usize::from(inner.height);
        self.tree.page = height.saturating_sub(1).max(1);
        if self.tree.selected < self.tree.top {
            self.tree.top = self.tree.selected;
        } else if height > 0 && self.tree.selected >= self.tree.top + height {
            self.tree.top = self.tree.selected + 1 - height;
        }
        let flat = self
            .schema
            .as_ref()
            .is_none_or(|schema| schema_names(schema).len() <= 1);
        let width = usize::from(inner.width);
        let type_style = theme.token_style(karet_core::StandardToken::Type.id());
        let lines: Vec<Line> = nodes
            .iter()
            .enumerate()
            .skip(self.tree.top)
            .take(height)
            .map(|(pos, node)| {
                let indent = |level: usize| "  ".repeat(if flat { level - 1 } else { level });
                let arrow = |open: bool| if open { "v " } else { "> " };
                let mut spans = match node {
                    Node::Schema(name) => vec![Span::raw(format!(
                        "{}{name}",
                        arrow(self.tree.open.contains(name))
                    ))],
                    Node::Table(idx) => {
                        let table = self.table(*idx);
                        let open = table.is_some_and(|table| {
                            self.tree
                                .open
                                .contains(&format!("{}.{}", table.schema, table.name))
                        });
                        let name = table.map_or("", |table| table.name.as_str());
                        let mut spans =
                            vec![Span::raw(format!("{}{}{name}", indent(1), arrow(open)))];
                        if table.is_some_and(|table| table.view) {
                            spans.push(Span::styled(" view", muted));
                        }
                        spans
                    }
                    Node::Column(table, column) => {
                        let column = self
                            .table(*table)
                            .and_then(|table| table.columns.get(*column));
                        let (name, sql_type, key) = column.map_or(("", "", false), |column| {
                            (
                                column.name.as_str(),
                                column.sql_type.as_str(),
                                column.primary,
                            )
                        });
                        let mark = if key { "* " } else { "  " };
                        vec![
                            Span::raw(format!("{}{mark}{name} ", indent(2))),
                            Span::styled(sql_type.to_string(), type_style),
                        ]
                    }
                };
                let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
                spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
                let line = Line::from(spans);
                if pos == self.tree.selected {
                    selected(line, theme, self.focus == Focus::Tree)
                } else {
                    line
                }
            })
            .collect();
        if lines.is_empty() {
            let text = if self.tree.filter.is_some() {
                "  No table matches"
            } else {
                "  No tables"
            };
            frame.render_widget(Paragraph::new(Line::styled(text, muted)), inner);
        } else {
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }

    fn render_grid(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let error_style = theme.style(ThemeRole::DiagnosticError);
        let title = match (&self.error, &self.grid) {
            (Some(_), _) => " error ".to_string(),
            (None, Some(grid)) => format!(" {} ", grid.summary()),
            (None, None) => " result ".to_string(),
        };
        let mut block = self.pane(Focus::Grid, title, theme);
        if let Some(grid) = &self.grid
            && grid.result.origins.iter().any(Option::is_some)
            && self.error.is_none()
        {
            block = block.title(
                Line::styled(" Enter edits a cell ", theme.style(ThemeRole::Muted)).right_aligned(),
            );
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if let Some(err) = &self.error {
            frame.render_widget(
                Paragraph::new(Line::styled(err.clone(), error_style)).wrap(Wrap { trim: false }),
                inner,
            );
            return;
        }
        let focused = self.focus == Focus::Grid;
        if let Some(grid) = &mut self.grid {
            grid.render(frame, inner, theme, focused);
        }
    }

    pub fn render_popup(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let Some(popup) = &mut self.popup else { return };
        match popup {
            Popup::Connections { selected } => {
                let lines: Vec<Line> = self
                    .connections
                    .iter()
                    .enumerate()
                    .map(|(idx, conn)| {
                        let line = Line::from(vec![
                            Span::raw(format!(" {} ", conn.name)),
                            Span::styled(
                                format!("{} ", conn.describe()),
                                theme.style(ThemeRole::Muted),
                            ),
                        ]);
                        if idx == *selected {
                            line.style(Style::default().add_modifier(Modifier::REVERSED))
                        } else {
                            line
                        }
                    })
                    .collect();
                popup_box(frame, area, " Connections ", lines, theme);
            }
            Popup::Confirm { confirm, .. } => confirm.render(frame, area, theme),
            Popup::Edit { text, null } => {
                let column = self
                    .grid
                    .as_ref()
                    .and_then(Grid::column_name)
                    .unwrap_or("value");
                let value = if *null {
                    Line::styled("NULL", theme.style(ThemeRole::Muted))
                } else {
                    Line::raw(format!("{text}\u{2588}"))
                };
                popup_box(frame, area, &format!(" {column} "), vec![value], theme);
            }
        }
    }
}

/// A centred box with a border and these lines.
pub fn popup_box(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line>, theme: &Theme) {
    let base = crate::styles::surface(theme);
    let wanted = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(40)
        .max(title.chars().count() + 4)
        .saturating_add(4);
    let width = u16::try_from(wanted.max(50))
        .unwrap_or(u16::MAX)
        .min(area.width.saturating_sub(4));
    let height = u16::try_from(lines.len().saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 3,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title.to_string())
        .style(base);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

/// The schema names in order.
fn schema_names(schema: &Schema) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for table in &schema.tables {
        if names.last() != Some(&table.schema) && !names.contains(&table.schema) {
            names.push(table.schema.clone());
        }
    }
    names
}

fn selected<'a>(line: Line<'a>, theme: &Theme, focused: bool) -> Line<'a> {
    let colour = theme.style(ThemeRole::Selection).fg;
    match colour {
        Some(colour) if focused => {
            let spans: Vec<Span> = line
                .spans
                .into_iter()
                .map(|span| {
                    let style = span.style.bg(colour);
                    span.style(style)
                })
                .collect();
            Line::from(spans)
        }
        None if focused => line.style(Style::default().add_modifier(Modifier::REVERSED)),
        _ => line.style(Style::default().add_modifier(Modifier::BOLD)),
    }
}

/// Where this workspace's database state lives: outside the project.
fn state_dir(root: &Path) -> Option<PathBuf> {
    crate::statefile::path("db", root, "")
}

/// The query editor's file for a connection, made when missing.
fn scratch_file(root: &Path, connection: &str) -> Option<PathBuf> {
    let dir = state_dir(root)?;
    std::fs::create_dir_all(&dir).ok()?;
    let safe: String = connection
        .chars()
        .map(|chr| {
            if chr.is_alphanumeric() || chr == '-' || chr == '_' {
                chr
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("{safe}.sql"));
    if !path.exists() {
        std::fs::write(&path, "").ok()?;
    }
    Some(path)
}

fn last_connection(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(state_dir(root)?.join("last")).ok()?;
    Some(text.trim().to_string()).filter(|name| !name.is_empty())
}

fn save_last(root: &Path, name: &str) {
    if let Some(dir) = state_dir(root)
        && std::fs::create_dir_all(&dir).is_ok()
    {
        let _ = std::fs::write(dir.join("last"), name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(schema: &str, name: &str) -> driver::Table {
        driver::Table {
            schema: schema.into(),
            name: name.into(),
            view: false,
            columns: vec![driver::Column {
                name: "id".into(),
                sql_type: "int".into(),
                nullable: false,
                primary: true,
            }],
        }
    }

    #[test]
    fn tree_opens_the_default_schema_and_filters_tables() {
        let mut view = Database::new(PathBuf::from("/nowhere"));
        view.set_schema(Schema {
            tables: vec![
                table("audit", "log"),
                table("public", "auth_user"),
                table("public", "shop_order"),
            ],
        });
        assert_eq!(
            view.nodes(),
            [
                Node::Schema("audit".into()),
                Node::Schema("public".into()),
                Node::Table(1),
                Node::Table(2),
            ]
        );
        view.tree.filter = Some("LO".into());
        assert_eq!(view.nodes(), [Node::Schema("audit".into()), Node::Table(0)]);
    }

    /// Against real servers: `SANITTY_DB_ROOT=/path/to/project cargo test
    /// live -- --ignored --nocapture` connects to every connection found
    /// there, reads its schema and the first table.
    #[tokio::test]
    #[ignore = "needs a project with running database servers"]
    async fn live() {
        let Some(root) = std::env::var_os("SANITTY_DB_ROOT") else {
            return;
        };
        let (found, problems) = conn::discover(Path::new(&root));
        println!("problems: {problems:?}");
        for conn in &found {
            println!("{}: {}", conn.name, conn.describe());
            match driver::connect(conn).await {
                Ok(pool) => match driver::schema(&pool).await {
                    Ok(schema) => {
                        println!("  {} tables", schema.tables.len());
                        if let Some(table) = schema.tables.first() {
                            let name = sql::table_name(conn.engine, &table.schema, &table.name);
                            let sql = format!("select *, 1 as one from {name} limit 3");
                            match driver::run(&pool, &sql).await {
                                Ok(result) => {
                                    println!("  {:?}", result.rows.first());
                                    println!("  origins {:?}", result.origins);
                                }
                                Err(err) => println!("  {err}"),
                            }
                            if conn.engine == conn::Engine::MySql {
                                // `#` hides the rest of the line: still a read.
                                let sql = format!("select * from {name} # x; delete from {name}");
                                println!(
                                    "  reads only: {}, {:?}",
                                    sql::reads_only(conn.engine, &sql),
                                    driver::run(&pool, &sql).await.map(|found| found.rows.len())
                                );
                            }
                            for sql in [
                                format!("select * from {name} where 1 = 0"),
                                format!("explain select * from {name}"),
                            ] {
                                match driver::run(&pool, &sql).await {
                                    Ok(result) => println!(
                                        "  {sql}: {} rows, columns {:?}, first origin {:?}",
                                        result.rows.len(),
                                        result.columns,
                                        result.origins.first()
                                    ),
                                    Err(err) => println!("  {sql}: {err}"),
                                }
                            }
                        }
                    }
                    Err(err) => println!("  schema: {err}"),
                },
                Err(err) => println!("  {err}"),
            }
        }
    }
}
