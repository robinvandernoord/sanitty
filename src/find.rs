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
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent search toggles read directly by the view; grouping them would change pub fields"
)]
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
        let mut find = Self {
            origin: doc.caret(),
            ..Default::default()
        };
        if let Some(prev) = previous {
            find.query.clone_from(&prev.query);
            find.case = prev.case;
            find.word = prev.word;
            find.regex = prev.regex;
        }
        // A single-line selection seeds the query, as in PyCharm.
        if let Some(sel) = doc.selected_text().filter(|text| !text.contains('\n')) {
            find.query = sel;
            if let Some(range) = doc.state.selection_range() {
                find.origin = range.start;
            }
        }
        if replace {
            find.replace = Some(
                previous
                    .and_then(|prev| prev.replace.clone())
                    .unwrap_or_default(),
            );
        }
        find
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
            .filter(|found| found.end > found.start)
            .take(100_000)
            .map(|found| Range {
                start: doc.buffer.byte_to_line_col(BytePos(found.start)),
                end: doc.buffer.byte_to_line_col(BytePos(found.end)),
            })
            .collect();
        let origin = (self.origin.line, self.origin.col);
        self.current = self
            .matches
            .iter()
            .position(|range| (range.start.line, range.start.col) >= origin)
            .or(if self.matches.is_empty() {
                None
            } else {
                Some(0)
            });
    }

    /// Select the current match in the editor.
    pub fn show(&self, doc: &mut Doc) {
        if let Some(range) = self.current.and_then(|idx| self.matches.get(idx)) {
            doc.goto(range.start);
            doc.state.set_selection(&doc.buffer, range.start, range.end);
        }
    }

    pub fn step(&mut self, doc: &mut Doc, forward: bool) {
        if self.matches.is_empty() {
            return;
        }
        let count = self.matches.len();
        self.current = Some(match self.current {
            Some(idx) if forward => (idx + 1) % count,
            Some(idx) => (idx + count - 1) % count,
            None => 0,
        });
        if let Some(range) = self.current.and_then(|idx| self.matches.get(idx)) {
            self.origin = range.start;
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
        let sel = doc
            .state
            .selection_range()
            .filter(|range| !range.is_empty());
        let (sel_start, sel_end) =
            sel.map_or((doc.caret(), doc.caret()), |range| (range.start, range.end));
        let key = |pos: LineCol| (pos.line, pos.col);
        self.current = if forward {
            self.matches
                .iter()
                .position(|range| key(range.start) >= key(sel_end))
                .or(Some(0))
        } else {
            self.matches
                .iter()
                .rposition(|range| key(range.start) < key(sel_start))
                .or(Some(self.matches.len() - 1))
        };
        self.show(doc);
        true
    }

    fn replace_current(&mut self, doc: &mut Doc) {
        let (Some(idx), Some(with)) = (self.current, self.replace.clone()) else {
            return;
        };
        let Some(range) = self.matches.get(idx).copied() else {
            return;
        };
        // Continue after the replacement, not on it.
        let mut after = range.start;
        let added = u32::try_from(with.chars().count()).unwrap_or(u32::MAX);
        after.col = after.col.saturating_add(added);
        doc.apply_edits(vec![TextEdit {
            range,
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
            .map(|range| TextEdit {
                range: *range,
                new_text: with.clone(),
            })
            .collect();
        let count = edits.len();
        if count > 0 {
            doc.apply_edits(edits);
        }
        self.search(doc);
        count
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
                self.step(doc, !(shift && key.code == KeyCode::Enter));
            }
            KeyCode::Up => self.step(doc, false),
            KeyCode::Tab | KeyCode::BackTab if self.replace.is_some() => {
                self.in_replace = !self.in_replace;
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
                let count = self.replace_all(doc);
                *status = format!("Replaced {count} occurrence(s)");
            }
            KeyCode::Backspace => {
                self.field().pop();
                research = !self.in_replace;
            }
            KeyCode::Char('u') if ctrl => {
                self.field().clear();
                research = !self.in_replace;
            }
            KeyCode::Char(chr) if !ctrl && !alt => {
                self.field().push(chr);
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
            (Some(text), true) => text,
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
            .map(|(idx, range)| {
                let role = if Some(idx) == self.current {
                    ThemeRole::SearchMatchCurrent
                } else {
                    ThemeRole::SearchMatch
                };
                Decoration::new(*range, DecorationKind::TextBackground, Some(role))
            })
            .collect()
    }
}
