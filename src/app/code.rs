//! Language server features: syncing open files, problems, hover, go to, rename and reformat.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use karet_core::LineCol;
use karet_core::Location;
use karet_core::Range;
use karet_core::Severity;
use karet_core::TextEdit;
use karet_core::ThemeRole;
use karet_core::VirtualText;
use karet_lsp::LspClient;

use crate::doc::Doc;
use crate::lsp;
use crate::lsp::Nav;
use crate::picker::Item;
use crate::picker::Kind;
use crate::picker::Picker;
use crate::picker::Target;

use super::App;
use super::Overlay;

impl App {
    /// F8 / Shift+F8: the next problem in this file, else the first one in the
    /// next file with problems (files in path order, wrapping around).
    pub(super) fn step_problem(&mut self, forward: bool) {
        let key = |pos: &LineCol| (pos.line, pos.col);
        let current = self.cur().map(|doc| doc.path.clone());
        if let Some(doc) = self.doc() {
            let caret = doc.caret();
            let mut starts: Vec<LineCol> = doc
                .diagnostics
                .iter()
                .filter(|diag| counts(diag.severity))
                .map(|diag| diag.range.start)
                .collect();
            starts.sort_by_key(key);
            let target = if forward {
                starts.iter().find(|start| key(start) > key(&caret))
            } else {
                starts.iter().rev().find(|start| key(start) < key(&caret))
            };
            if let Some(start) = target.copied() {
                doc.goto(start);
                return;
            }
        }
        let mut files: Vec<&PathBuf> = self
            .problems
            .iter()
            .filter(|(_, diags)| diags.iter().any(|diag| counts(diag.severity)))
            .map(|(path, _)| path)
            .collect();
        files.sort();
        let pos = current
            .as_ref()
            .map_or(0, |cur| files.partition_point(|path| *path <= cur));
        let next_idx = if forward {
            pos.checked_rem(files.len())
        } else {
            let here = current
                .as_ref()
                .and_then(|cur| files.iter().position(|path| *path == cur))
                .unwrap_or(pos);
            (here + files.len())
                .checked_sub(1)
                .and_then(|idx| idx.checked_rem(files.len()))
        };
        let Some(next) = next_idx
            .and_then(|idx| files.get(idx))
            .map(|path| (*path).clone())
        else {
            self.status = "No problems".into();
            return;
        };
        let mut starts: Vec<LineCol> = self
            .problems
            .get(&next)
            .into_iter()
            .flatten()
            .filter(|diag| counts(diag.severity))
            .map(|diag| diag.range.start)
            .collect();
        starts.sort_by_key(key);
        let first = if forward {
            starts.first()
        } else {
            starts.last()
        };
        if let Some(start) = first.copied() {
            // Ranges here are still UTF-16; `jump` converts.
            self.jump(&next, start);
        }
    }

    /// Launcher items for every reported problem, errors first.
    pub(super) fn problem_items(&self) -> Vec<Item> {
        let mut all: Vec<(&PathBuf, &karet_core::Diagnostic)> = self
            .problems
            .iter()
            .flat_map(|(path, diags)| {
                diags
                    .iter()
                    .filter(|diag| counts(diag.severity))
                    .map(move |diag| (path, diag))
            })
            .collect();
        all.sort_by(|(path_a, diag_a), (path_b, diag_b)| {
            diag_a
                .severity
                .cmp(&diag_b.severity)
                .then(path_a.cmp(path_b))
                .then(diag_a.range.start.line.cmp(&diag_b.range.start.line))
        });
        all.into_iter()
            .map(|(path, diag)| Item {
                label: format!(
                    "{}: {}",
                    severity_word(diag.severity),
                    one_line(&diag.message)
                ),
                detail: format!("{}:{}", self.rel(path).display(), diag.range.start.line + 1),
                target: Target::Location(path.clone(), diag.range.start),
                kind: Kind::Problem,
                keywords: "",
            })
            .collect()
    }

    pub(super) fn on_lsp_ready(&mut self, key: &'static str, client: Arc<LspClient>) {
        for doc in &mut self.docs {
            if let Some(lang) = doc.lsp_lang.filter(|lang| lsp::Hub::is_key(lang, key)) {
                doc.lsp_synced = doc.buffer.version();
                lsp::did_open(
                    (key, client.clone()),
                    doc.path.clone(),
                    lang,
                    doc.lsp_version,
                    doc.buffer.text(),
                    self.tx.clone(),
                );
            }
        }
        self.lsp.ready(key, client);
    }

    /// Send full text for every document edited since the last sync.
    pub(super) fn sync_lsp(&mut self) {
        for doc in &mut self.docs {
            if doc.buffer.version() == doc.lsp_synced {
                continue;
            }
            let servers = doc
                .lsp_lang
                .map_or_else(Vec::new, |lang| self.lsp.running(lang));
            if servers.is_empty() {
                continue;
            }
            doc.lsp_version += 1;
            doc.lsp_synced = doc.buffer.version();
            let text = doc.buffer.text();
            for server in servers {
                lsp::did_change(
                    server,
                    doc.path.clone(),
                    doc.lsp_version,
                    text.clone(),
                    self.tx.clone(),
                );
            }
        }
    }

    /// Look up the type of the name the caret has rested on for a tick.
    pub(super) fn auto_hover(&mut self) {
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

    pub(super) fn request_hover(&mut self, popup: bool) {
        self.sync_lsp();
        let Some(doc) = self.cur() else {
            return;
        };
        let Some(client) = doc.lsp_lang.and_then(|lang| self.lsp.client(lang)) else {
            if popup {
                self.status = "No language server for this file".into();
            }
            return;
        };
        let (path, caret, caret16) = (doc.path.clone(), doc.caret(), doc.caret_utf16());
        self.hover_asked = Some((path.clone(), caret));
        lsp::hover(client, path, caret, caret16, popup, self.tx.clone());
    }

    pub(super) fn request_nav(&mut self, nav: Nav) {
        self.sync_lsp();
        let Some(doc) = self.cur() else {
            return;
        };
        // Style variables are found by searching the project, no server needed.
        let caret = doc.caret();
        let line = doc.buffer.line(caret.line as usize).unwrap_or_default();
        if let Some(name) = crate::stylenav::variable_at(&doc.path, &line, caret.col as usize) {
            let open = self
                .docs
                .iter()
                .map(|doc| (doc.path.clone(), doc.buffer.text()))
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

    pub(super) fn on_locations(&mut self, nav: Nav, locs: Vec<Location>) {
        self.status.clear();
        let here = self
            .docs
            .get(self.active)
            .map(|doc| (doc.path.clone(), doc.caret_utf16()));
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
                self.jump(&only.path, only.range.start);
            }
            _ => {
                let items = locs
                    .into_iter()
                    .map(|loc| {
                        let (label, detail) = lsp::describe(&loc, &self.root);
                        Item {
                            label,
                            detail,
                            target: Target::Location(loc.path, loc.range.start),
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

    pub(super) fn request_rename(&mut self, path: PathBuf, pos: LineCol, name: String) {
        let Some(client) = self
            .docs
            .get(self.active)
            .and_then(|doc| doc.lsp_lang)
            .and_then(|lang| self.lsp.client(lang))
        else {
            self.status = "No language server for rename".into();
            return;
        };
        self.sync_lsp();
        lsp::rename(client, path, pos, name, self.tx.clone());
    }

    /// Reformat the current file through its language servers.
    pub(super) fn reformat(&mut self) {
        // The server must see exactly the text it formats.
        self.sync_lsp();
        let Some(doc) = self.cur() else { return };
        let Some(lang) = doc.lsp_lang else {
            self.status = "No formatter for this file type".into();
            return;
        };
        let servers = self.lsp.running(lang);
        if servers.is_empty() {
            self.status = "The language server is not running yet".into();
            return;
        }
        let indentation = karet_lsp::Indentation {
            tab_size: 4,
            insert_spaces: !doc.buffer.text().lines().any(|line| line.starts_with('\t')),
        };
        lsp::format(
            servers,
            doc.path.clone(),
            doc.buffer.version(),
            indentation,
            self.tx.clone(),
        );
        self.status = "Reformatting...".into();
    }

    pub(super) fn on_formatted(
        &mut self,
        path: &Path,
        version: u64,
        edits: Vec<karet_core::TextEdit>,
    ) {
        let Some(doc) = self.docs.iter_mut().find(|doc| doc.path == path) else {
            return;
        };
        if doc.buffer.version() != version {
            self.status = "Not reformatted: the file changed meanwhile; try again".into();
            return;
        }
        if edits.is_empty() {
            self.status = "Already formatted".into();
            return;
        }
        let edits: Vec<TextEdit> = edits
            .into_iter()
            .map(|edit| TextEdit {
                range: doc.range_from_utf16(edit.range),
                new_text: edit.new_text,
            })
            .collect();
        self.status = if doc.apply_edits(edits) {
            "Reformatted; Ctrl+Z undoes it".into()
        } else {
            "Reformat failed: the edits did not apply".into()
        };
    }

    pub(super) fn on_rename(&mut self, edit: lsp::Edit) {
        let here = self.here();
        let mut batches = edit.changes;
        let mut skipped = 0;
        for change in edit.document_changes {
            match change {
                karet_core::DocumentChange::Edit(doc_edit) => {
                    batches.push((doc_edit.path, doc_edit.edits));
                }
                _ => skipped += 1,
            }
        }
        let mut files = 0;
        for (path, edits) in batches {
            self.open(&path, None);
            if let Some(doc) = self.doc() {
                let edits: Vec<TextEdit> = edits
                    .into_iter()
                    .map(|edit| TextEdit {
                        range: doc.range_from_utf16(edit.range),
                        new_text: edit.new_text,
                    })
                    .collect();
                if doc.apply_edits(edits) {
                    files += 1;
                }
            }
        }
        if let Some((path, _)) = here
            && let Some(idx) = self.docs.iter().position(|doc| doc.path == path)
        {
            self.active = idx;
        }
        let note = if skipped > 0 {
            format!(" ({skipped} file create/rename/delete ops skipped)")
        } else {
            String::new()
        };
        self.status = format!("Renamed in {files} file(s); save with Ctrl+S{note}");
    }
}

/// Problem messages at the end of their line: errors everywhere, and whatever
/// is on the caret's line, so you can read what an underline means.
pub(super) fn problem_texts(doc: &Doc) -> Vec<VirtualText> {
    let caret_line = doc.caret().line;
    let mut best: std::collections::BTreeMap<u32, &karet_core::Diagnostic> =
        std::collections::BTreeMap::new();
    for diag in &doc.diagnostics {
        let line = diag.range.start.line;
        if diag.severity != Severity::Error && line != caret_line {
            continue;
        }
        // The most severe diagnostic on a line wins (Error sorts first).
        if best
            .get(&line)
            .is_none_or(|kept| diag.severity < kept.severity)
        {
            best.insert(line, diag);
        }
    }
    best.into_iter()
        .map(|(line, diag)| {
            let role = match diag.severity {
                Severity::Error => ThemeRole::DiagnosticError,
                Severity::Warning => ThemeRole::DiagnosticWarning,
                Severity::Information => ThemeRole::DiagnosticInfo,
                _ => ThemeRole::DiagnosticHint,
            };
            VirtualText::end_of_line(line, format!("   {}", one_line(&diag.message)), role, 0)
        })
        .collect()
}

/// One line for the bottom bar from hover markdown: the first line of the first
/// code block (else the first line), with the name prefixed when it is only a type.
pub(super) fn hover_summary(text: &str, word: Option<&str>) -> String {
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
        .or_else(|| text.lines().map(str::trim).find(|line| !line.is_empty()))
        .unwrap_or("");
    match word {
        Some(name) if !line.contains(name) => format!("{name}: {line}"),
        _ => line.to_string(),
    }
}

/// Severities that count as problems (hints are too quiet to step through).
pub(super) fn counts(severity: Severity) -> bool {
    severity <= Severity::Information
}

pub(super) fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        _ => "info",
    }
}

/// A multi-line diagnostic on one line: "mismatched types: expected u32, found &str".
pub(super) fn one_line(message: &str) -> String {
    let mut parts = message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = parts.next().unwrap_or("").to_string();
    let rest: Vec<&str> = parts.collect();
    if rest.is_empty() {
        first
    } else {
        format!("{first}: {}", rest.join("; "))
    }
}

pub(super) fn contains(range: Range, pos: LineCol) -> bool {
    let point = (pos.line, pos.col);
    (range.start.line, range.start.col) <= point && point <= (range.end.line, range.end.col)
}
