//! Find and replace in the current file: a bar at the bottom of the editor.

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use karet_core::BytePos;
use karet_core::Decoration;
use karet_core::DecorationKind;
use karet_core::LineCol;
use karet_core::Range;
use karet_core::TextEdit;
use karet_core::ThemeRole;
use karet_search::Matcher;
use karet_search::SearchQuery;

use crate::doc::Doc;

#[derive(Default)]
pub struct Find {
    pub query: String,
    /// The replacement text, when the replace field is shown.
    pub replace: Option<String>,
    /// Whether typing goes into the replace field.
    pub in_replace: bool,
    pub case: bool,
    pub word: bool,
    pub regex: bool,
    pub matches: Vec<Range>,
    pub current: Option<usize>,
    pub invalid: bool,
    /// Where the caret was when the bar opened; the first match is searched
    /// from here as you type.
    origin: LineCol,
}

pub enum Outcome {
    /// The key was used by the bar.
    Handled,
    /// Close the bar.
    Close,
    /// Not a find-bar key; let the app handle it.
    Ignored,
}

impl Find {
    pub fn open(doc: &Doc, replace: bool, previous: Option<&Find>) -> Self {
        let mut f = Self {
            origin: doc.caret(),
            ..Default::default()
        };
        if let Some(p) = previous {
            f.query.clone_from(&p.query);
            f.case = p.case;
            f.word = p.word;
            f.regex = p.regex;
        }
        // A single-line selection seeds the query, as in PyCharm.
        if let Some(sel) = doc.selected_text().filter(|t| !t.contains('\n')) {
            f.query = sel;
            if let Some(r) = doc.state.selection_range() {
                f.origin = r.start;
            }
        }
        if replace {
            f.replace = Some(previous.and_then(|p| p.replace.clone()).unwrap_or_default());
        }
        f
    }

    /// Recompute matches for the current query and buffer.
    pub fn search(&mut self, doc: &Doc) {
        self.matches.clear();
        self.current = None;
        self.invalid = false;
        if self.query.is_empty() {
            return;
        }
        let query = SearchQuery {
            pattern: self.query.clone(),
            regex: self.regex,
            // Smart case: an uppercase letter makes it case-sensitive.
            case_sensitive: self.case || self.query.chars().any(char::is_uppercase),
            whole_word: self.word,
            ..Default::default()
        };
        let Ok(matcher) = Matcher::new(&query) else {
            self.invalid = true;
            return;
        };
        let text = doc.buffer.text();
        self.matches = matcher
            .find(&text)
            .into_iter()
            .filter(|m| m.end > m.start)
            .take(100_000)
            .map(|m| Range {
                start: doc.buffer.byte_to_line_col(BytePos(m.start)),
                end: doc.buffer.byte_to_line_col(BytePos(m.end)),
            })
            .collect();
        let origin = (self.origin.line, self.origin.col);
        self.current = self
            .matches
            .iter()
            .position(|r| (r.start.line, r.start.col) >= origin)
            .or(if self.matches.is_empty() {
                None
            } else {
                Some(0)
            });
    }

    /// Select the current match in the editor.
    pub fn show(&self, doc: &mut Doc) {
        if let Some(r) = self.current.and_then(|i| self.matches.get(i)) {
            doc.goto(r.start);
            doc.state.set_selection(&doc.buffer, r.start, r.end);
        }
    }

    pub fn step(&mut self, doc: &mut Doc, forward: bool) {
        if self.matches.is_empty() {
            return;
        }
        let n = self.matches.len();
        self.current = Some(match self.current {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            None => 0,
        });
        if let Some(r) = self.current.and_then(|i| self.matches.get(i)) {
            self.origin = r.start;
        }
        self.show(doc);
    }

    /// F3 / Shift+F3 with the bar closed: the next match after the selection
    /// (or caret), or the one before it.
    pub fn jump_from_caret(&mut self, doc: &mut Doc, forward: bool) -> bool {
        self.search(doc);
        if self.matches.is_empty() {
            return false;
        }
        let sel = doc.state.selection_range().filter(|r| !r.is_empty());
        let (from, to) = sel.map_or((doc.caret(), doc.caret()), |r| (r.start, r.end));
        let key = |p: LineCol| (p.line, p.col);
        self.current = if forward {
            self.matches
                .iter()
                .position(|r| key(r.start) >= key(to))
                .or(Some(0))
        } else {
            self.matches
                .iter()
                .rposition(|r| key(r.start) < key(from))
                .or(Some(self.matches.len() - 1))
        };
        self.show(doc);
        true
    }

    fn replace_current(&mut self, doc: &mut Doc) {
        let (Some(i), Some(with)) = (self.current, self.replace.clone()) else {
            return;
        };
        let Some(r) = self.matches.get(i).copied() else {
            return;
        };
        // Continue after the replacement, not on it.
        let mut after = r.start;
        after.col += with.chars().count() as u32;
        doc.apply_edits(vec![TextEdit {
            range: r,
            new_text: with,
        }]);
        self.origin = after;
        self.search(doc);
        self.show(doc);
    }

    fn replace_all(&mut self, doc: &mut Doc) -> usize {
        let Some(with) = self.replace.clone() else {
            return 0;
        };
        let edits: Vec<TextEdit> = self
            .matches
            .iter()
            .map(|r| TextEdit {
                range: *r,
                new_text: with.clone(),
            })
            .collect();
        let n = edits.len();
        if n > 0 {
            doc.apply_edits(edits);
        }
        self.search(doc);
        n
    }

    /// Handle a key while the bar is open. `status` receives a message.
    pub fn handle_key(&mut self, key: &KeyEvent, doc: &mut Doc, status: &mut String) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let mut research = false;
        match key.code {
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Enter if self.in_replace => self.replace_current(doc),
            KeyCode::Enter | KeyCode::Down => {
                self.step(doc, !(shift && key.code == KeyCode::Enter))
            }
            KeyCode::Up => self.step(doc, false),
            KeyCode::Tab | KeyCode::BackTab if self.replace.is_some() => {
                self.in_replace = !self.in_replace
            }
            KeyCode::Char('c') if alt => {
                self.case = !self.case;
                research = true;
            }
            KeyCode::Char('w') if alt => {
                self.word = !self.word;
                research = true;
            }
            KeyCode::Char('x') if alt => {
                self.regex = !self.regex;
                research = true;
            }
            KeyCode::Char('a') if alt && self.replace.is_some() => {
                let n = self.replace_all(doc);
                *status = format!("Replaced {n} occurrence(s)");
            }
            KeyCode::Backspace => {
                self.field().pop();
                research = !self.in_replace;
            }
            KeyCode::Char('u') if ctrl => {
                self.field().clear();
                research = !self.in_replace;
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.field().push(c);
                research = !self.in_replace;
            }
            _ => return Outcome::Ignored,
        }
        if research {
            self.search(doc);
            self.show(doc);
        }
        Outcome::Handled
    }

    fn field(&mut self) -> &mut String {
        match (&mut self.replace, self.in_replace) {
            (Some(r), true) => r,
            _ => &mut self.query,
        }
    }

    /// Paste into whichever field has focus.
    pub fn paste(&mut self, text: &str, doc: &mut Doc) {
        self.field().push_str(text.lines().next().unwrap_or(""));
        if !self.in_replace {
            self.search(doc);
            self.show(doc);
        }
    }

    /// Match backgrounds for the editor, the current one distinct.
    pub fn decorations(&self) -> Vec<Decoration> {
        self.matches
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let role = if Some(i) == self.current {
                    ThemeRole::SearchMatchCurrent
                } else {
                    ThemeRole::SearchMatch
                };
                Decoration::new(*r, DecorationKind::TextBackground, Some(role))
            })
            .collect()
    }
}
