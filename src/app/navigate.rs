//! Moving between files: the launcher, the project tree, opening files and jumping to places.

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use karet_core::LineCol;

use crate::doc::Doc;
use crate::explorer::Explorer;
use crate::keymap::Action;
use crate::lsp;
use crate::picker::Item;
use crate::picker::Kind;
use crate::picker::Picker;
use crate::picker::Target;

use super::App;
use super::Overlay;
use super::file_name;
use super::parent_dir;

impl App {
    /// Close the current popup; a closing find bar is kept for F3.
    /// The find bar is kept for F3, the tree to reopen where it was left.
    pub(super) fn close_overlay(&mut self) {
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Find(find) => self.last_find = Some(find),
            Overlay::Explorer(explorer) => self.tree = Some(explorer),
            _ => {}
        }
    }

    /// Open the tree as it was left, refreshed, with the current file selected.
    pub(super) fn open_tree(&mut self) {
        let reveal = self.cur().map(|doc| doc.path.clone());
        let mut tree = self
            .tree
            .take()
            .unwrap_or_else(|| Explorer::new(&self.root, None));
        tree.reveal(reveal.as_deref());
        self.overlay = Overlay::Explorer(tree);
    }

    pub(super) fn open_launcher(&mut self, tab: Kind, query: &str) {
        let mut items = self.file_items();
        items.extend(
            Action::ALL
                .iter()
                .filter(|(action, ..)| *action != Action::Launcher)
                .map(|(action, label, _)| Item {
                    label: (*label).to_string(),
                    detail: self.display_key(*action),
                    target: Target::Action(*action),
                    kind: Kind::Action,
                    keywords: action.keywords(),
                }),
        );
        items.extend(self.problem_items());
        self.overlay = Overlay::Picker(Picker::launcher(items, tab, query));
        self.launcher_search();
    }

    /// Start (or cancel) the async parts of a launcher query.
    pub(super) fn launcher_search(&mut self) {
        let Overlay::Picker(picker) = &mut self.overlay else {
            return;
        };
        let (scope, query) = picker.scope();
        let query = query.to_string();
        let generation = self.search_gen.fetch_add(1, Ordering::Relaxed) + 1;
        let min_text = if scope == Kind::All { 3 } else { 2 };
        if matches!(scope, Kind::All | Kind::Text) && query.chars().count() >= min_text {
            picker.start_search(Kind::Text);
            crate::search::text(
                self.root.clone(),
                query.clone(),
                generation,
                self.search_gen.clone(),
                self.tx.clone(),
            );
        } else {
            picker.set_kind_items(Kind::Text, Vec::new());
        }
        let client = self
            .docs
            .get(self.active)
            .and_then(|doc| doc.lsp_lang)
            .and_then(|lang| self.lsp.client(lang))
            .or_else(|| self.lsp.any_client());
        if matches!(scope, Kind::All | Kind::Symbol)
            && !query.is_empty()
            && let Some(client) = client
        {
            picker.start_search(Kind::Symbol);
            crate::search::symbols(
                client,
                self.root.clone(),
                query,
                generation,
                self.tx.clone(),
            );
        } else {
            picker.set_kind_items(Kind::Symbol, Vec::new());
        }
    }

    /// Files for the launcher: recently used first, then the project index.
    pub(super) fn file_items(&self) -> Vec<Item> {
        let item = |path: &PathBuf| Item {
            label: file_name(path),
            detail: parent_dir(self.rel(path)),
            target: Target::File(path.clone()),
            kind: Kind::File,
            keywords: "",
        };
        let mut items: Vec<Item> = self.recent.iter().map(item).collect();
        items.extend(
            self.files
                .iter()
                .filter(|path| !self.recent.contains(path))
                .map(item),
        );
        items
    }

    pub(super) fn pick(&mut self, target: Target) {
        match target {
            Target::File(path) => self.open(&path, None),
            Target::Location(path, pos) => self.jump(&path, pos),
            Target::Action(action) => self.run(action),
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
        if let Some(idx) = self.docs.iter().position(|doc| doc.path == path) {
            self.active = idx;
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
                        for server in self.lsp.running(lang) {
                            lsp::did_open(
                                server,
                                path.clone(),
                                lang,
                                version,
                                text.clone(),
                                self.tx.clone(),
                            );
                        }
                        self.lsp.ensure(lang, &self.root, &self.tx);
                    }
                }
                Err(err) => {
                    self.status = format!("Cannot open {}: {err}", path.display());
                    return;
                }
            }
        }
        if let Some(pos) = pos
            && let Some(doc) = self.doc()
        {
            doc.goto(pos);
        }
        self.recent.retain(|recent| *recent != path);
        self.recent.insert(0, path);
        self.recent.truncate(50);
    }

    /// Jump to an LSP location (UTF-16 columns), recording where we came from.
    pub(super) fn jump(&mut self, path: &Path, pos16: LineCol) {
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
}
