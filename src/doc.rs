//! One open file: its text, caret state, syntax highlights and LSP bookkeeping.

use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use karet_core::AppliedEdit;
use karet_core::Change;
use karet_core::Diagnostic;
use karet_core::EditCause;
use karet_core::Highlights;
use karet_core::LineCol;
use karet_core::Range;
use karet_core::TextEdit;
use karet_editor::EditorState;
use karet_editor::editing;
use karet_editor::editing::Edit;
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

    /// Apply an edit and move the caret where it says.
    pub fn apply(&mut self, edit: Edit, cause: EditCause) -> bool {
        let Edit { change, caret } = edit;
        let ctx = EditContext {
            tick_ms: Self::now_ms(),
            cause,
            cursor_before: self.state.cursors().clone(),
        };
        let Ok(applied) = self.buffer.apply(&change, ctx) else {
            return false;
        };
        self.state.place_caret(caret);
        self.state.reveal(&self.buffer, caret);
        self.rehighlight(&applied.edits);
        true
    }

    /// Apply raw edits (e.g. from an LSP rename) whose columns are already in
    /// chars. The caret stays where it was, clamped.
    pub fn apply_edits(&mut self, edits: Vec<TextEdit>) -> bool {
        let change = Change::new(self.buffer.version(), edits);
        let caret = self.caret();
        // Undo puts the caret back where it was before these edits.
        let ctx = EditContext {
            tick_ms: Self::now_ms(),
            cause: EditCause::Replace,
            cursor_before: self.state.cursors().clone(),
        };
        let Ok(applied) = self.buffer.apply(&change, ctx) else {
            return false;
        };
        let clamped = self.buffer.clamp(caret);
        self.state.place_caret(clamped);
        self.rehighlight(&applied.edits);
        true
    }

    pub fn type_text(&mut self, text: &str) {
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
        let edit = editing::newline(
            self.caret(),
            self.selection(),
            &self.buffer,
            self.buffer.version(),
        );
        self.apply(edit, EditCause::Newline);
    }

    pub fn backspace(&mut self, word: bool) {
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
        if let Some(sel) = self.selection().filter(|range| !range.is_empty()) {
            let text = self.buffer.slice_range(sel).unwrap_or_default();
            let edit = editing::insert(sel.end, None, self.buffer.version(), &text);
            self.apply(edit, EditCause::Replace);
            return;
        }
        let caret = self.caret();
        let len = self.line_len(caret.line);
        let line = self.buffer.line(caret.line as usize).unwrap_or_default();
        let text = format!("\n{}", line.trim_end_matches(['\n', '\r']));
        let mut edit = editing::insert(
            LineCol::new(caret.line, len),
            None,
            self.buffer.version(),
            &text,
        );
        edit.caret = LineCol::new(caret.line + 1, caret.col);
        self.apply(edit, EditCause::Replace);
    }

    pub fn delete_line(&mut self) {
        let caret = self.caret();
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
        let mut edit = editing::insert(range.start, Some(range), self.buffer.version(), "");
        let target_line = range
            .start
            .line
            .min(to_u32(self.buffer.line_count().saturating_sub(2)));
        edit.caret = LineCol::new(
            if caret.line < last {
                caret.line
            } else {
                target_line
            },
            0,
        );
        self.apply(edit, EditCause::Delete);
        let clamped = self
            .buffer
            .clamp(LineCol::new(self.caret().line, caret.col));
        self.state.place_caret(clamped);
    }

    pub fn toggle_comment(&mut self) {
        let Some(prefix) = comment_prefix(&self.path) else {
            return;
        };
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
            self.apply_edits(edits);
            self.state.place_caret(self.buffer.clamp(LineCol::new(
                caret.line + u32::from(first == last),
                caret.col,
            )));
            if first == last {
                self.state.reveal(&self.buffer, self.caret());
            }
        }
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

    pub fn redo(&mut self) {
        if let Some(applied) = self.buffer.redo() {
            if let Some(cursors) = applied.restored_cursor {
                self.state.set_cursor_state(&self.buffer, cursors);
            }
            let clamped = self.buffer.clamp(self.caret());
            self.state.place_caret(clamped);
            self.state.reveal(&self.buffer, clamped);
            self.rehighlight(&applied.edits);
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection()
            .filter(|range| !range.is_empty())
            .and_then(|range| self.buffer.slice_range(range))
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
    Some(match path.extension()?.to_str()? {
        "py" | "pyi" => "python",
        "rs" => "rust",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        _ => return None,
    })
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

fn line_visible(state: &EditorState, line: u32) -> bool {
    let view = state.viewport();
    view.start.line < view.end.line && view.start.line <= line && line < view.end.line
}

/// A count as a `u32` column or line number, saturating for absurd sizes.
fn to_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

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
