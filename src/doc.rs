//! One open file: its text, caret state, syntax highlights and LSP bookkeeping.

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::util::to_u32;
use karet_core::AppliedEdit;
use karet_core::Change;
use karet_core::CursorState;
use karet_core::Diagnostic;
use karet_core::EditCause;
use karet_core::Highlights;
use karet_core::LineCol;
use karet_core::Range;
use karet_core::Selection;
use karet_core::TextEdit;
use karet_editor::EditorState;
use karet_editor::editing;
use karet_editor::editing::Edit;
use karet_editor::editing::MultiEdit;
use karet_syntax::LanguageId;
use karet_syntax::LayeredHighlighter;
use karet_syntax::LayeredParser;
use karet_syntax::LayeredTree;
use karet_text::EditContext;
use karet_text::TextBuffer;

pub struct Doc {
    pub path: PathBuf,
    /// The buffer version right after loading: a higher one means edited
    /// (even if saved since).
    pub loaded_version: u64,
    /// Unique for every time a file is opened, so a late answer about an
    /// earlier copy of the same file is recognised.
    pub instance: u64,
    pub buffer: TextBuffer,
    pub state: EditorState,
    pub highlights: Option<Highlights>,
    pub diagnostics: Vec<Diagnostic>,
    /// The LSP language id (`"python"`, `"rust"`, ...), if a server could serve it.
    pub lsp_lang: Option<&'static str>,
    /// Bumped on every change sent to the language server.
    pub lsp_version: i32,
    /// The buffer version last sent to the language server.
    pub lsp_synced: u64,
    /// A jump target to frame on the next render, once the pane height is known.
    pub frame_line: Option<u32>,
    /// Colours from the language server's semantic tokens, as decorations.
    pub semantic: Vec<karet_core::Decoration>,
    /// Colour values (style files): where each starts and its RGBA.
    pub swatches: Vec<(LineCol, [u8; 4])>,
    /// Colours of our own on top of tree-sitter (SCSS/Less variables).
    pub extra_colours: Vec<karet_core::Decoration>,
    /// Set for CSV/TSV files, which get rainbow columns instead of tree-sitter.
    pub table: Option<crate::csv::Table>,
    lang: Option<LanguageId>,
    /// The syntax tree of the current text, kept so an edit reparses only what
    /// it touched: a full parse of a large file takes about a second.
    tree: Option<LayeredTree>,
    parser: LayeredParser,
    highlighter: LayeredHighlighter,
}

/// Counts every file opened, for [`Doc::instance`].
static INSTANCES: AtomicU64 = AtomicU64::new(0);

impl Doc {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let buffer = TextBuffer::load(path)?;
        let lang = syntax_language(path);
        let mut doc = Self {
            path: path.to_path_buf(),
            instance: INSTANCES.fetch_add(1, Ordering::Relaxed),
            lsp_synced: buffer.version(),
            loaded_version: buffer.version(),
            buffer,
            state: EditorState::new(),
            highlights: None,
            diagnostics: Vec::new(),
            lsp_lang: lsp_language(path),
            lsp_version: 1,
            frame_line: None,
            semantic: Vec::new(),
            swatches: Vec::new(),
            extra_colours: Vec::new(),
            table: crate::csv::is_table(path).then(Default::default),
            lang,
            tree: None,
            parser: LayeredParser::new(),
            highlighter: LayeredHighlighter::new(),
        };
        doc.rehighlight(&[]);
        Ok(doc)
    }

    /// Recolour after `edits` (as the buffer reported them; empty for a fresh
    /// start, which parses everything).
    fn rehighlight(&mut self, edits: &[AppliedEdit]) {
        let started = std::time::Instant::now();
        self.rehighlight_inner(edits);
        crate::debug_log(&format!(
            "rehighlight {}: {:?}",
            self.path.display(),
            started.elapsed()
        ));
    }

    fn rehighlight_inner(&mut self, edits: &[AppliedEdit]) {
        self.restyle();
        if let Some(table) = &self.table {
            let text = self.buffer.text();
            // Detect once; a separator that flips mid-edit would repaint everything.
            let sep = if table.separator == 0 {
                crate::csv::detect_separator(&self.path, &text)
            } else {
                table.separator
            };
            let (highlights, table) = crate::csv::highlight(&text, sep);
            self.highlights = Some(highlights);
            self.diagnostics = table
                .problems
                .iter()
                .map(|problem| Diagnostic {
                    range: Range {
                        start: self
                            .buffer
                            .byte_to_line_col(karet_core::BytePos(problem.start)),
                        end: self
                            .buffer
                            .byte_to_line_col(karet_core::BytePos(problem.end)),
                    },
                    severity: if problem.error {
                        karet_core::Severity::Error
                    } else {
                        karet_core::Severity::Warning
                    },
                    message: problem.message.clone(),
                    source: Some("csv".into()),
                    code: None,
                    code_href: None,
                    tags: Vec::new(),
                    related: Vec::new(),
                })
                .collect();
            self.table = Some(table);
            return;
        }
        if crate::tshl::handles(&self.path)
            && let Some(highlights) = crate::tshl::highlight(&self.buffer.text(), &self.path)
        {
            self.highlights = Some(highlights);
            return;
        }
        let Some(lang) = self.lang else {
            return;
        };
        let text = self.buffer.text();
        if let (Some(tree), Some(previous), false) = (
            self.tree.as_mut(),
            self.highlights.as_ref(),
            edits.is_empty(),
        ) && let Ok(changed) = self.parser.reparse_changed(tree, edits, &text)
        {
            self.highlights = Some(
                self.highlighter
                    .rehighlight(tree, &text, previous, edits, &changed),
            );
            return;
        }
        if let Ok(tree) = self.parser.parse(lang, &text) {
            self.highlights = Some(self.highlighter.highlight(&tree, &text));
            self.tree = Some(tree);
        } else {
            self.highlights = None;
            self.tree = None;
        }
    }

    /// Swatches and style variables, recomputed with the highlights.
    fn restyle(&mut self) {
        let wants = crate::colors::wants_swatches(&self.path);
        if !wants {
            return;
        }
        let text = self.buffer.text();
        self.swatches = crate::colors::find(&text);
        self.extra_colours = crate::colors::style_variables(&text, &self.path)
            .into_iter()
            .map(|(start, len)| {
                karet_core::Decoration::new(
                    Range {
                        start,
                        end: LineCol::new(start.line, start.col + len),
                    },
                    karet_core::DecorationKind::TextStyle,
                    None,
                )
                .with_fg(karet_core::StandardToken::Attribute.id())
            })
            .collect();
    }

    pub fn caret(&self) -> LineCol {
        self.state.cursor()
    }

    fn selection(&self) -> Option<Range> {
        self.state.selection_range()
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            })
    }

    /// Apply an edit and move the caret where it says. Other carets (with
    /// several) stay, moved along with the text.
    pub fn apply(&mut self, edit: Edit, cause: EditCause) -> bool {
        let Edit { change, caret } = edit;
        let before = self.state.cursors().clone();
        let ctx = EditContext {
            tick_ms: Self::now_ms(),
            cause,
            cursor_before: before.clone(),
        };
        let Ok(applied) = self.buffer.apply(&change, ctx) else {
            return false;
        };
        if before.selections.len() > 1 {
            self.keep_others(&before, &change.edits, Selection::caret(caret));
        } else {
            self.state.place_caret(caret);
        }
        self.note_carets();
        self.state.reveal(&self.buffer, caret);
        self.rehighlight(&applied.edits);
        true
    }

    /// After an edit at the main caret: the main one goes to `primary`, the
    /// others move with the text the edit changed.
    fn keep_others(&mut self, before: &CursorState, edits: &[TextEdit], primary: Selection) {
        let selections = before
            .selections
            .iter()
            .enumerate()
            .map(|(idx, selection)| {
                if idx == before.primary {
                    primary
                } else {
                    editing::map_selection(*selection, edits)
                }
            })
            .collect();
        self.state.set_cursor_state(
            &self.buffer,
            CursorState {
                selections,
                primary: before.primary,
            },
        );
    }

    /// Whether there is more than one caret.
    pub fn multi(&self) -> bool {
        self.state.has_multiple_cursors()
    }

    /// With several carets: one edit per caret (from `build`, given the
    /// caret's index, its selection and the buffer version), applied as one
    /// undo step. A caret whose `build` gives `None` stays where it is.
    fn edit_each(
        &mut self,
        cause: EditCause,
        build: impl Fn(&Self, usize, Selection, u64) -> Option<Edit>,
    ) -> bool {
        let base = self.buffer.version();
        let cursors = self.state.cursors().clone();
        let items: Vec<(Selection, Option<Edit>)> = cursors
            .selections
            .iter()
            .enumerate()
            .map(|(idx, selection)| (*selection, build(self, idx, *selection, base)))
            .collect();
        match editing::compose(base, cursors.primary, &items) {
            Some(multi) => self.apply_multi(multi, cause),
            None => false,
        }
    }

    /// Apply an edit that spans every caret, and take the carets it leaves.
    fn apply_multi(&mut self, multi: MultiEdit, cause: EditCause) -> bool {
        let ctx = EditContext {
            tick_ms: Self::now_ms(),
            cause,
            cursor_before: self.state.cursors().clone(),
        };
        let Ok(applied) = self.buffer.apply(&multi.change, ctx) else {
            return false;
        };
        self.state.set_cursor_state(&self.buffer, multi.cursors);
        self.note_carets();
        self.state.reveal(&self.buffer, self.caret());
        self.rehighlight(&applied.edits);
        true
    }

    /// Paste at every caret: with as many lines as carets, one line each;
    /// otherwise the whole text at each.
    pub fn paste_each(&mut self, text: &str) {
        let carets = self.state.cursors().selections.len();
        let body = text.strip_suffix('\n').unwrap_or(text);
        let lines: Vec<String> = body.split('\n').map(str::to_string).collect();
        if lines.len() == carets {
            self.insert_each(&lines);
        } else {
            self.type_text(text);
        }
    }

    /// Put `texts[i]` at caret `i` (replacing its selection): a paste with as
    /// many lines as there are carets.
    pub fn insert_each(&mut self, texts: &[String]) {
        self.edit_each(EditCause::Paste, |_, idx, selection, base| {
            let text = texts.get(idx)?;
            Some(editing::insert(
                selection.head,
                selected(selection),
                base,
                text,
            ))
        });
    }

    /// Apply raw edits (e.g. from an LSP rename) whose columns are already in
    /// chars. The caret stays where it was, clamped.
    pub fn apply_edits(&mut self, edits: Vec<TextEdit>) -> bool {
        self.apply_edits_placing(edits, None)
    }

    /// Apply edits made at the main caret (a completion), then put the main
    /// caret there: at `head`, selecting from `anchor` when they differ. The
    /// other carets move with the text, and redo puts them all back here.
    pub fn apply_at_main(&mut self, edits: Vec<TextEdit>, anchor: LineCol, head: LineCol) -> bool {
        self.apply_edits_placing(edits, Some(Selection { anchor, head }))
    }

    /// [`apply_edits`](Self::apply_edits), with the main caret going to
    /// `main` when given (else it moves with the text).
    fn apply_edits_placing(&mut self, edits: Vec<TextEdit>, main: Option<Selection>) -> bool {
        let change = Change::new(self.buffer.version(), edits);
        let caret = self.caret();
        let before = self.state.cursors().clone();
        // Undo puts the caret back where it was before these edits.
        let ctx = EditContext {
            tick_ms: Self::now_ms(),
            cause: EditCause::Replace,
            cursor_before: before.clone(),
        };
        let Ok(applied) = self.buffer.apply(&change, ctx) else {
            return false;
        };
        if before.selections.len() > 1 {
            // Every caret moves with the text (a rename, a reformat).
            let primary = main.unwrap_or_else(|| {
                before
                    .selections
                    .get(before.primary)
                    .map_or(Selection::caret(caret), |selection| {
                        editing::map_selection(*selection, &change.edits)
                    })
            });
            self.keep_others(&before, &change.edits, primary);
        } else {
            match main {
                Some(selection) if !selection.is_empty() => {
                    self.state
                        .set_selection(&self.buffer, selection.anchor, selection.head);
                }
                Some(selection) => self.state.place_caret(selection.head),
                None => self.state.place_caret(self.buffer.clamp(caret)),
            }
        }
        // Only now, with every caret in its final place.
        self.note_carets();
        if let Some(selection) = main {
            self.state.reveal(&self.buffer, selection.head);
        }
        self.rehighlight(&applied.edits);
        true
    }

    pub fn type_text(&mut self, text: &str) {
        if self.multi() {
            let cause = if text.chars().count() == 1 {
                EditCause::Type
            } else {
                EditCause::Paste
            };
            self.edit_each(cause, |_, _, selection, base| {
                Some(editing::insert(
                    selection.head,
                    selected(selection),
                    base,
                    text,
                ))
            });
            return;
        }
        let base = self.buffer.version();
        let edit = editing::insert(self.caret(), self.selection(), base, text);
        let cause = if text.chars().count() == 1 {
            EditCause::Type
        } else {
            EditCause::Paste
        };
        self.apply(edit, cause);
    }

    pub fn newline(&mut self) {
        if self.multi() {
            self.edit_each(EditCause::Newline, |doc, _, selection, base| {
                Some(editing::newline(
                    selection.head,
                    selected(selection),
                    &doc.buffer,
                    base,
                ))
            });
            return;
        }
        let edit = editing::newline(
            self.caret(),
            self.selection(),
            &self.buffer,
            self.buffer.version(),
        );
        self.apply(edit, EditCause::Newline);
    }

    pub fn backspace(&mut self, word: bool) {
        if self.multi() {
            self.edit_each(EditCause::Delete, |doc, _, selection, base| {
                let (head, range) = (selection.head, selected(selection));
                if word {
                    editing::delete_word_backward(head, range, &doc.buffer, base)
                } else {
                    editing::backspace(head, range, &doc.buffer, base)
                }
            });
            return;
        }
        let (caret, selection, version) = (self.caret(), self.selection(), self.buffer.version());
        let edit = if word {
            editing::delete_word_backward(caret, selection, &self.buffer, version)
        } else {
            editing::backspace(caret, selection, &self.buffer, version)
        };
        if let Some(edit) = edit {
            self.apply(edit, EditCause::Delete);
        }
    }

    pub fn delete(&mut self, word: bool) {
        if self.multi() {
            self.edit_each(EditCause::Delete, |doc, _, selection, base| {
                let (head, range) = (selection.head, selected(selection));
                if word {
                    editing::delete_word_forward(head, range, &doc.buffer, base)
                } else {
                    editing::delete_forward(head, range, &doc.buffer, base)
                }
            });
            return;
        }
        let (caret, selection, version) = (self.caret(), self.selection(), self.buffer.version());
        let edit = if word {
            editing::delete_word_forward(caret, selection, &self.buffer, version)
        } else {
            editing::delete_forward(caret, selection, &self.buffer, version)
        };
        if let Some(edit) = edit {
            self.apply(edit, EditCause::Delete);
        }
    }

    fn line_len(&self, line: u32) -> u32 {
        self.buffer.line(line as usize).map_or(0, |text| {
            to_u32(text.trim_end_matches(['\n', '\r']).chars().count())
        })
    }

    pub fn duplicate_line(&mut self) {
        if self.multi() {
            self.edit_each(EditCause::Replace, |doc, _, selection, base| {
                Some(doc.duplicate_edit(selection, base))
            });
            return;
        }
        let selection = self.state.cursors().primary();
        let edit = self.duplicate_edit(selection, self.buffer.version());
        self.apply(edit, EditCause::Replace);
    }

    pub fn delete_line(&mut self) {
        if self.multi() {
            self.edit_each(EditCause::Delete, |doc, _, selection, base| {
                Some(doc.delete_line_edit(selection.head, base))
            });
            return;
        }
        let edit = self.delete_line_edit(self.caret(), self.buffer.version());
        self.apply(edit, EditCause::Delete);
    }

    pub fn toggle_comment(&mut self) {
        let Some(prefix) = comment_prefix(&self.path) else {
            return;
        };
        if self.multi() {
            if let Some(multi) = editing::toggle_line_comment(
                &self.buffer,
                self.state.cursors(),
                self.buffer.version(),
                editing::LineComment::Prefix(prefix),
            ) {
                self.apply_multi(multi, EditCause::Replace);
            }
            return;
        }
        let caret = self.caret();
        let (first, last) = match self.selection().filter(|range| !range.is_empty()) {
            Some(range) => (
                range.start.line,
                if range.end.col == 0 && range.end.line > range.start.line {
                    range.end.line - 1
                } else {
                    range.end.line
                },
            ),
            None => (caret.line, caret.line),
        };
        let lines: Vec<String> = (first..=last)
            .map(|line| self.buffer.line(line as usize).unwrap_or_default())
            .collect();
        let all_commented = lines
            .iter()
            .filter(|line| !line.trim().is_empty())
            .all(|line| line.trim_start().starts_with(prefix));
        let mut edits = Vec::new();
        for (line_no, line) in (first..).zip(lines.iter()) {
            let indent = to_u32(
                line.chars()
                    .take_while(|chr| *chr == ' ' || *chr == '\t')
                    .count(),
            );
            if all_commented {
                let rest: String = line.chars().skip(indent as usize).collect();
                if let Some(after) = rest.strip_prefix(prefix) {
                    let removed =
                        to_u32(prefix.chars().count()) + u32::from(after.starts_with(' '));
                    edits.push(TextEdit {
                        range: Range {
                            start: LineCol::new(line_no, indent),
                            end: LineCol::new(line_no, indent + removed),
                        },
                        new_text: String::new(),
                    });
                }
            } else if !line.trim().is_empty() {
                edits.push(TextEdit {
                    range: Range {
                        start: LineCol::new(line_no, indent),
                        end: LineCol::new(line_no, indent),
                    },
                    new_text: format!("{prefix} "),
                });
            }
        }
        if !edits.is_empty() {
            // One line: on to the next, as PyCharm does; a selection keeps
            // the caret. Worked out before the edit, then moved through it,
            // so the caret lands with the edit (and redo puts it back).
            let target = if first == last {
                self.buffer.clamp(LineCol::new(caret.line + 1, caret.col))
            } else {
                caret
            };
            let after = editing::map_position(target, &edits);
            self.apply_at_main(edits, after, after);
        }
    }

    /// Duplicate Line at one caret: a selection is copied after itself, a
    /// bare caret's line below it (the caret going along).
    fn duplicate_edit(&self, selection: Selection, base: u64) -> Edit {
        if let Some(range) = selected(selection) {
            let text = self.buffer.slice_range(range).unwrap_or_default();
            return editing::insert(range.end, None, base, &text);
        }
        let caret = selection.head;
        let line = self.buffer.line(caret.line as usize).unwrap_or_default();
        let text = format!("\n{}", line.trim_end_matches(['\n', '\r']));
        let mut edit = editing::insert(
            LineCol::new(caret.line, self.line_len(caret.line)),
            None,
            base,
            &text,
        );
        edit.caret = LineCol::new(caret.line + 1, caret.col);
        edit
    }

    /// Delete Line at one caret: its line with the break after it (before
    /// it, on the last line).
    fn delete_line_edit(&self, caret: LineCol, base: u64) -> Edit {
        let last = to_u32(self.buffer.line_count().saturating_sub(1));
        let range = if caret.line < last {
            Range {
                start: LineCol::new(caret.line, 0),
                end: LineCol::new(caret.line + 1, 0),
            }
        } else if caret.line > 0 {
            Range {
                start: LineCol::new(caret.line - 1, self.line_len(caret.line - 1)),
                end: LineCol::new(caret.line, self.line_len(caret.line)),
            }
        } else {
            Range {
                start: LineCol::new(0, 0),
                end: LineCol::new(0, self.line_len(0)),
            }
        };
        let mut edit = editing::insert(range.start, Some(range), base, "");
        // The caret keeps its column on the line that takes this one's place
        // (the next, or the previous for the last line), worked out before
        // the edit so redo puts it back there too.
        let beside = if caret.line < last {
            caret.line + 1
        } else {
            caret.line.saturating_sub(1)
        };
        let before = self.buffer.clamp(LineCol::new(beside, caret.col));
        edit.caret = editing::map_position(before, &edit.change.edits);
        edit
    }

    pub fn undo(&mut self) {
        if let Some(applied) = self.buffer.undo() {
            if let Some(cursors) = applied.restored_cursor {
                self.state.set_cursor_state(&self.buffer, cursors);
            }
            let clamped = self.buffer.clamp(self.caret());
            self.state.reveal(&self.buffer, clamped);
            self.rehighlight(&applied.edits);
        }
    }

    /// Redo the last undone step and put every caret back where that step
    /// left it (several carets stay several).
    pub fn redo(&mut self) {
        if let Some(applied) = self.buffer.redo() {
            // Without a recorded state the carets stay, kept inside the text.
            let cursors = applied
                .restored_cursor
                .unwrap_or_else(|| self.state.cursors().clone());
            self.state.set_cursor_state(&self.buffer, cursors);
            self.rehighlight(&applied.edits);
        }
    }

    /// Tell the buffer where the carets are right after an edit, so redoing
    /// it puts them back there (undo already knows where they were before).
    fn note_carets(&mut self) {
        let cursors = self.state.cursors().clone();
        self.buffer.note_cursor_after(&cursors);
    }

    /// The selected text; with several selections, each on its own line.
    pub fn selected_text(&self) -> Option<String> {
        let parts: Vec<String> = self
            .state
            .selection_ranges()
            .into_iter()
            .filter_map(|range| self.buffer.slice_range(range))
            .collect();
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    /// The current line including its break, PyCharm-style copy without selection.
    pub fn current_line_text(&self) -> String {
        let mut line = self
            .buffer
            .line(self.caret().line as usize)
            .unwrap_or_default();
        if !line.ends_with('\n') {
            line.push('\n');
        }
        line
    }

    pub fn cut(&mut self) -> String {
        if self.multi() {
            if let Some(text) = self.selected_text() {
                // Only the selections go; bare carets stay as they are.
                self.edit_each(EditCause::Delete, |_, _, selection, base| {
                    let range = selected(selection)?;
                    Some(editing::insert(range.start, Some(range), base, ""))
                });
                return text;
            }
            // No selection: every caret's line, once each.
            let mut lines: Vec<u32> = self
                .state
                .cursors()
                .selections
                .iter()
                .map(|selection| selection.head.line)
                .collect();
            lines.dedup();
            let text: String = lines
                .iter()
                .map(|line| {
                    let text = self.buffer.line(*line as usize).unwrap_or_default();
                    if text.ends_with('\n') {
                        text
                    } else {
                        format!("{text}\n")
                    }
                })
                .collect();
            self.delete_line();
            return text;
        }
        if let Some(text) = self.selected_text() {
            self.backspace(false);
            text
        } else {
            let text = self.current_line_text();
            self.delete_line();
            text
        }
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        self.buffer.save(&self.path)?;
        Ok(())
    }

    pub fn goto(&mut self, pos: LineCol) {
        let pos = self.buffer.clamp(pos);
        self.state.place_caret(pos);
        if line_visible(&self.state, pos.line) {
            self.state.reveal(&self.buffer, pos);
        } else {
            self.frame_line = Some(pos.line);
        }
    }

    /// The identifier under the caret, for rename prefill.
    pub fn word_at_caret(&self) -> Option<String> {
        let (start, end) = karet_text::words::identifier_at(&self.buffer, self.caret())?;
        self.buffer.slice_range(Range { start, end })
    }

    /// Caret converted to the UTF-16 columns LSP speaks.
    /// The table column under the caret, as a status-line label.
    pub fn table_column(&self) -> Option<String> {
        let table = self.table.as_ref()?;
        let caret = self.caret();
        let line = self.buffer.line(caret.line as usize)?;
        let byte = line
            .char_indices()
            .nth(caret.col as usize)
            .map_or(line.len(), |(offset, _)| offset);
        Some(table.describe(table.column_at(caret.line as usize, &line, byte)))
    }

    /// Take diagnostics in LSP's UTF-16 columns.
    pub fn set_diagnostics(&mut self, diags: &[Diagnostic]) {
        self.diagnostics = diags
            .iter()
            .cloned()
            .map(|mut diagnostic| {
                diagnostic.range = self.range_from_utf16(diagnostic.range);
                diagnostic
            })
            .collect();
    }

    pub fn caret_utf16(&self) -> LineCol {
        let caret = self.caret();
        LineCol::new(caret.line, self.buffer.line_col_to_utf16(caret))
    }

    pub fn range_from_utf16(&self, range: Range) -> Range {
        Range {
            start: self
                .buffer
                .utf16_to_line_col(range.start.line, range.start.col),
            end: self.buffer.utf16_to_line_col(range.end.line, range.end.col),
        }
    }
}

/// Syntax colours for a whole text that is not open as a document (the two
/// sides of a reviewed file): the same highlighting a document gets.
pub fn highlight_text(path: &Path, text: &str) -> Option<Highlights> {
    if crate::tshl::handles(path) {
        return crate::tshl::highlight(text, path);
    }
    let lang = syntax_language(path)?;
    let tree = LayeredParser::new().parse(lang, text).ok()?;
    Some(LayeredHighlighter::new().highlight(&tree, text))
}

/// The tree-sitter grammar for a path. SCSS and Less use the CSS grammar:
/// their own highlight rules in karet colour almost nothing, while the CSS
/// grammar handles the CSS that most of their lines are.
fn syntax_language(path: &Path) -> Option<LanguageId> {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("scss" | "less") => karet_syntax::language_id_from_path(Path::new("x.css")),
        _ => karet_syntax::language_id_from_path(path),
    }
}

/// The LSP language id for a path, by extension.
pub fn lsp_language(path: &Path) -> Option<&'static str> {
    crate::lsp::registry().language(path.extension()?.to_str()?)
}

fn comment_prefix(path: &Path) -> Option<&'static str> {
    Some(match path.extension()?.to_str()? {
        "py" | "pyi" | "sh" | "bash" | "toml" | "yaml" | "yml" | "rb" | "dockerfile" | "conf"
        | "ini" => "#",
        "rs" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" | "go" | "c" | "h" | "cpp" | "java"
        | "css" | "scss" | "json5" => "//",
        "sql" | "lua" => "--",
        _ => return None,
    })
}

/// A selection's range, or `None` for a bare caret.
fn selected(selection: Selection) -> Option<Range> {
    (!selection.is_empty()).then(|| selection.range())
}

fn line_visible(state: &EditorState, line: u32) -> bool {
    let view = state.viewport();
    view.start.line < view.end.line && view.start.line <= line && line < view.end.line
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Typing, deleting, pasting, commenting and cutting at several carets.
    #[test]
    fn edits_at_every_caret() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("sanitty-multi-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("a.py");
        std::fs::write(&path, "one\ntwo\nthree\n")?;
        let mut doc = Doc::open(&path)?;
        doc.goto(LineCol::new(0, 0));
        doc.state.add_caret_below(&doc.buffer);
        doc.state.add_caret_below(&doc.buffer);
        assert!(doc.multi());

        // Typing and Backspace at all three, each one undo step.
        doc.type_text("x");
        assert_eq!(doc.buffer.text(), "xone\nxtwo\nxthree\n");
        doc.backspace(false);
        assert_eq!(doc.buffer.text(), "one\ntwo\nthree\n");
        doc.type_text("-");
        doc.undo();
        assert_eq!(doc.buffer.text(), "one\ntwo\nthree\n");
        assert_eq!(doc.state.cursors().selections.len(), 3);
        // Redo brings the text back and keeps all three carets, after it.
        doc.redo();
        assert_eq!(doc.buffer.text(), "-one\n-two\n-three\n");
        let heads: Vec<LineCol> = doc
            .state
            .cursors()
            .selections
            .iter()
            .map(|selection| selection.head)
            .collect();
        assert_eq!(
            heads,
            [LineCol::new(0, 1), LineCol::new(1, 1), LineCol::new(2, 1)]
        );
        doc.undo();

        // As many lines as carets: one each.
        doc.paste_each("a\nb\nc\n");
        assert_eq!(doc.buffer.text(), "aone\nbtwo\ncthree\n");
        // Otherwise the whole text at each.
        doc.paste_each("#");
        assert_eq!(doc.buffer.text(), "a#one\nb#two\nc#three\n");

        // Comments toggle on every caret's line.
        doc.toggle_comment();
        assert_eq!(doc.buffer.text(), "# a#one\n# b#two\n# c#three\n");
        doc.toggle_comment();

        // Two selections copy as two lines, and cut takes only them.
        doc.goto(LineCol::new(0, 0));
        doc.state.add_next_occurrence(&doc.buffer);
        doc.state.add_next_occurrence(&doc.buffer);
        assert_eq!(doc.selected_text().as_deref(), Some("a"));
        doc.goto(LineCol::new(0, 2));
        doc.state.add_next_occurrence(&doc.buffer); // selects "one"
        doc.state.add_caret_below(&doc.buffer);
        let text = doc.cut();
        assert_eq!(text, "one");
        assert_eq!(doc.buffer.text(), "a#\nb#two\nc#three\n");

        // An edit at the main caret leaves the others in place.
        doc.goto(LineCol::new(1, 0));
        doc.state.add_caret_below(&doc.buffer);
        let base = doc.buffer.version();
        let edit = editing::insert(doc.caret(), None, base, "!");
        doc.apply(edit, EditCause::Type);
        assert_eq!(doc.buffer.text(), "a#\nb#two\n!c#three\n");
        assert_eq!(doc.state.cursors().selections.len(), 2);

        // A completion at the main caret keeps the other caret, and redo
        // puts the main caret back where the completion left it.
        let main = doc.caret();
        let edits = vec![TextEdit {
            range: Range {
                start: main,
                end: main,
            },
            new_text: "done".into(),
        }];
        let after = LineCol::new(main.line, main.col + 4);
        assert!(doc.apply_at_main(edits, after, after));
        assert_eq!(doc.state.cursors().selections.len(), 2);
        assert_eq!(doc.caret(), after);
        doc.undo();
        doc.redo();
        assert_eq!(doc.caret(), after);
        assert_eq!(doc.state.cursors().selections.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// Delete Line and Toggle Comment with one caret: the caret lands where
    /// `PyCharm` puts it, and redo puts it back there.
    #[test]
    fn line_operations_redo_to_the_same_caret() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("sanitty-lines-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("a.py");
        std::fs::write(&path, "first line\nsecond\nthird line\n")?;
        let mut doc = Doc::open(&path)?;
        doc.goto(LineCol::new(0, 8));
        doc.delete_line();
        assert_eq!(doc.buffer.text(), "second\nthird line\n");
        // Column 8 on "second" (6 long) is its end.
        assert_eq!(doc.caret(), LineCol::new(0, 6));
        doc.undo();
        doc.redo();
        assert_eq!(doc.caret(), LineCol::new(0, 6));

        doc.goto(LineCol::new(0, 3));
        doc.toggle_comment();
        assert_eq!(doc.buffer.text(), "# second\nthird line\n");
        assert_eq!(doc.caret(), LineCol::new(1, 3));
        doc.undo();
        doc.redo();
        assert_eq!(doc.caret(), LineCol::new(1, 3));
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }

    /// Undoing several edits applied as one step (a completion with its
    /// import) puts the caret back where it was.
    #[test]
    fn undoing_batch_edits_puts_the_caret_back() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("sanitty-undo-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("a.py");
        std::fs::write(&path, "import os\n\n\ndef f():\n    return Pa\n")?;
        let mut doc = Doc::open(&path)?;
        doc.goto(LineCol::new(4, 13));
        // A completion with an import: two edits, one undo step.
        let edits = vec![
            TextEdit {
                range: Range {
                    start: LineCol::new(0, 0),
                    end: LineCol::new(0, 0),
                },
                new_text: "from pathlib import Path\n".into(),
            },
            TextEdit {
                range: Range {
                    start: LineCol::new(4, 11),
                    end: LineCol::new(4, 13),
                },
                new_text: "Path".into(),
            },
        ];
        assert!(doc.apply_edits(edits));
        doc.undo();
        assert_eq!(
            doc.buffer.text(),
            "import os\n\n\ndef f():\n    return Pa\n"
        );
        assert_eq!(doc.caret(), LineCol::new(4, 13));
        std::fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// Incremental recolouring after edits (and their undo) must match what a
    /// fresh parse of the same text gives.
    #[test]
    fn incremental_highlights_match_a_fresh_parse() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!("sanitty-doc-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("a.py");
        std::fs::write(
            &path,
            "def f(x):\n    \"\"\"Doc.\"\"\"\n    return x + 1\n\nclass A:\n    pass\n",
        )?;
        let mut doc = Doc::open(&path)?;
        doc.goto(LineCol::new(2, 4));
        doc.type_text("y = \"open");
        doc.goto(LineCol::new(5, 8));
        doc.type_text("\n    def g(self): return 2");
        doc.backspace(false);
        doc.undo();

        let fresh_path = dir.join("b.py");
        std::fs::write(&fresh_path, doc.buffer.text())?;
        let fresh = Doc::open(&fresh_path)?;
        let spans = |opened: &Doc| {
            opened
                .highlights
                .as_ref()
                .map(|highlights| highlights.all().to_vec())
        };
        let _ = std::fs::remove_dir_all(&dir);
        let incremental = spans(&doc).ok_or("no highlights after edits")?;
        assert_eq!(Some(incremental), spans(&fresh));
        Ok(())
    }
}
