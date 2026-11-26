//! Code completion (Alt+J): the popup's state, how typing narrows it, and what
//! accepting an item does to the text.
//!
//! The popup opens only on the key. Typing keeps going to the editor and
//! narrows the list by the word before the caret; Up/Down select, Enter accepts
//! (replacing the rest of the word after the caret), Esc closes, and anything
//! that moves the caret out of the word closes it too.

use std::path::PathBuf;

use karet_core::CompletionItem;
use karet_core::CompletionKind;
use karet_core::LineCol;
use karet_core::TextEdit;
use nucleo_matcher::Config;
use nucleo_matcher::Matcher;
use nucleo_matcher::Utf32Str;
use nucleo_matcher::pattern::CaseMatching;
use nucleo_matcher::pattern::Normalization;
use nucleo_matcher::pattern::Pattern;
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

use crate::picker::keep_start;
use crate::util::to_u16;
use crate::util::to_u32;

/// Numbers every completion list, for [`Completion::list`].
static LISTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Rows the list shows at most.
const ROWS: usize = 10;

/// The docs panel goes beside the list when at least this many cells are free.
const SIDE_DOCS_MIN: u16 = 30;

/// Widths (chars) of the list's label and detail columns.
#[derive(Clone, Copy)]
struct Columns {
    label: usize,
    detail: usize,
}

pub struct Completion {
    /// Tags requests and answers; an answer for an older request is dropped.
    pub generation: u64,
    /// Which list the rows came from: bumped by every answer, so details
    /// asked for a row of an older list never land on the new one.
    pub list: u64,
    pub path: PathBuf,
    /// The server that answered, asked again to fill in the selected item;
    /// `None` for candidates sanitty made itself (SQL from the schema).
    pub client: Option<crate::lsp::Server>,
    /// Where the word being completed starts (chars). Typing narrows the list
    /// by the text from here to the caret.
    pub anchor: LineCol,
    pub items: Vec<CompletionItem>,
    /// The server has answered; until then typing has nothing to narrow.
    pub loaded: bool,
    /// The server left candidates out, so a longer prefix asks again.
    pub incomplete: bool,
    /// Indices into `items` that match the prefix, best first.
    pub filtered: Vec<usize>,
    /// Position in `filtered`.
    pub selected: usize,
    /// The first visible row.
    scroll: usize,
    /// The prefix `filtered` was made for.
    pub prefix: String,
    /// Items whose details were asked for already.
    pub resolving: Vec<usize>,
    /// Enter was pressed on an item still being filled in: accept it on
    /// arrival, if the text and caret are still as they were then.
    /// (item index, buffer version, caret)
    pub accepting: Option<(usize, u64, LineCol)>,
    matcher: Matcher,
}

impl Completion {
    pub fn new(
        generation: u64,
        path: PathBuf,
        client: Option<crate::lsp::Server>,
        anchor: LineCol,
    ) -> Self {
        Self {
            generation,
            list: 0,
            path,
            client,
            anchor,
            items: Vec::new(),
            loaded: false,
            incomplete: false,
            filtered: Vec::new(),
            selected: 0,
            scroll: 0,
            prefix: String::new(),
            resolving: Vec::new(),
            accepting: None,
            matcher: Matcher::new(Config::DEFAULT),
        }
    }

    /// Take a server's answer and filter it by `prefix`.
    pub fn set_items(&mut self, mut items: Vec<CompletionItem>, incomplete: bool, prefix: &str) {
        // The server's own order: sort text, then label.
        items.sort_by(|left, right| {
            let left_key = left.sort_text.as_deref().unwrap_or(&left.label);
            let right_key = right.sort_text.as_deref().unwrap_or(&right.label);
            left_key.cmp(right_key)
        });
        self.items = items;
        // Unique across popups too: a reply for an earlier popup's row must
        // not land on this one.
        self.list = LISTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.loaded = true;
        self.incomplete = incomplete;
        self.resolving.clear();
        self.accepting = None;
        self.prefix = String::from("\u{0}");
        self.refilter(prefix);
    }

    /// Narrow the list to the items matching `prefix`. The selection resets
    /// only when the prefix changed.
    pub fn refilter(&mut self, prefix: &str) {
        if prefix == self.prefix {
            return;
        }
        self.prefix = prefix.to_string();
        self.selected = 0;
        self.scroll = 0;
        self.filtered = filter(&self.items, prefix, &mut self.matcher);
    }

    /// Select the row showing item `index`, if it is still listed.
    pub fn select_item(&mut self, index: usize) {
        if let Some(row) = self.filtered.iter().position(|&item| item == index) {
            self.selected = row;
        }
    }

    pub fn current(&self) -> Option<usize> {
        self.filtered.get(self.selected).copied()
    }

    /// Move the selection by `delta` rows, wrapping at the ends.
    pub fn step(&mut self, delta: isize) {
        let len = self.filtered.len();
        let Some(last) = len.checked_sub(1) else {
            return;
        };
        let amount = delta.unsigned_abs();
        let back = delta < 0;
        self.selected = if amount > 1 {
            // Page keys stop at the ends instead of wrapping.
            if back {
                self.selected.saturating_sub(amount).min(last)
            } else {
                self.selected.saturating_add(amount).min(last)
            }
        } else {
            let amount = amount % len;
            if back {
                (self.selected + len - amount) % len
            } else {
                (self.selected + amount) % len
            }
        };
    }

    /// Draw the list under the caret cell `caret` (above it near the bottom),
    /// with the selected item's documentation beside it.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        caret: (u16, u16),
        theme: &karet_theme::Theme,
    ) {
        if self.filtered.is_empty() {
            return;
        }
        let rows = self.filtered.len().min(ROWS);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
        let columns = self.columns();
        let (rect, below) = self.list_rect(area, caret, rows, columns);
        self.render_list(frame, rect, rows, columns, theme);
        self.render_docs(frame, area, rect, below, columns.detail, theme);
    }

    /// Width: the widest label and detail among all matches, within limits.
    fn columns(&self) -> Columns {
        let matches = || self.filtered.iter().filter_map(|&idx| self.items.get(idx));
        let label = matches()
            .map(|item| item.label.chars().count())
            .max()
            .unwrap_or(0)
            .min(40);
        let detail = matches()
            .filter_map(|item| item.detail.as_deref())
            .map(|detail| first_line(detail).chars().count())
            .max()
            .unwrap_or(0)
            .min(36);
        Columns { label, detail }
    }

    /// Where the list goes, and whether that is below the caret (else above).
    fn list_rect(
        &self,
        area: Rect,
        caret: (u16, u16),
        rows: usize,
        columns: Columns,
    ) -> (Rect, bool) {
        let detail_width = if columns.detail > 0 {
            2 + columns.detail
        } else {
            0
        };
        let inner_width = 3 + columns.label + detail_width;
        let width = to_u16(inner_width + 2).min(area.width);
        let height = to_u16(rows).saturating_add(2);
        let (caret_x, caret_y) = caret;
        let below = caret_y + 1 + height <= area.y + area.height;
        let y = if below {
            caret_y + 1
        } else {
            caret_y.saturating_sub(height).max(area.y)
        };
        // Line the labels up with the word being completed.
        let word_len = to_u16(self.prefix.chars().count());
        let x = caret_x
            .saturating_sub(word_len.saturating_add(4))
            .max(area.x)
            .min(area.x + area.width.saturating_sub(width));
        let rect = Rect {
            x,
            y,
            width,
            height: height.min(area.height),
        };
        (rect, below)
    }

    /// The list itself, in `rect`, with the selected row highlighted.
    fn render_list(
        &self,
        frame: &mut Frame,
        rect: Rect,
        rows: usize,
        columns: Columns,
        theme: &karet_theme::Theme,
    ) {
        let muted = theme.style(karet_core::ThemeRole::Muted);
        let title = if self.filtered.len() > rows {
            format!(" {}/{} ", self.selected + 1, self.filtered.len())
        } else {
            String::new()
        };
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title_bottom(Line::styled(title, muted).right_aligned())
            .style(crate::styles::surface(theme));
        let room = usize::from(rect.width.saturating_sub(2));
        let lines: Vec<Line> = self
            .filtered
            .iter()
            .enumerate()
            .skip(self.scroll)
            .take(rows)
            .filter_map(|(row, &idx)| Some((row, self.items.get(idx)?)))
            .map(|(row, item)| {
                let line = list_line(item, columns.label, room, muted);
                if row == self.selected {
                    line.style(Style::default().add_modifier(Modifier::REVERSED))
                } else {
                    line
                }
            })
            .collect();
        frame.render_widget(Clear, rect);
        frame.render_widget(Paragraph::new(lines).block(block), rect);
    }

    /// The selected item's documentation: beside the list at `rect`, or
    /// below/above it when there is no room to the side.
    fn render_docs(
        &self,
        frame: &mut Frame,
        area: Rect,
        rect: Rect,
        below: bool,
        detail_width: usize,
        theme: &karet_theme::Theme,
    ) {
        let Some(item) = self.current().and_then(|idx| self.items.get(idx)) else {
            return;
        };
        let text = docs_text(item, detail_width);
        if text.trim().is_empty() {
            return;
        }
        let (docs_area, beside) = docs_area(area, rect, below);
        if docs_area.height < 3 {
            return;
        }
        let max_width = usize::from(70.min(docs_area.width));
        let lines = crate::app::doc_lines(&text, theme, max_width.saturating_sub(4).max(10));
        let docs_width =
            to_u16((lines.iter().map(Line::width).max().unwrap_or(10) + 4).min(max_width));
        let docs_height = to_u16(lines.len()).saturating_add(2).min(docs_area.height);
        let docs_rect = Rect {
            x: if beside {
                docs_area.x
            } else {
                rect.x
                    .min(docs_area.x + docs_area.width.saturating_sub(docs_width))
            },
            y: if beside || below {
                docs_area.y
            } else {
                docs_area.y + docs_area.height - docs_height
            },
            width: docs_width,
            height: docs_height,
        };
        frame.render_widget(Clear, docs_rect);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .padding(ratatui::widgets::Padding::horizontal(1))
            .style(crate::styles::surface(theme));
        frame.render_widget(Paragraph::new(lines).block(block), docs_rect);
    }
}

/// One row of the list: the kind letter, then two columns: the label padded
/// to `label_width`, then the detail (a type or signature), cut to what is
/// left of `room`.
fn list_line(
    item: &CompletionItem,
    label_width: usize,
    room: usize,
    muted: Style,
) -> Line<'static> {
    let label = keep_start(&item.label, label_width.min(room.saturating_sub(3)));
    let label_len = label.chars().count();
    let mut spans = vec![
        Span::styled(format!(" {} ", kind_letter(item.kind)), muted),
        if item.deprecated {
            Span::styled(label, Style::default().add_modifier(Modifier::CROSSED_OUT))
        } else {
            Span::raw(label)
        },
    ];
    let free = room.saturating_sub(3 + label_len);
    let detail = item.detail.as_deref().map_or("", first_line);
    let gap = label_width.saturating_sub(label_len) + 2;
    let text = if detail.is_empty() || free <= gap + 2 {
        " ".repeat(free)
    } else {
        let detail = keep_start(detail, free - gap);
        let tail = free - gap - detail.chars().count();
        format!("{}{detail}{}", " ".repeat(gap), " ".repeat(tail))
    };
    spans.push(Span::styled(text, muted));
    Line::from(spans)
}

/// What the docs panel shows for `item`: its detail in full when the list
/// had to cut it (longer than `detail_width` or on several lines), then its
/// documentation.
fn docs_text(item: &CompletionItem, detail_width: usize) -> String {
    let mut text = String::new();
    if let Some(detail) = item.detail.as_deref()
        && (detail.contains('\n') || detail.chars().count() > detail_width)
    {
        text.push_str("```\n");
        text.push_str(detail);
        text.push_str("\n```\n");
    }
    if let Some(doc) = &item.documentation {
        text.push_str(&doc.value);
    }
    text
}

/// Where the docs panel may go, given the list at `rect` inside `area`, and
/// whether that is beside the list.
fn docs_area(area: Rect, rect: Rect, below: bool) -> (Rect, bool) {
    let right = rect.x + rect.width;
    let side_room = (area.x + area.width).saturating_sub(right);
    if side_room >= SIDE_DOCS_MIN {
        let beside = Rect {
            x: right,
            y: rect.y,
            width: side_room,
            height: (area.y + area.height).saturating_sub(rect.y),
        };
        return (beside, true);
    }
    let docs = if below {
        let top = rect.y + rect.height;
        Rect {
            x: area.x,
            y: top,
            width: area.width,
            height: (area.y + area.height).saturating_sub(top),
        }
    } else {
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: rect.y.saturating_sub(area.y),
        }
    };
    (docs, false)
}

/// The indices of `items` that match `prefix`, best first: items starting
/// with it, then fuzzy matches; equal scores (and an empty prefix) keep the
/// server's order.
fn filter(items: &[CompletionItem], prefix: &str, matcher: &mut Matcher) -> Vec<usize> {
    if prefix.is_empty() {
        return (0..items.len()).collect();
    }
    let pattern = Pattern::parse(prefix, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let lower = prefix.to_lowercase();
    let mut scored: Vec<(bool, u32, usize)> = items
        .iter()
        .enumerate()
        .filter_map(|(idx, item)| {
            let starts = item.filter_key().to_lowercase().starts_with(&lower);
            let key = Utf32Str::new(item.filter_key(), &mut buf);
            pattern
                .score(key, matcher)
                .map(|score| (starts, score, idx))
        })
        .collect();
    // Words that start with what was typed first, then the fuzzy score.
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then(right.1.cmp(&left.1))
            .then(left.2.cmp(&right.2))
    });
    scored.into_iter().map(|(_, _, idx)| idx).collect()
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("").trim()
}

/// One letter per kind, as the popup's first column.
fn kind_letter(kind: CompletionKind) -> char {
    match kind {
        CompletionKind::Method => 'm',
        CompletionKind::Function => 'f',
        CompletionKind::Field | CompletionKind::Property => 'p',
        CompletionKind::Variable => 'v',
        CompletionKind::Class | CompletionKind::Struct => 'c',
        CompletionKind::Interface => 'i',
        CompletionKind::Module => 'M',
        CompletionKind::Keyword => 'k',
        CompletionKind::Snippet => 's',
        CompletionKind::Constant => 'K',
        CompletionKind::Enum => 'e',
        _ => ' ',
    }
}

/// Whether `chr` can be part of the word being completed.
pub fn is_word_char(chr: char) -> bool {
    chr.is_alphanumeric() || chr == '_' || chr == '$'
}

/// What accepting an item does, worked out in the document's current
/// coordinates (chars): the edits to apply as one undo step, and where the
/// caret (and selection, for a placeholder) ends up afterwards.
pub struct Accept {
    pub edits: Vec<TextEdit>,
    pub caret: LineCol,
    pub select_to: Option<LineCol>,
}

/// Plan accepting `item`: its text replaces `start..end` (the word around the
/// caret), and its additional edits (an import) apply too. `additional` must
/// already be in chars. The caret goes to the snippet's first stop, or after
/// the inserted text.
pub fn plan_accept(
    item: &CompletionItem,
    start: LineCol,
    end: LineCol,
    additional: Vec<TextEdit>,
) -> Accept {
    let text = item
        .edit
        .as_ref()
        .map_or(item.insert_text.as_str(), |edit| edit.new_text.as_str())
        .to_string();
    // The first stop a snippet visits: $1 before $0, else the end.
    let stop = item
        .tabstops
        .iter()
        .filter(|tabstop| tabstop.index > 0)
        .min_by_key(|tabstop| tabstop.index)
        .or_else(|| item.tabstops.iter().find(|tabstop| tabstop.index == 0))
        .and_then(|tabstop| tabstop.spans.first().copied());
    let total = to_u32(text.chars().count());
    let (stop_start, stop_end) = stop.unwrap_or((total, total));
    let caret = advance(start, &text, stop_start);
    let select_to = (stop_end > stop_start).then(|| advance(start, &text, stop_end));
    let mut caret = caret;
    let mut select_to = select_to;
    // Additional edits before the completed word move it; apply their shifts
    // from the last to the first, each in coordinates the others leave alone.
    let mut before: Vec<&TextEdit> = additional
        .iter()
        .filter(|edit| edit.range.end <= start)
        .collect();
    before.sort_by_key(|edit| std::cmp::Reverse(edit.range.start));
    for edit in before {
        caret = shift(caret, edit);
        select_to = select_to.map(|pos| shift(pos, edit));
    }
    let mut edits = additional;
    edits.push(TextEdit {
        range: karet_core::Range { start, end },
        new_text: text,
    });
    Accept {
        edits,
        caret,
        select_to,
    }
}

/// The position `chars` characters into `text` inserted at `start`.
fn advance(start: LineCol, text: &str, chars: u32) -> LineCol {
    let mut pos = start;
    for chr in text.chars().take(chars as usize) {
        if chr == '\n' {
            pos = LineCol::new(pos.line + 1, 0);
        } else {
            pos.col += 1;
        }
    }
    pos
}

/// Where `pos` moves when `edit`, which ends at or before it, is applied.
fn shift(pos: LineCol, edit: &TextEdit) -> LineCol {
    let new_end = advance(
        edit.range.start,
        &edit.new_text,
        to_u32(edit.new_text.chars().count()),
    );
    let end = edit.range.end;
    if pos.line == end.line {
        LineCol::new(new_end.line, new_end.col + (pos.col - end.col))
    } else {
        LineCol::new(pos.line - end.line + new_end.line, pos.col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use karet_core::Range;
    use karet_core::Tabstop;

    fn edit(start_line: u32, start_col: u32, end_line: u32, end_col: u32, text: &str) -> TextEdit {
        TextEdit {
            range: Range {
                start: LineCol::new(start_line, start_col),
                end: LineCol::new(end_line, end_col),
            },
            new_text: text.into(),
        }
    }

    #[test]
    fn an_import_above_moves_the_caret_down() -> Result<(), Box<dyn std::error::Error>> {
        let item = CompletionItem::new("Path", CompletionKind::Class);
        let import = edit(0, 0, 0, 0, "from pathlib import Path\n");
        let plan = plan_accept(&item, LineCol::new(5, 4), LineCol::new(5, 6), vec![import]);
        assert_eq!(plan.caret, LineCol::new(6, 8));
        assert_eq!(plan.edits.len(), 2);
        let inserted = plan.edits.get(1).ok_or("no edit for the item")?;
        assert_eq!(inserted.new_text, "Path");
        Ok(())
    }

    #[test]
    fn the_caret_goes_to_the_first_snippet_stop() {
        let mut item = CompletionItem::new("push", CompletionKind::Method);
        item.insert_text = "push(value)".into();
        item.tabstops = vec![
            Tabstop {
                index: 0,
                spans: vec![(11, 11)],
                choices: vec![],
            },
            Tabstop {
                index: 1,
                spans: vec![(5, 10)],
                choices: vec![],
            },
        ];
        let plan = plan_accept(&item, LineCol::new(2, 3), LineCol::new(2, 5), vec![]);
        assert_eq!(plan.caret, LineCol::new(2, 8));
        assert_eq!(plan.select_to, Some(LineCol::new(2, 13)));
    }

    #[test]
    fn typing_narrows_and_keeps_the_server_order_on_ties() {
        let items: Vec<CompletionItem> = ["method", "meta", "make_response", "media_type", "other"]
            .map(|label| CompletionItem::new(label, CompletionKind::Field))
            .to_vec();
        let mut matcher = Matcher::new(Config::DEFAULT);
        assert_eq!(filter(&items, "", &mut matcher), vec![0, 1, 2, 3, 4]);
        assert_eq!(filter(&items, "me", &mut matcher), vec![0, 1, 3, 2]);
        // Fuzzy: a letter at a word start counts most.
        assert_eq!(filter(&items, "mt", &mut matcher).first(), Some(&3));
        // Starting with the typed word beats a better fuzzy score.
        let items: Vec<CompletionItem> = ["split_at_unchecked", "push"]
            .map(|label| CompletionItem::new(label, CompletionKind::Method))
            .to_vec();
        assert_eq!(filter(&items, "pu", &mut matcher), vec![1, 0]);
        assert_eq!(filter(&items, "xyz", &mut matcher), Vec::<usize>::new());
    }
}
