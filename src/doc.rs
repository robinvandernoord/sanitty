//! One open file: its text, caret state, syntax highlights and LSP bookkeeping.

use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

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
use karet_syntax::Highlighter;
use karet_syntax::LanguageId;
use karet_syntax::ParserPool;
use karet_syntax::SyntaxTree;
use karet_text::EditContext;
use karet_text::TextBuffer;

pub struct Doc {
    pub path: PathBuf,
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
    highlighter: Option<Highlighter>,
    pool: ParserPool,
}

impl Doc {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let buffer = TextBuffer::load(path)?;
        let lang = syntax_language(path);
        let highlighter = lang.and_then(|l| Highlighter::new(l).ok());
        let mut doc = Self {
            path: path.to_path_buf(),
            lsp_synced: buffer.version(),
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
            highlighter,
            pool: ParserPool::new(),
        };
        doc.rehighlight();
        Ok(doc)
    }

    /// Full reparse and re-highlight. Tree-sitter parses a few thousand lines in
    /// well under a frame, so incremental parsing can wait until it matters.
    fn rehighlight(&mut self) {
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
                .map(|p| Diagnostic {
                    range: Range {
                        start: self.buffer.byte_to_line_col(karet_core::BytePos(p.start)),
                        end: self.buffer.byte_to_line_col(karet_core::BytePos(p.end)),
                    },
                    severity: if p.error {
                        karet_core::Severity::Error
                    } else {
                        karet_core::Severity::Warning
                    },
                    message: p.message.clone(),
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
            && let Some(h) = crate::tshl::highlight(&self.buffer.text(), &self.path)
        {
            self.highlights = Some(h);
            return;
        }
        let (Some(lang), Some(hl)) = (self.lang, self.highlighter.as_ref()) else {
            return;
        };
        let text = self.buffer.text();
        if let Ok(tree) = SyntaxTree::parse(&mut self.pool, lang, &text) {
            self.highlights = hl.highlight(&tree, &text).ok();
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
            .map_or(0, |d| d.as_millis() as u64)
    }

    /// Apply an edit and move the caret where it says.
    pub fn apply(&mut self, edit: Edit, cause: EditCause) -> bool {
        let ctx = EditContext {
            tick_ms: Self::now_ms(),
            cause,
            cursor_before: self.state.cursors().clone(),
        };
        if self.buffer.apply(&edit.change, ctx).is_err() {
            return false;
        }
        self.state.place_caret(edit.caret);
        self.state.reveal(&self.buffer, edit.caret);
        self.rehighlight();
        true
    }

    /// Apply raw edits (e.g. from an LSP rename) whose columns are already in
    /// chars. The caret stays where it was, clamped.
    pub fn apply_edits(&mut self, edits: Vec<TextEdit>) -> bool {
        let change = Change::new(self.buffer.version(), edits);
        let caret = self.caret();
        let ok = self.buffer.apply_simple(&change).is_ok();
        if ok {
            let c = self.buffer.clamp(caret);
            self.state.place_caret(c);
            self.rehighlight();
        }
        ok
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
        let (c, s, v) = (self.caret(), self.selection(), self.buffer.version());
        let edit = if word {
            editing::delete_word_backward(c, s, &self.buffer, v)
        } else {
            editing::backspace(c, s, &self.buffer, v)
        };
        if let Some(e) = edit {
            self.apply(e, EditCause::Delete);
        }
    }

    pub fn delete(&mut self, word: bool) {
        let (c, s, v) = (self.caret(), self.selection(), self.buffer.version());
        let edit = if word {
            editing::delete_word_forward(c, s, &self.buffer, v)
        } else {
            editing::delete_forward(c, s, &self.buffer, v)
        };
        if let Some(e) = edit {
            self.apply(e, EditCause::Delete);
        }
    }

    fn line_len(&self, line: u32) -> u32 {
        self.buffer.line(line as usize).map_or(0, |l| {
            l.trim_end_matches(['\n', '\r']).chars().count() as u32
        })
    }

    pub fn duplicate_line(&mut self) {
        if let Some(sel) = self.selection().filter(|r| !r.is_empty()) {
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
        let last = self.buffer.line_count().saturating_sub(1) as u32;
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
            .min(self.buffer.line_count().saturating_sub(2) as u32);
        edit.caret = LineCol::new(
            if caret.line < last {
                caret.line
            } else {
                target_line
            },
            0,
        );
        self.apply(edit, EditCause::Delete);
        let c = self
            .buffer
            .clamp(LineCol::new(self.caret().line, caret.col));
        self.state.place_caret(c);
    }

    pub fn toggle_comment(&mut self) {
        let Some(prefix) = comment_prefix(&self.path) else {
            return;
        };
        let caret = self.caret();
        let (first, last) = match self.selection().filter(|r| !r.is_empty()) {
            Some(r) => (
                r.start.line,
                if r.end.col == 0 && r.end.line > r.start.line {
                    r.end.line - 1
                } else {
                    r.end.line
                },
            ),
            None => (caret.line, caret.line),
        };
        let lines: Vec<String> = (first..=last)
            .map(|l| self.buffer.line(l as usize).unwrap_or_default())
            .collect();
        let all_commented = lines
            .iter()
            .filter(|l| !l.trim().is_empty())
            .all(|l| l.trim_start().starts_with(prefix));
        let mut edits = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let ln = first + i as u32;
            let indent = line.chars().take_while(|c| *c == ' ' || *c == '\t').count() as u32;
            if all_commented {
                let rest: String = line.chars().skip(indent as usize).collect();
                if let Some(after) = rest.strip_prefix(prefix) {
                    let n = prefix.chars().count() as u32 + u32::from(after.starts_with(' '));
                    edits.push(TextEdit {
                        range: Range {
                            start: LineCol::new(ln, indent),
                            end: LineCol::new(ln, indent + n),
                        },
                        new_text: String::new(),
                    });
                }
            } else if !line.trim().is_empty() {
                edits.push(TextEdit {
                    range: Range {
                        start: LineCol::new(ln, indent),
                        end: LineCol::new(ln, indent),
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
            if let Some(cs) = applied.restored_cursor {
                self.state.set_cursor_state(&self.buffer, cs);
            }
            let c = self.buffer.clamp(self.caret());
            self.state.reveal(&self.buffer, c);
            self.rehighlight();
        }
    }

    pub fn redo(&mut self) {
        if let Some(applied) = self.buffer.redo() {
            if let Some(cs) = applied.restored_cursor {
                self.state.set_cursor_state(&self.buffer, cs);
            }
            let c = self.buffer.clamp(self.caret());
            self.state.place_caret(c);
            self.state.reveal(&self.buffer, c);
            self.rehighlight();
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection()
            .filter(|r| !r.is_empty())
            .and_then(|r| self.buffer.slice_range(r))
    }

    /// The current line including its break, PyCharm-style copy without selection.
    pub fn current_line_text(&self) -> String {
        let mut l = self
            .buffer
            .line(self.caret().line as usize)
            .unwrap_or_default();
        if !l.ends_with('\n') {
            l.push('\n');
        }
        l
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
        if !line_visible(&self.state, pos.line) {
            self.frame_line = Some(pos.line);
        } else {
            self.state.reveal(&self.buffer, pos);
        }
    }

    /// The identifier under the caret, for rename prefill.
    pub fn word_at_caret(&self) -> Option<String> {
        let (a, b) = karet_text::words::identifier_at(&self.buffer, self.caret())?;
        self.buffer.slice_range(Range { start: a, end: b })
    }

    /// Caret converted to the UTF-16 columns LSP speaks.
    /// The table column under the caret, as a status-line label.
    pub fn table_column(&self) -> Option<String> {
        let table = self.table.as_ref()?;
        let c = self.caret();
        let line = self.buffer.line(c.line as usize)?;
        let byte = line
            .char_indices()
            .nth(c.col as usize)
            .map_or(line.len(), |(b, _)| b);
        Some(table.describe(table.column_at(c.line as usize, &line, byte)))
    }

    /// Take diagnostics in LSP's UTF-16 columns.
    pub fn set_diagnostics(&mut self, diags: &[Diagnostic]) {
        self.diagnostics = diags
            .iter()
            .cloned()
            .map(|mut d| {
                d.range = self.range_from_utf16(d.range);
                d
            })
            .collect();
    }

    pub fn caret_utf16(&self) -> LineCol {
        let c = self.caret();
        LineCol::new(c.line, self.buffer.line_col_to_utf16(c))
    }

    pub fn range_from_utf16(&self, r: Range) -> Range {
        Range {
            start: self.buffer.utf16_to_line_col(r.start.line, r.start.col),
            end: self.buffer.utf16_to_line_col(r.end.line, r.end.col),
        }
    }
}

/// The tree-sitter grammar for a path. SCSS and Less use the CSS grammar:
/// their own highlight rules in karet colour almost nothing, while the CSS
/// grammar handles the CSS that most of their lines are.
fn syntax_language(path: &Path) -> Option<LanguageId> {
    match path.extension().and_then(|e| e.to_str()) {
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
    let v = state.viewport();
    v.start.line < v.end.line && v.start.line <= line && line < v.end.line
}
