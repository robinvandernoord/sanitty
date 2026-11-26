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
mod render;
pub mod sql;
mod tree;

use tree::schema_names;

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;

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
    /// Show this result over the code (it was run from a `.sql` file), and
    /// read the schema again with this generation when it changed it.
    Popup(Grid, Option<u64>),
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
    /// Bumped for every schema read, apart from runs: a query started while
    /// the schema is being read again does not cancel that.
    schema_generation: u64,
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
            schema_generation: 0,
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
        self.schema_generation += 1;
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
        if generation != self.schema_generation {
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

    /// A new schema read: the generation its answer must carry.
    fn refresh(&mut self) -> u64 {
        self.schema_generation += 1;
        self.schema_generation
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
            let refresh = changes_schema.then(|| self.refresh());
            return Outcome::Popup(Grid::new(result, pending.sql), refresh);
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
            Outcome::Refresh(self.refresh())
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
