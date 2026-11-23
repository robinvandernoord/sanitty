//! Drawing the screen: top bar, editor, overlays, hint bar and status line.

use karet_core::Severity;
use karet_core::ThemeRole;
use karet_core::VirtualText;
use karet_editor::Editor;
use karet_theme::Theme;
use ratatui::Frame;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

use crate::find::Find;
use crate::keymap::Action;

use super::App;
use super::Overlay;
use super::counts;
use super::one_line;
use super::plural;
use super::problem_texts;
use super::visible;

impl App {
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(
            Block::new().style(crate::styles::surface(&self.theme)),
            area,
        );
        let find_rows = match &self.overlay {
            Overlay::Find(find) => 1 + u16::from(find.replace.is_some()),
            _ => 0,
        };
        let [top, body, find_area, status, hints] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Fill(1),
            Constraint::Length(find_rows),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);
        self.render_top(frame, top);
        self.render_editor(frame, body);
        self.render_status(frame, status);
        self.render_hints(frame, hints);
        self.render_overlay(frame, body, find_area);
    }

    /// The current file with its decorations, or the welcome screen.
    fn render_editor(&mut self, frame: &mut Frame, body: Rect) {
        let mut decorations = match &self.overlay {
            Overlay::Find(find) => find.decorations(),
            _ => Vec::new(),
        };
        if let Some(doc) = self.cur() {
            decorations.extend(doc.semantic.iter().cloned());
            decorations.extend(doc.extra_colours.iter().cloned());
        }
        let mut inline = self
            .docs
            .get(self.active)
            .map_or_else(Vec::new, problem_texts);
        // A swatch cell in front of each colour value; painted in the colour
        // itself once the editor has placed it.
        let swatch_from = inline.len();
        if let Some(doc) = self.docs.get(self.active) {
            inline.extend(doc.swatches.iter().map(|(pos, _)| {
                VirtualText::inline(pos.line, pos.col, "\u{25a0} ", ThemeRole::Muted)
            }));
        }
        let Some(doc) = visible(&mut self.docs, self.active, self.home) else {
            self.render_welcome(frame, body);
            return;
        };
        // Frame a jump target a third of the way down, as PyCharm does.
        if let Some(line) = doc.frame_line.take() {
            doc.state
                .scroll_to_line(line.saturating_sub(u32::from(body.height / 3)));
        }
        let mut editor = Editor::new(&doc.buffer)
            .theme(&self.theme)
            .focused(matches!(self.overlay, Overlay::None))
            .cell_caret(true)
            .diagnostics(&doc.diagnostics)
            .decorations(&decorations)
            .virtual_text(&inline);
        if let Some(highlights) = &doc.highlights {
            editor = editor.highlights(highlights);
        }
        frame.render_stateful_widget(editor, body, &mut doc.state);
        for (idx, (_, [red, green, blue, _])) in doc.swatches.iter().enumerate() {
            if let Some(rect) = doc.state.virtual_text_rect(swatch_from + idx)
                && let Some(cell) = frame.buffer_mut().cell_mut((rect.x, rect.y))
            {
                cell.set_fg(ratatui::style::Color::Rgb(*red, *green, *blue));
            }
        }
    }

    /// Whatever is open on top of the editor: a popup, the find bar, or the
    /// completion list.
    fn render_overlay(&mut self, frame: &mut Frame, body: Rect, find_area: Rect) {
        let theme = &self.theme;
        match &mut self.overlay {
            Overlay::None => {
                if let Some(completion) = self.completion.as_mut()
                    && let Some(caret) = self
                        .docs
                        .get(self.active)
                        .filter(|doc| !self.home && doc.path == completion.path)
                        .and_then(|doc| doc.state.primary_caret_cell(body, &doc.buffer, &[]))
                {
                    completion.render(frame, body, caret, theme);
                }
            }
            Overlay::Picker(picker) => {
                if picker.root.is_none() {
                    picker.root = Some(self.root.clone());
                }
                picker.render(frame, body, theme);
            }
            Overlay::Explorer(explorer) => explorer.render(frame, body, theme),
            Overlay::Rename { text, .. } => render_rename(frame, body, text, theme),
            Overlay::Find(find) => render_find(frame, find_area, find, theme),
            Overlay::AllKeys(all_keys) => all_keys.render(frame, body, theme),
            Overlay::KeyCheck(key_check) => key_check.render(frame, body, theme),
            Overlay::Doc(text) => {
                let caret = self
                    .docs
                    .get(self.active)
                    .and_then(|doc| doc.state.primary_caret_cell(body, &doc.buffer, &[]));
                render_doc(frame, body, caret, text, theme);
            }
        }
    }

    /// The nano-style line of keys for whatever has focus right now.
    pub(super) fn hints(&self) -> Vec<(String, &'static str)> {
        let key = |action: Action| self.display_key(action);
        if self.completion.is_some() && matches!(self.overlay, Overlay::None) {
            return vec![
                hint("Enter", "Insert"),
                hint("Up/Down", "Select"),
                hint("Type", "Narrow"),
                hint("Esc", "Close"),
            ];
        }
        match &self.overlay {
            Overlay::Doc(_) => vec![hint("Esc", "Close"), hint("Any key", "Close and continue")],
            Overlay::KeyCheck(_) => vec![
                hint("Esc", "Did not arrive"),
                hint("Backspace", "Redo previous step"),
            ],
            Overlay::AllKeys(_) => vec![
                hint("Type", "Filter"),
                hint("Enter", "Run"),
                hint("Esc", "Close"),
            ],
            Overlay::Picker(picker) if picker.is_launcher() => vec![
                hint("Enter", "Open"),
                hint("Tab", "Next filter"),
                hint(">", "Actions"),
                hint("@", "Symbols"),
                hint("/", "Text"),
                hint(":", "Line"),
                hint("Esc", "Close"),
            ],
            Overlay::Picker(_) => vec![
                hint("Enter", "Open"),
                hint("Up/Down", "Move"),
                hint("Esc", "Close"),
            ],
            Overlay::Explorer(_) => vec![
                hint("Type", "Search"),
                hint("Up/Down", "Next match"),
                hint("Right", "Expand"),
                hint("Left", "Collapse"),
                hint("Enter", "Open"),
                hint("Esc", "Close"),
            ],
            Overlay::Rename { .. } => vec![hint("Enter", "Rename"), hint("Esc", "Cancel")],
            Overlay::Find(find) => self.find_hints(find),
            Overlay::None if self.cur().is_none() => vec![
                (key(Action::Launcher), "Search everywhere"),
                (key(Action::RecentFiles), "Recent"),
                (key(Action::ToggleExplorer), "Tree"),
                (key(Action::Shell), "Shell"),
                (key(Action::Quit), "Quit"),
                (key(Action::AllKeys), "All keys"),
            ],
            Overlay::None => self.editor_hints(),
        }
    }

    /// Hints while the find bar has focus.
    fn find_hints(&self, find: &Find) -> Vec<(String, &'static str)> {
        let prev = if self.enhanced_keys {
            "Shift+Enter"
        } else {
            "Up"
        };
        let mut shown = if find.in_replace {
            vec![
                hint("Enter", "Replace"),
                hint("Alt+A", "Replace all"),
                hint("Tab", "Find field"),
            ]
        } else {
            vec![hint("Enter", "Next"), hint(prev, "Previous")]
        };
        shown.push(hint("Alt+C", if find.case { "Case: on" } else { "Case" }));
        shown.push(hint("Alt+W", if find.word { "Words: on" } else { "Words" }));
        shown.push(hint(
            "Alt+X",
            if find.regex { "Regex: on" } else { "Regex" },
        ));
        if find.replace.is_none() {
            shown.push((self.display_key(Action::Replace), "Replace"));
        } else if !find.in_replace {
            shown.push(hint("Tab", "Replace field"));
        }
        shown.push(hint("Esc", "Close"));
        shown
    }

    /// Hints while editing a file with nothing on top of it.
    fn editor_hints(&self) -> Vec<(String, &'static str)> {
        let key = |action: Action| self.display_key(action);
        let mut shown = vec![(key(Action::Launcher), "Search")];
        let any_problems = self
            .problems
            .values()
            .any(|diags| diags.iter().any(|diag| counts(diag.severity)))
            || self
                .docs
                .get(self.active)
                .is_some_and(|doc| doc.diagnostics.iter().any(|diag| counts(diag.severity)));
        if any_problems {
            shown.push((key(Action::NextProblem), "Next problem"));
        }
        shown.extend([
            (key(Action::Find), "Find"),
            (key(Action::GotoDefinition), "Definition"),
            (key(Action::FindUsages), "Usages"),
            (key(Action::RecentFiles), "Recent"),
            (key(Action::ToggleExplorer), "Tree"),
            (key(Action::Save), "Save"),
            (key(Action::AllKeys), "All keys"),
        ]);
        shown
    }

    pub(super) fn render_hints(&self, frame: &mut Frame, area: Rect) {
        let base = crate::styles::surface(&self.theme);
        // Plain text, keys in the keyword colour: readable without shouting.
        let key_style = self.theme.token_style(karet_core::TokenId::KEYWORD);
        let label_style = self
            .theme
            .token_style(karet_core::StandardToken::Punctuation.id());
        let mut hints: Vec<(String, &str)> = self
            .hints()
            .into_iter()
            .filter(|(key, _)| !key.is_empty())
            .collect();
        // Each hint renders as "key label" plus a three-space gap, after a one-space margin.
        let width = |shown: &[(String, &str)]| {
            1 + shown
                .iter()
                .map(|(key, label)| key.chars().count() + label.chars().count() + 4)
                .sum::<usize>()
        };
        // Drop hints from the end, but keep the last one (All keys / Close).
        while hints.len() > 1 && width(&hints) > area.width as usize {
            hints.remove(hints.len() - 2);
        }
        let mut spans = vec![Span::raw(" ")];
        for (key, label) in hints {
            spans.push(Span::styled(key, key_style));
            spans.push(Span::styled(format!(" {label}   "), label_style));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)).style(base), area);
    }

    /// The top bar: what you are looking at. File, unsaved mark, its problems,
    /// files elsewhere with problems, language server.
    pub(super) fn render_top(&self, frame: &mut Frame, area: Rect) {
        let style = crate::styles::bar(&self.theme);
        let err = self.theme.style(ThemeRole::DiagnosticError);
        let warn = self.theme.style(ThemeRole::DiagnosticWarning);
        let clean = self.theme.token_style(karet_core::TokenId::STRING);
        let muted = self.theme.style(ThemeRole::Muted);
        let mut left: Vec<Span> = Vec::new();
        let current = self.cur();
        match current {
            Some(doc) => {
                let dirty = if doc.buffer.is_dirty() { " *" } else { "" };
                left.push(Span::styled(
                    format!(" {}{dirty}   ", self.rel(&doc.path).display()),
                    style.add_modifier(Modifier::BOLD),
                ));
                if let Some(column) = doc.table_column() {
                    left.push(Span::styled(format!("{column}   "), muted));
                }
                // Files something checks: a language server, or the CSV checker.
                if doc.lsp_lang.is_some() || doc.table.is_some() {
                    let count_of = |sev| {
                        doc.diagnostics
                            .iter()
                            .filter(|diag| diag.severity == sev)
                            .count()
                    };
                    let (errs, warns) = (count_of(Severity::Error), count_of(Severity::Warning));
                    if errs + warns == 0 {
                        left.push(Span::styled("no problems   ", clean));
                    }
                    if errs > 0 {
                        left.push(Span::styled(
                            format!("{errs} {}  ", plural(errs, "error")),
                            err.add_modifier(Modifier::BOLD),
                        ));
                    }
                    if warns > 0 {
                        left.push(Span::styled(
                            format!("{warns} {}  ", plural(warns, "warning")),
                            warn,
                        ));
                    }
                    left.push(Span::raw(" "));
                }
            }
            None => left.push(Span::styled(
                format!(" {}   ", self.root.display()),
                style.add_modifier(Modifier::BOLD),
            )),
        }
        // Other files: those with errors, else those with warnings.
        let others = |sev: Severity| {
            self.problems
                .iter()
                .filter(|(path, diags)| {
                    current.is_none_or(|doc| doc.path != **path)
                        && diags.iter().any(|diag| diag.severity == sev)
                })
                .count()
        };
        let (other_errs, other_warns) = (others(Severity::Error), others(Severity::Warning));
        if other_errs > 0 {
            left.push(Span::styled("|  ", muted));
            left.push(Span::styled(
                format!(
                    "{other_errs} other {} with errors",
                    plural(other_errs, "file")
                ),
                err,
            ));
        } else if other_warns > 0 {
            left.push(Span::styled("|  ", muted));
            left.push(Span::styled(
                format!(
                    "{other_warns} other {} with warnings",
                    plural(other_warns, "file")
                ),
                warn,
            ));
        }
        let lsp = self.lsp.status();
        let right = if lsp.is_empty() {
            String::new()
        } else {
            format!("{lsp} ")
        };
        let used: usize = left.iter().map(|span| span.content.chars().count()).sum();
        let pad = (area.width as usize).saturating_sub(used + right.chars().count());
        left.push(Span::raw(" ".repeat(pad)));
        left.push(Span::styled(right, muted));
        frame.render_widget(Paragraph::new(Line::from(left)).style(style), area);
    }

    /// The bottom status line: what is happening. The message (or the problem
    /// under the caret) and the caret position.
    pub(super) fn render_status(&self, frame: &mut Frame, area: Rect) {
        let style = crate::styles::bar(&self.theme);
        let mut message = self.status.clone();
        let mut is_type = false;
        let mut right = String::new();
        if let Some(doc) = self.cur() {
            let caret = doc.caret();
            if message.is_empty()
                && let Some(diag) = doc
                    .diagnostics
                    .iter()
                    .filter(|diag| {
                        diag.range.start.line <= caret.line && caret.line <= diag.range.end.line
                    })
                    .min_by_key(|diag| diag.severity)
            {
                message = one_line(&diag.message);
            }
            if message.is_empty()
                && let Some((path, pos, hint)) = &self.type_hint
                && *path == doc.path
                && *pos == caret
            {
                message.clone_from(hint);
                is_type = true;
            }
            right = format!("{}:{} ", caret.line + 1, caret.col + 1);
        }
        let room = (area.width as usize).saturating_sub(right.chars().count() + 1);
        let message: String = format!(" {message}").chars().take(room).collect();
        let pad =
            (area.width as usize).saturating_sub(message.chars().count() + right.chars().count());
        // A type from hover is coloured like code; anything else is plain.
        let mut spans = if is_type {
            crate::sig::spans(&message, &self.theme, style)
        } else {
            vec![Span::raw(message)]
        };
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::raw(right));
        frame.render_widget(Paragraph::new(Line::from(spans)).style(style), area);
    }

    pub(super) fn render_welcome(&self, frame: &mut Frame, area: Rect) {
        let muted = self.theme.style(ThemeRole::Muted);
        let home = dirs::home_dir().unwrap_or_default();
        let root = match self.root.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => self.root.display().to_string(),
        };
        let mut lines = vec![Line::from(format!("sanitty  {root}")), Line::from("")];
        for (action, label) in [
            (Action::Launcher, "Search everywhere"),
            (Action::RecentFiles, "Recent files"),
            (Action::ToggleExplorer, "Project tree"),
            (Action::AllKeys, "All keys"),
            (Action::Shell, "Shell"),
            (Action::Quit, "Quit"),
        ] {
            lines.push(Line::from(vec![
                Span::styled(format!("{:>12}  ", self.display_key(action)), muted),
                Span::raw(format!("{label:<17}")),
            ]));
        }
        let height = to_u16(lines.len());
        let rect = Rect {
            y: area.y + area.height.saturating_sub(height) / 3,
            height: height.min(area.height),
            ..area
        };
        frame.render_widget(Paragraph::new(lines).centered(), rect);
    }

    /// One key per action, for hints and lists: the first binding that this
    /// terminal can actually deliver. Quit shows Ctrl+C, the terminal habit.
    pub(super) fn display_key(&self, action: Action) -> String {
        if action == Action::Quit
            && self
                .keymap
                .chords_for(Action::Copy)
                .iter()
                .any(|chord| chord.to_string() == "Ctrl+C")
        {
            return "Ctrl+C".into();
        }
        // Recent Files preselects the previous file, so Enter switches to it.
        if action == Action::PreviousFile && self.keymap.chords_for(action).is_empty() {
            return format!("{} Enter", self.display_key(Action::RecentFiles));
        }
        let chords = self.keymap.chords_for(action);
        if let Some(chord) = chords
            .iter()
            .find(|chord| self.enhanced_keys || chord.survives_legacy())
        {
            return chord.to_string();
        }
        // No key this terminal can send: show the way in through the launcher.
        let launcher = self
            .keymap
            .chords_for(Action::Launcher)
            .into_iter()
            .find(|chord| self.enhanced_keys || chord.survives_legacy());
        match (launcher, action) {
            (None, _) | (_, Action::Launcher) => {
                chords.first().map_or_else(String::new, ToString::to_string)
            }
            (Some(prefix), Action::GotoSymbol) => format!("{prefix} @"),
            (Some(prefix), Action::SearchInFiles) => format!("{prefix} /"),
            (Some(prefix), Action::GotoLine) => format!("{prefix} :"),
            (Some(prefix), _) => format!("{prefix} >"),
        }
    }
}

/// The quick-documentation popup, under the caret (or above it near the bottom).
pub(super) fn render_doc(
    frame: &mut Frame,
    area: Rect,
    caret: Option<(u16, u16)>,
    text: &str,
    theme: &Theme,
) {
    let base = crate::styles::surface(theme);
    let max_w = 80.min(area.width.saturating_sub(2)) as usize;
    let lines = doc_lines(text, theme, max_w.saturating_sub(4).max(10));
    let width = to_u16((lines.iter().map(Line::width).max().unwrap_or(10) + 4).min(max_w));
    let height = to_u16(lines.len())
        .saturating_add(2)
        .min(area.height.saturating_sub(1))
        .max(3);
    let (caret_x, caret_y) = caret.unwrap_or((area.x + 4, area.y));
    let below = caret_y + 1 + height <= area.y + area.height;
    let y = if below {
        caret_y + 1
    } else {
        caret_y.saturating_sub(height).max(area.y)
    };
    let x = caret_x.min(area.x + area.width.saturating_sub(width));
    let rect = Rect {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .padding(ratatui::widgets::Padding::horizontal(1))
        .style(base);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

/// The rename popup: the new name in a small box near the top.
fn render_rename(frame: &mut Frame, body: Rect, text: &str, theme: &Theme) {
    let width = 60.min(body.width);
    let rect = Rect {
        x: body.x + (body.width - width) / 2,
        y: body.y + 2,
        width,
        height: 3,
    };
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(" Rename (Enter to apply) ")
        .style(crate::styles::surface(theme));
    let inner = block.inner(rect);
    frame.render_widget(Paragraph::new(text).block(block), rect);
    frame.set_cursor_position((
        inner.x.saturating_add(to_u16(text.chars().count())),
        inner.y,
    ));
}

/// The find bar: query (and replace) fields with the match count.
pub(super) fn render_find(frame: &mut Frame, area: Rect, find: &Find, theme: &Theme) {
    let base = crate::styles::bar(theme);
    let muted = theme.style(ThemeRole::Muted);
    let count = if find.invalid {
        "invalid regex".to_string()
    } else if find.query.is_empty() {
        String::new()
    } else if find.matches.is_empty() {
        "no matches".to_string()
    } else {
        format!(
            "{} of {}",
            find.current.map_or(0, |idx| idx + 1),
            find.matches.len()
        )
    };
    let flags: String = [(find.case, "Aa"), (find.word, "W"), (find.regex, ".*")]
        .iter()
        .filter(|(enabled, _)| *enabled)
        .flat_map(|(_, flag)| [" [", *flag, "]"])
        .collect();
    let find_line = Line::from(vec![
        Span::raw(" Find:    "),
        Span::raw(find.query.clone()),
        Span::styled(format!("   {count}{flags}"), muted),
    ]);
    frame.render_widget(
        Paragraph::new(find_line).style(base),
        Rect { height: 1, ..area },
    );
    let (cursor_row, cursor_col) = if let Some(replace) = &find.replace {
        let row = Rect {
            y: area.y + 1,
            height: 1,
            ..area
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(" Replace: "),
                Span::raw(replace.clone()),
            ]))
            .style(base),
            row,
        );
        if find.in_replace {
            (row.y, replace.chars().count())
        } else {
            (area.y, find.query.chars().count())
        }
    } else {
        (area.y, find.query.chars().count())
    };
    frame.set_cursor_position(((area.x + 10).saturating_add(to_u16(cursor_col)), cursor_row));
}

/// One hint: the keys to press and what they do.
fn hint(keys: &str, label: &'static str) -> (String, &'static str) {
    (keys.to_string(), label)
}

/// A count as a terminal coordinate, saturating instead of wrapping.
fn to_u16(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

/// Documentation (hover text, a completion item's docs) as lines at most
/// `inner_w` wide: Markdown lightly, fenced code in the code colour, prose
/// wrapped, rules dropped.
pub(crate) fn doc_lines(text: &str, theme: &Theme, inner_w: usize) -> Vec<Line<'static>> {
    let base = crate::styles::surface(theme);
    let mut depth = 0usize;
    // Markdown, lightly: fenced code in the code colour, prose wrapped, rules dropped.
    let mut lines: Vec<Line> = Vec::new();
    // Plain-text hovers (ty sends these) have no fences: the signature is
    // everything up to the first divider or blank line.
    let fenced = text.contains("```");
    // Code that arrives without any indentation (ty) gets indented by bracket
    // depth; code that is indented already (TypeScript) is left alone.
    let flat_code = {
        let mut inside = !fenced;
        let mut indented = false;
        for line in text.lines() {
            if line.trim_start().starts_with("```") {
                inside = !inside;
            } else if !fenced && line.trim().is_empty() {
                break;
            } else if inside && line.starts_with([' ', '\t']) {
                indented = true;
            }
        }
        !indented
    };
    let mut in_code = !fenced;
    for raw in text.lines() {
        if !fenced && in_code && raw.trim().is_empty() {
            in_code = false;
        }
        if raw.trim_start().starts_with("```") {
            in_code = !in_code;
            // A gap between a code block and the prose after it.
            if !in_code {
                lines.push(Line::from(""));
            }
            continue;
        }
        // A rule (`---`, or a row of dashes ty puts under the signature).
        let trimmed = raw.trim();
        if trimmed.len() >= 3 && trimmed.chars().all(|chr| chr == '-') {
            if !fenced {
                in_code = false;
            }
            lines.push(Line::from(""));
            continue;
        }
        if in_code {
            // ty puts each parameter on its own line without indenting it;
            // indent by bracket depth so the signature reads as one. Fenced
            // code (Markdown hovers) comes indented already.
            let opens_first = raw.trim_start().starts_with([')', ']', '}']);
            let indent = "    ".repeat(depth.saturating_sub(usize::from(opens_first)));
            let line = if flat_code {
                format!("{indent}{}", raw.trim_start())
            } else {
                raw.to_string()
            };
            for chr in raw.chars() {
                match chr {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            let line: String = line.chars().take(inner_w).collect();
            lines.push(Line::from(crate::sig::spans(&line, theme, base)));
            continue;
        }
        // Prose: inline Markdown, wrapped by words.
        let style_of = |style: crate::markdown::Style| match style {
            crate::markdown::Style::Plain => base,
            crate::markdown::Style::Bold => base.add_modifier(Modifier::BOLD),
            crate::markdown::Style::Italic => base.add_modifier(Modifier::ITALIC),
            crate::markdown::Style::Code => theme.token_style(karet_core::TokenId::FUNCTION),
            crate::markdown::Style::Link => base.add_modifier(Modifier::UNDERLINED),
        };
        let mut current: Vec<Span> = Vec::new();
        let mut width = 0usize;
        for (word, style, space) in crate::markdown::words(raw) {
            let word_len = word.chars().count();
            if width > 0 && width + word_len + usize::from(space) > inner_w {
                lines.push(Line::from(std::mem::take(&mut current)));
                width = 0;
            }
            if space && width > 0 {
                current.push(Span::raw(" "));
                width += 1;
            }
            width += word_len;
            current.push(Span::styled(word, style_of(style)));
        }
        lines.push(Line::from(current));
    }
    // No double gaps (a blank line after a fence), none at the end.
    lines.dedup_by(|next, prev| next.width() == 0 && prev.width() == 0);
    while lines.last().is_some_and(|line| line.width() == 0) {
        lines.pop();
    }
    lines
}
