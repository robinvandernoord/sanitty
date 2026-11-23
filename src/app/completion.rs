//! Code completion: asking the language server, narrowing as you type, and
//! accepting an item. The popup itself lives in `crate::complete`.

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use karet_core::CompletionItem;
use karet_core::CompletionList;
use karet_core::CompletionTrigger;
use karet_core::LineCol;
use karet_core::ServerFeature;
use karet_core::TextEdit;

use crate::complete::Completion;
use crate::complete::is_word_char;
use crate::complete::plan_accept;
use crate::doc::Doc;
use crate::keymap::Action;
use crate::lsp;

use super::App;
use super::Overlay;

/// How far PageUp/PageDown move the selection.
const PAGE: isize = 10;

impl App {
    /// Alt+J: ask the file's language server what can go at the caret.
    pub(super) fn start_completion(&mut self) {
        // The server must see the text as it is now.
        self.sync_lsp();
        let Some(doc) = self.cur() else { return };
        let Some(lang) = doc.lsp_lang else {
            self.status = "No code completion for this file type".into();
            return;
        };
        let servers: Vec<_> = self
            .lsp
            .running(lang)
            .into_iter()
            .filter(|(name, _)| !lsp::is_linter(name))
            .map(|(_, client)| client)
            .collect();
        if servers.is_empty() {
            self.status = "The language server is not running yet".into();
            return;
        }
        let Some(client) = servers
            .into_iter()
            .find(|client| client.supports(ServerFeature::Completion))
        else {
            self.status = "The language server cannot complete code".into();
            return;
        };
        let anchor = word_start(doc);
        let (path, pos16) = (doc.path.clone(), doc.caret_utf16());
        self.completion_gen += 1;
        let generation = self.completion_gen;
        lsp::complete(
            client.clone(),
            path.clone(),
            pos16,
            CompletionTrigger::Invoked,
            generation,
            self.tx.clone(),
        );
        self.completion = Some(Completion::new(generation, path, client, anchor));
    }

    pub(super) fn on_completions(
        &mut self,
        generation: u64,
        answer: Result<CompletionList, String>,
    ) {
        let Some(popup) = &mut self.completion else {
            return;
        };
        if popup.generation != generation {
            return;
        }
        let list = match answer {
            Ok(list) => list,
            Err(err) => {
                self.status = format!("Code completion: {err}");
                self.completion = None;
                return;
            }
        };
        let anchor = popup.anchor;
        let now = self
            .cur()
            .map_or_else(String::new, |doc| prefix(doc, anchor));
        let Some(popup) = &mut self.completion else {
            return;
        };
        popup.set_items(list.items, list.incomplete, &now);
        if popup.filtered.is_empty() {
            self.status = "No suggestions".into();
            self.completion = None;
            return;
        }
        self.resolve_selected();
    }

    pub(super) fn on_completion_resolved(
        &mut self,
        generation: u64,
        index: usize,
        resolved: Option<Box<CompletionItem>>,
    ) {
        let Some(popup) = &mut self.completion else {
            return;
        };
        if popup.generation != generation {
            return;
        }
        let Some(item) = popup.items.get_mut(index) else {
            return;
        };
        match resolved {
            Some(full) => item.adopt_resolved(*full),
            // Nothing more to get; stop waiting for it.
            None => item.payload = None,
        }
        if popup.accepting == Some(index) {
            self.accept_completion();
        }
    }

    /// A key while the popup is open. Returns whether the popup used it; the
    /// rest (typing, Tab) goes on to the editor as usual.
    pub(super) fn completion_key(&mut self, key: &KeyEvent, action: Option<Action>) -> bool {
        if self.completion.is_none() || !matches!(self.overlay, Overlay::None) {
            return false;
        }
        if action == Some(Action::Complete) {
            return false;
        }
        if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return false;
        }
        let Some(popup) = &mut self.completion else {
            return false;
        };
        match key.code {
            KeyCode::Up => popup.step(-1),
            KeyCode::Down => popup.step(1),
            KeyCode::PageUp => popup.step(-PAGE),
            KeyCode::PageDown => popup.step(PAGE),
            KeyCode::Enter => {
                self.accept_completion();
                return true;
            }
            KeyCode::Esc => {
                self.completion = None;
                return true;
            }
            // Tab keeps indenting; the popup gets out of the way.
            KeyCode::Tab => {
                self.completion = None;
                return false;
            }
            _ => return false,
        }
        self.resolve_selected();
        true
    }

    /// After every key: close the popup once the caret left the word, else
    /// narrow it to what is typed now.
    pub(super) fn refresh_completion(&mut self) {
        let Some(popup) = &self.completion else {
            return;
        };
        let keep = matches!(self.overlay, Overlay::None)
            && self.cur().is_some_and(|doc| {
                let caret = doc.caret();
                doc.path == popup.path
                    && caret.line == popup.anchor.line
                    && caret.col >= popup.anchor.col
                    && prefix(doc, popup.anchor).chars().all(is_word_char)
            });
        if !keep {
            self.completion = None;
            return;
        }
        // The answer is still on its way; it gets filtered when it arrives.
        if !popup.loaded {
            return;
        }
        let Some(doc) = self.cur() else { return };
        let now = prefix(doc, popup.anchor);
        if now == popup.prefix {
            return;
        }
        // A list the server cut short is asked for again with the longer word.
        if popup.incomplete {
            self.sync_lsp();
            let Some(doc) = self.cur() else { return };
            let pos16 = doc.caret_utf16();
            self.completion_gen += 1;
            let generation = self.completion_gen;
            let Some(popup) = &mut self.completion else {
                return;
            };
            popup.generation = generation;
            lsp::complete(
                popup.client.clone(),
                popup.path.clone(),
                pos16,
                CompletionTrigger::Incomplete,
                generation,
                self.tx.clone(),
            );
        }
        let Some(popup) = &mut self.completion else {
            return;
        };
        popup.refilter(&now);
        if popup.filtered.is_empty() && !popup.incomplete {
            self.completion = None;
            return;
        }
        self.resolve_selected();
    }

    /// Ask the server to fill in the selected item (its docs, an import it
    /// needs) if it left that out of the list.
    fn resolve_selected(&mut self) {
        let Some(popup) = &mut self.completion else {
            return;
        };
        let Some(idx) = popup.current() else { return };
        let Some(item) = popup.items.get(idx) else {
            return;
        };
        if item.payload.is_none() || popup.resolving.contains(&idx) {
            return;
        }
        let item = item.clone();
        popup.resolving.push(idx);
        lsp::resolve_completion(
            popup.client.clone(),
            item,
            popup.generation,
            idx,
            self.tx.clone(),
        );
    }

    /// Enter: put the selected item in, replacing the whole word around the
    /// caret, plus any import it needs, as one undo step.
    fn accept_completion(&mut self) {
        let Some(popup) = &mut self.completion else {
            return;
        };
        let Some(idx) = popup.current() else { return };
        // The server may still owe the import this item needs.
        if popup
            .items
            .get(idx)
            .is_some_and(|item| item.payload.is_some())
        {
            popup.accepting = Some(idx);
            self.resolve_selected();
            return;
        }
        let Some(popup) = self.completion.take() else {
            return;
        };
        let Some(item) = popup.items.get(idx) else {
            return;
        };
        let Some(doc) = self.cur_mut() else { return };
        let caret = doc.caret();
        let start = item
            .edit
            .as_ref()
            .map(|edit| doc.range_from_utf16(edit.range).start)
            .filter(|pos| pos.line == caret.line && pos.col <= caret.col)
            .unwrap_or(popup.anchor);
        let end = word_end(doc);
        let additional: Vec<TextEdit> = item
            .additional_edits
            .iter()
            .map(|edit| TextEdit {
                range: doc.range_from_utf16(edit.range),
                new_text: edit.new_text.clone(),
            })
            .collect();
        let added = additional
            .iter()
            .map(|edit| edit.new_text.trim())
            .find(|text| !text.is_empty())
            .map(|text| text.lines().next().unwrap_or(text).to_string());
        let mut plan = plan_accept(item, start, end, additional);
        plan.edits.sort_by_key(|edit| edit.range.start);
        if !doc.apply_edits(plan.edits) {
            self.status = "Code completion failed: the edit did not apply".into();
            return;
        }
        match plan.select_to {
            Some(select_to) => doc.state.set_selection(&doc.buffer, plan.caret, select_to),
            None => doc.state.place_caret(plan.caret),
        }
        doc.state.reveal(&doc.buffer, plan.caret);
        if let Some(line) = added {
            self.status = format!("Added: {line}");
        }
    }
}

/// Where the word before the caret starts.
fn word_start(doc: &Doc) -> LineCol {
    let caret = doc.caret();
    let line = doc.buffer.line(caret.line as usize).unwrap_or_default();
    let before: Vec<char> = line.chars().take(caret.col as usize).collect();
    let len = before
        .iter()
        .rev()
        .take_while(|chr| is_word_char(**chr))
        .count();
    LineCol::new(caret.line, caret.col.saturating_sub(to_u32(len)))
}

/// Where the word around the caret ends.
fn word_end(doc: &Doc) -> LineCol {
    let caret = doc.caret();
    let line = doc.buffer.line(caret.line as usize).unwrap_or_default();
    let len = line
        .chars()
        .skip(caret.col as usize)
        .take_while(|chr| is_word_char(*chr))
        .count();
    LineCol::new(caret.line, caret.col.saturating_add(to_u32(len)))
}

/// A char count as `u32`, saturating: no line gets near the limit.
fn to_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The text from `anchor` to the caret.
fn prefix(doc: &Doc, anchor: LineCol) -> String {
    let caret = doc.caret();
    if caret.line != anchor.line || caret.col < anchor.col {
        return String::new();
    }
    let line = doc.buffer.line(caret.line as usize).unwrap_or_default();
    line.chars()
        .skip(anchor.col as usize)
        .take((caret.col - anchor.col) as usize)
        .collect()
}
