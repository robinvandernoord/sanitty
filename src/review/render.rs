//! Drawing the Review view: the title, the file list, the diff (split or
//! inline, in syntax colours), the comment box and the source picker, and
//! the hint bar's keys.

use std::fmt::Write as _;

use karet_core::ThemeRole;
use karet_diff::Cell;
use karet_diff::DiffLine;
use karet_diff::FileDiff;
use karet_diff::LineKind;
use karet_theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

use super::Focus;
use super::Review;
use super::SPLIT_MIN_WIDTH;
use super::Target;
use super::colour;
use super::rows::Row;
use super::state::Comment;
use super::state::Kind;
use crate::util::counted as plural;
use crate::util::to_u16;

impl Review {
    /// The keys to show in the hint bar.
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        if let Some(picker) = &self.picker {
            return picker.hints();
        }
        if self.editing.is_some() {
            return vec![("Enter", "Save"), ("Tab", "Type"), ("Esc", "Cancel")];
        }
        let mut hints = vec![("c", "Comment")];
        if self.comment_at(self.cursor).is_some() {
            hints.push(("Enter", "Edit"));
            hints.push(("d", "Delete"));
        }
        hints.extend([
            ("v", "Viewed"),
            ("\\", "Split/inline"),
            ("] [", "File"),
            ("Tab", "Files/diff"),
            ("y", "Copy review"),
        ]);
        hints.push(("x", "Discard review"));
        hints.push(("s", "Source"));
        hints
    }

    // --------------------------------------------------------------- drawing

    pub fn render(&mut self, frame: &mut Frame, top: Rect, body: Rect, theme: &Theme) {
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        self.render_title(frame, top, theme);
        if self.loaded.is_none() {
            let text = match (&self.error, self.loading) {
                (Some(err), _) => format!("  {err}  (s picks a source)"),
                (None, true) => "  Loading...".into(),
                (None, false) => String::new(),
            };
            frame.render_widget(Paragraph::new(Line::styled(text, muted)).style(base), body);
            self.render_picker(frame, body, theme);
            return;
        }
        let names_width = self
            .files()
            .iter()
            .map(|file| file.path.chars().count())
            .max()
            .unwrap_or(10);
        let files_width = to_u16(names_width.saturating_add(14))
            .clamp(20, 44)
            .min(body.width / 3);
        let files_area = Rect {
            width: files_width,
            ..body
        };
        let diff_area = Rect {
            x: body.x.saturating_add(files_width),
            width: body.width.saturating_sub(files_width),
            ..body
        };
        self.render_files(frame, files_area, theme);
        self.render_diff(frame, diff_area, theme);
        self.render_picker(frame, body, theme);
    }

    pub(super) fn render_title(&self, frame: &mut Frame, top: Rect, theme: &Theme) {
        let bar = crate::styles::bar(theme);
        let mut text = String::from(" Review");
        if let Some(loaded) = &self.loaded {
            let (added, removed) = loaded.files.iter().fold((0, 0), |(add, del), file| {
                let (file_add, file_del) = counts(file);
                (add + file_add, del + file_del)
            });
            let _ = write!(
                text,
                "   {}   {}  +{added} -{removed}   {}",
                loaded.detail,
                plural(loaded.files.len(), "file"),
                plural(self.comments().len(), "comment")
            );
            if let Some((source, note)) = &self.notice
                && *source == loaded.source
            {
                let _ = write!(text, "   ({note})");
            }
        }
        if self.loading {
            text.push_str("   (loading...)");
        }
        frame.render_widget(Paragraph::new(text).style(bar), top);
    }

    pub(super) fn render_files(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        // The diff colours are background tints; a mark needs the bright version.
        let added = theme
            .style(ThemeRole::DiffAdded)
            .fg
            .map_or(muted, |colour| Style::default().fg(brighter(colour, 4)));
        let border = if self.focus == Focus::Files {
            base
        } else {
            muted
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(" files ")
            .style(base);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let height = usize::from(inner.height);
        if self.file < self.file_scroll {
            self.file_scroll = self.file;
        } else if height > 0 && self.file >= self.file_scroll + height {
            self.file_scroll = self.file + 1 - height;
        }
        let width = usize::from(inner.width);
        let lines: Vec<Line> = self
            .files()
            .iter()
            .enumerate()
            .skip(self.file_scroll)
            .take(height)
            .map(|(idx, file)| {
                let (add, del) = counts(file);
                let notes = self
                    .comments()
                    .iter()
                    .filter(|comment| comment.path == file.path)
                    .count();
                let mark = if self.is_viewed(file) {
                    "\u{2713}"
                } else {
                    " "
                };
                let stats = if notes > 0 {
                    format!(" +{add} -{del} {notes}")
                } else {
                    format!(" +{add} -{del}")
                };
                let room = width.saturating_sub(stats.chars().count() + 1);
                let name = crate::picker::keep_end(&file.path, room);
                let pad = room.saturating_sub(name.chars().count());
                let line = Line::from(vec![
                    Span::styled(mark, added),
                    Span::raw(name),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(stats, muted),
                ]);
                if idx == self.file {
                    as_selected(line, theme)
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }

    pub(super) fn render_diff(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let split = self.split.unwrap_or(area.width >= SPLIT_MIN_WIDTH);
        if self.rows_split != Some(split) {
            self.rebuild(split);
        }
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        let border = if self.focus == Focus::Diff {
            base
        } else {
            muted
        };
        let title = self
            .current_file()
            .map_or_else(String::new, |file| match &file.old_path {
                Some(old) => format!(" {old} -> {} ", file.path),
                None => format!(" {} ", file.path),
            });
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(title)
            .title(Line::styled(if split { " split " } else { " inline " }, muted).right_aligned())
            .style(base);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let height = usize::from(inner.height);
        self.page = height.saturating_sub(2).max(1);
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if height > 0 && self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
        if self.files().is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled("  Nothing changed", muted)),
                inner,
            );
            return;
        }
        let selected = match self.mark {
            Some(mark) => (mark.min(self.cursor), mark.max(self.cursor)),
            None => (self.cursor, self.cursor),
        };
        let width = usize::from(inner.width);
        let lines: Vec<Line> = self
            .rows
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(height)
            .map(|(idx, row)| {
                let line = self.row_line(row, width, theme);
                let chosen = self.focus == Focus::Diff && idx >= selected.0 && idx <= selected.1;
                if chosen {
                    as_selected(line, theme)
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
        self.render_editing(frame, inner, theme);
    }

    pub(super) fn row_line(&self, row: &Row, width: usize, theme: &Theme) -> Line<'static> {
        let muted = theme.style(ThemeRole::Muted);
        let colours = self
            .loaded
            .as_ref()
            .zip(self.current_file())
            .and_then(|(loaded, file)| loaded.colours.get(&file.path));
        match row {
            Row::Hunk(header) => Line::styled(fit(header, width), muted),
            Row::Note(text) => Line::styled(fit(&format!("  {text}"), width), muted),
            Row::Comment(idx) => {
                let Some(comment) = self.comments().get(*idx) else {
                    return Line::raw("");
                };
                let role = if comment.kind == Kind::Issue {
                    ThemeRole::DiagnosticWarning
                } else {
                    ThemeRole::DiagnosticInfo
                };
                let text = format!(
                    "  \u{25b8} {}  {}  ({})",
                    comment.kind.word(),
                    comment.body,
                    comment.lines_label()
                );
                Line::styled(fit(&text, width), theme.style(role))
            }
            Row::Line(line) => {
                Line::from(diff_spans(&line_cell(line), width, (None, colours), theme))
            }
            Row::Pair { left, right } => {
                let half = width.saturating_sub(1) / 2;
                let pair = match (left, right) {
                    (Some(old), Some(new))
                        if old.kind == LineKind::Remove && new.kind == LineKind::Add =>
                    {
                        Some(karet_diff::compute_highlights(&old.content, &new.content))
                    }
                    _ => None,
                };
                let mut spans = match left {
                    Some(cell) => diff_spans(
                        cell,
                        half,
                        (
                            pair.as_ref().map(|pair| pair.old_segments.as_slice()),
                            colours,
                        ),
                        theme,
                    ),
                    None => vec![Span::raw(" ".repeat(half))],
                };
                spans.push(Span::styled("\u{2502}", muted));
                match right {
                    Some(cell) => spans.extend(diff_spans(
                        cell,
                        width.saturating_sub(half + 1),
                        (
                            pair.as_ref().map(|pair| pair.new_segments.as_slice()),
                            colours,
                        ),
                        theme,
                    )),
                    None => spans.push(Span::raw(" ".repeat(width.saturating_sub(half + 1)))),
                }
                Line::from(spans)
            }
        }
    }

    pub(super) fn render_editing(&self, frame: &mut Frame, inner: Rect, theme: &Theme) {
        let Some(editing) = &self.editing else { return };
        let label = match &editing.target {
            Target::New { start, end, .. } if start == end => format!("line {start}"),
            Target::New { start, end, .. } => format!("lines {start}-{end}"),
            Target::Existing(idx) => self
                .comments()
                .get(*idx)
                .map_or_else(String::new, Comment::lines_label),
        };
        let row = to_u16(self.cursor.saturating_sub(self.scroll).saturating_add(1));
        let height = 3;
        let mut y = inner.y.saturating_add(row);
        if y.saturating_add(height) > inner.y.saturating_add(inner.height) {
            y = inner.y.saturating_add(row.saturating_sub(height + 1));
        }
        let rect = Rect {
            x: inner.x.saturating_add(2),
            y,
            width: inner.width.saturating_sub(4).min(90),
            height,
        };
        let base = crate::styles::surface(theme);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(format!(" {} (Tab: type) - {label} ", editing.kind.word()))
            .title_bottom(" Enter save   Esc cancel ")
            .style(base);
        let text_area = block.inner(rect);
        frame.render_widget(Clear, rect);
        frame.render_widget(block, rect);
        let room = usize::from(text_area.width).saturating_sub(1);
        let shown: String = {
            let count = editing.text.chars().count();
            editing
                .text
                .chars()
                .skip(count.saturating_sub(room))
                .collect()
        };
        let cursor_x = text_area.x.saturating_add(to_u16(shown.chars().count()));
        frame.render_widget(Paragraph::new(shown), text_area);
        frame.set_cursor_position((cursor_x, text_area.y));
    }

    pub(super) fn render_picker(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        if let Some(picker) = &self.picker {
            picker.render(frame, area, theme);
        }
    }
}

/// A diff line as a side-by-side cell.
pub(super) fn line_cell(line: &DiffLine) -> Cell {
    let lineno = match line.kind {
        LineKind::Remove => line.old_lineno,
        _ => line.new_lineno,
    };
    Cell {
        lineno: lineno.unwrap_or(0),
        content: line.content.clone(),
        kind: line.kind,
        hunk_line_idx: 0,
    }
}

/// One side of a row: line number, sign and text, `width` wide, in syntax
/// colours from `colours`, with the changed words marked when `segments`
/// says which they are.
///
/// The theme's diff colours are backgrounds (as in editor themes): a changed
/// line keeps its syntax colours on a tinted background, its changed words on
/// a stronger tint, and the sign in a bright version of the tint.
pub(super) fn diff_spans(
    cell: &Cell,
    width: usize,
    (segments, colours): (Option<&[karet_diff::Segment]>, Option<&colour::FileColours>),
    theme: &Theme,
) -> Vec<Span<'static>> {
    let muted = theme.style(ThemeRole::Muted);
    let surface = crate::styles::surface(theme);
    let tint = match cell.kind {
        LineKind::Add => theme.style(ThemeRole::DiffAdded).fg,
        LineKind::Remove => theme.style(ThemeRole::DiffRemoved).fg,
        LineKind::Context => None,
    };
    let sign = match cell.kind {
        LineKind::Add => '+',
        LineKind::Remove => '-',
        LineKind::Context => ' ',
    };
    let line_style = tint.map_or(surface, |colour| surface.bg(colour));
    let word_bg = tint.map(|colour| brighter(colour, 1));
    let sign_style = tint.map_or(muted, |colour| line_style.fg(brighter(colour, 4)));
    let number = format!("{:>4} ", cell.lineno);
    let room = width.saturating_sub(number.chars().count() + 2);
    let mut spans = vec![
        Span::styled(number, muted),
        Span::styled(format!("{sign} "), sign_style),
    ];
    let runs = colours.map_or(&[][..], |colours| {
        colours.line(cell.kind != LineKind::Remove, cell.lineno)
    });
    // Which characters are changed words, in the line's character order.
    let changed: Vec<bool> = segments.map_or_else(Vec::new, |segments| {
        segments
            .iter()
            .flat_map(|segment| std::iter::repeat_n(segment.changed, segment.text.chars().count()))
            .collect()
    });
    // Each character's style; neighbours with the same style become one span.
    let mut used = 0;
    let mut current: Option<(Style, String)> = None;
    for (idx, (byte, chr)) in cell.content.char_indices().enumerate() {
        let token = runs
            .iter()
            .find(|(start, end, _)| byte >= *start && byte < *end)
            .and_then(|(_, _, token)| theme.token_style(*token).fg);
        let mut style = token.map_or(line_style, |colour| line_style.fg(colour));
        if changed.get(idx).copied().unwrap_or(false)
            && let Some(stronger) = word_bg
        {
            style = style.bg(stronger);
        }
        let text = if chr == '\t' {
            "    ".to_string()
        } else {
            chr.to_string()
        };
        let count = text.chars().count();
        if used + count > room {
            break;
        }
        used += count;
        match &mut current {
            Some((run_style, run)) if *run_style == style => run.push_str(&text),
            _ => {
                if let Some((run_style, run)) = current.take() {
                    spans.push(Span::styled(run, run_style));
                }
                current = Some((style, text));
            }
        }
    }
    if let Some((run_style, run)) = current {
        spans.push(Span::styled(run, run_style));
    }
    spans.push(Span::styled(
        " ".repeat(room.saturating_sub(used)),
        line_style,
    ));
    spans
}

/// `colour` made brighter by `steps` (each roughly doubles its distance from
/// black), for the stronger diff tints.
pub(super) fn brighter(colour: ratatui::style::Color, steps: u8) -> ratatui::style::Color {
    match colour {
        ratatui::style::Color::Rgb(red, green, blue) => {
            let lift = |part: u8| {
                (0..steps).fold(part, |value, _| value.saturating_mul(2).saturating_add(8))
            };
            ratatui::style::Color::Rgb(lift(red), lift(green), lift(blue))
        }
        other => other,
    }
}

/// `line` as the selected row: every part on the selection background (over
/// any diff tint), so the row stands out across its whole width.
pub(super) fn as_selected<'a>(line: Line<'a>, theme: &Theme) -> Line<'a> {
    let Some(colour) = theme.style(ThemeRole::Selection).fg else {
        return line.style(Style::default().add_modifier(Modifier::REVERSED));
    };
    let spans = line
        .spans
        .into_iter()
        .map(|span| {
            let style = span.style.bg(colour);
            span.style(style)
        })
        .collect::<Vec<_>>();
    Line::from(spans).style(line.style.bg(colour))
}

/// `text` cut or padded to `width` characters.
pub(super) fn fit(text: &str, width: usize) -> String {
    let cut: String = text.chars().take(width).collect();
    let pad = width.saturating_sub(cut.chars().count());
    format!("{cut}{}", " ".repeat(pad))
}

/// Added and removed lines in a file.
pub(super) fn counts(file: &FileDiff) -> (usize, usize) {
    let lines = file.hunks.iter().flat_map(|hunk| &hunk.lines);
    lines.fold((0, 0), |(add, del), line| match line.kind {
        LineKind::Add => (add + 1, del),
        LineKind::Remove => (add, del + 1),
        LineKind::Context => (add, del),
    })
}
