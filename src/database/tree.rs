//! The schema tree: its rows, its keys, and opening a table from it.

use super::Database;
use super::Focus;
use super::KeyCode;
use super::KeyEvent;
use super::KeyModifiers;
use super::Node;
use super::Outcome;
use super::Pending;
use super::Schema;
use super::driver;
use super::sql;

impl Database {
    pub(super) fn tree_key(&mut self, key: &KeyEvent) -> Outcome {
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
                KeyCode::Char(chr)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
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
            KeyCode::Char('r') => return Outcome::Refresh(self.refresh()),
            KeyCode::Char('y') => {
                if let Some(name) = node.as_ref().and_then(|node| self.node_name(node)) {
                    return Outcome::Copy(name.clone(), format!("Copied {name}"));
                }
            }
            _ => {}
        }
        Outcome::Pending
    }

    pub(super) fn collapse(&mut self, node: Option<&Node>, nodes: &[Node]) {
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

    pub(super) fn open_table(&mut self, idx: usize) -> Outcome {
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

    pub(super) fn table(&self, idx: usize) -> Option<&driver::Table> {
        self.schema.as_ref()?.tables.get(idx)
    }

    pub(super) fn node_key(&self, node: &Node) -> Option<String> {
        match node {
            Node::Schema(name) => Some(name.clone()),
            Node::Table(idx) => self
                .table(*idx)
                .map(|table| format!("{}.{}", table.schema, table.name)),
            Node::Column(..) => None,
        }
    }

    /// The quoted table a node belongs to.
    pub(super) fn node_table(&self, node: &Node) -> Option<String> {
        let idx = match node {
            Node::Table(idx) | Node::Column(idx, _) => *idx,
            Node::Schema(_) => return None,
        };
        let table = self.table(idx)?;
        Some(sql::table_name(self.engine()?, &table.schema, &table.name))
    }

    pub(super) fn node_name(&self, node: &Node) -> Option<String> {
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
    pub(super) fn nodes(&self) -> Vec<Node> {
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
}

/// The schema names in order.
pub(super) fn schema_names(schema: &Schema) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for table in &schema.tables {
        if names.last() != Some(&table.schema) && !names.contains(&table.schema) {
            names.push(table.schema.clone());
        }
    }
    names
}
