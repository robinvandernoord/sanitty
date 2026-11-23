//! A fuzzy picker popup. Plain mode serves Recent Files and usage lists; launcher
//! mode is Search Everywhere: files, actions, symbols and text in one box, with
//! filter tabs and the prefixes `>` actions, `@` symbols, `/` text, `:` line.

use std::path::PathBuf;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use karet_core::LineCol;
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
use ratatui::widgets::List;
use ratatui::widgets::ListItem;
use ratatui::widgets::ListState;
use ratatui::widgets::Paragraph;

use crate::keymap::Action;

/// What picking an item does.
#[derive(Clone, Debug)]
pub enum Target {
    File(PathBuf),
    Location(PathBuf, LineCol),
    Action(Action),
    /// A line (and column) in the active file, both 1-based as typed.
    Line(u32, u32),
}

/// What an item is; also the launcher's filter tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    All,
    File,
    Action,
    Symbol,
    Text,
    Problem,
    Line,
}

impl Kind {
    pub const TABS: [Kind; 6] = [
        Kind::All,
        Kind::File,
        Kind::Action,
        Kind::Symbol,
        Kind::Text,
        Kind::Problem,
    ];

    fn tab_name(self) -> &'static str {
        match self {
            Kind::All => "All",
            Kind::File => "Files",
            Kind::Action => "Actions",
            Kind::Symbol => "Symbols",
            Kind::Text => "Text",
            Kind::Problem => "Problems",
            Kind::Line => "Line",
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Kind::All => "",
            Kind::File => "file",
            Kind::Action => "action",
            Kind::Symbol => "symbol",
            Kind::Text => "text",
            Kind::Problem => "problem",
            Kind::Line => "line",
        }
    }

    /// Results that come from a search already (LSP, text search) keep their
    /// order instead of being fuzzy-filtered again.
    fn prefiltered(self) -> bool {
        matches!(self, Kind::Symbol | Kind::Text)
    }

    /// How many of this kind the All tab shows.
    fn all_cap(self) -> usize {
        match self {
            Kind::File | Kind::Symbol => 8,
            Kind::Action => 6,
            Kind::Text => 12,
            Kind::Problem => 5,
            _ => 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub label: String,
    /// Dimmed text after the label (a directory, a keybinding, a code line).
    pub detail: String,
    pub target: Target,
    pub kind: Kind,
    /// Hidden search words (action synonyms).
    pub keywords: &'static str,
}

pub struct Picker {
    pub title: String,
    items: Vec<Item>,
    query: String,
    /// Indices into `items` with the match positions in `label`, best first.
    filtered: Vec<(usize, Vec<u32>)>,
    list: ListState,
    matcher: Matcher,
    /// Keep the given order when the query is empty (recent files, usages).
    keep_order: bool,
    /// Launcher mode: the selected filter tab, else `None` for a plain picker.
    tab: Option<Kind>,
    /// Set when the query or tab changed, so the app can start async searches.
    changed: bool,
    /// The project root, to show the selected entry's path relative to it.
    pub root: Option<PathBuf>,
    /// Kinds whose background search has not answered yet (text, symbols).
    searching: Vec<Kind>,
}

pub enum Outcome {
    Pending,
    Cancel,
    Pick(Target),
}

impl Picker {
    pub fn new(title: impl Into<String>, items: Vec<Item>, keep_order: bool) -> Self {
        let mut picker = Self {
            title: title.into(),
            items,
            query: String::new(),
            filtered: Vec::new(),
            list: ListState::default(),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            keep_order,
            tab: None,
            changed: false,
            root: None,
            searching: Vec::new(),
        };
        picker.refilter();
        picker
    }

    /// Search Everywhere, opened on `tab` with an optional starting query.
    pub fn launcher(items: Vec<Item>, tab: Kind, query: &str) -> Self {
        let mut picker = Self::new("Search Everywhere", items, true);
        picker.tab = Some(tab);
        picker.query = query.to_string();
        picker.changed = true;
        picker.refilter();
        picker
    }

    pub fn is_launcher(&self) -> bool {
        self.tab.is_some()
    }

    /// The effective filter and query: a typed prefix overrides the tab.
    pub fn scope(&self) -> (Kind, &str) {
        let tab = self.tab.unwrap_or(Kind::All);
        let mut chars = self.query.chars();
        let kind = match chars.next() {
            Some('>') => Kind::Action,
            Some('@') => Kind::Symbol,
            Some('/') => Kind::Text,
            Some(':') => Kind::Line,
            Some('!') => Kind::Problem,
            _ => return (tab, self.query.as_str()),
        };
        if self.tab.is_none() {
            return (tab, self.query.as_str());
        }
        (kind, chars.as_str().trim_start())
    }

    /// Whether the query or tab changed since the last call.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// A background search for `kind` started; its results replace the old ones.
    pub fn start_search(&mut self, kind: Kind) {
        if !self.searching.contains(&kind) {
            self.searching.push(kind);
        }
    }

    /// Replace all items of one kind (async results arriving).
    pub fn set_kind_items(&mut self, kind: Kind, items: Vec<Item>) {
        self.searching.retain(|searching| *searching != kind);
        let selected = self.list.selected();
        self.items.retain(|item| item.kind != kind);
        self.items.extend(items);
        self.refilter();
        if let Some(row) = selected.filter(|row| *row < self.filtered.len()) {
            self.list.select(Some(row));
        }
    }

    /// Preselect a row (Recent Files starts on the previous file, as in `PyCharm`).
    pub fn select(&mut self, idx: usize) {
        if idx < self.filtered.len() {
            self.list.select(Some(idx));
        }
    }

    fn refilter(&mut self) {
        let started = std::time::Instant::now();
        self.refilter_inner();
        crate::debug_log(&format!(
            "picker: filtered {} items for {:?} in {:?}",
            self.items.len(),
            self.query,
            started.elapsed()
        ));
    }

    fn refilter_inner(&mut self) {
        self.filtered.clear();
        if self.tab.is_some() {
            self.refilter_launcher();
        } else if self.query.is_empty() {
            let mut order: Vec<usize> = (0..self.items.len().min(5000)).collect();
            if !self.keep_order {
                order.sort_by_key(|&idx| self.label_len(idx));
            }
            self.filtered = order.into_iter().map(|idx| (idx, Vec::new())).collect();
        } else {
            let pattern = Pattern::parse(&self.query, CaseMatching::Smart, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize, Vec<u32>)> = Vec::new();
            for (item_idx, item) in self.items.iter().enumerate() {
                let hay = format!("{} {}", item.label, item.detail);
                let mut positions = Vec::new();
                if let Some(score) = pattern.indices(
                    Utf32Str::new(&hay, &mut buf),
                    &mut self.matcher,
                    &mut positions,
                ) {
                    // Matches in the file name weigh more than matches in the directory.
                    let label_len = to_u32(item.label.chars().count());
                    let in_label = to_u32(positions.iter().filter(|&&pos| pos < label_len).count());
                    positions.retain(|&pos| pos < label_len);
                    scored.push((score + in_label * 8, item_idx, positions));
                }
            }
            scored.sort_by(|left, right| {
                right
                    .0
                    .cmp(&left.0)
                    .then(self.label_len(left.1).cmp(&self.label_len(right.1)))
            });
            self.filtered = scored
                .into_iter()
                .take(500)
                .map(|(_, item_idx, positions)| (item_idx, positions))
                .collect();
        }
        self.list.select(if self.filtered.is_empty() {
            None
        } else {
            Some(0)
        });
    }

    fn refilter_launcher(&mut self) {
        let (scope, query) = self.scope();
        let query = query.to_string();
        if scope == Kind::Line {
            let mut parts = query
                .split([':', ','])
                .map(|part| part.trim().parse::<u32>().ok());
            if let Some(Some(line)) = parts.next() {
                let col = parts.next().flatten().unwrap_or(1);
                self.items.retain(|item| item.kind != Kind::Line);
                self.items.push(Item {
                    label: format!(
                        "Go to line {line}{}",
                        if col > 1 {
                            format!(", column {col}")
                        } else {
                            String::new()
                        }
                    ),
                    detail: String::new(),
                    target: Target::Line(line, col),
                    kind: Kind::Line,
                    keywords: "",
                });
                self.filtered = vec![(self.items.len() - 1, Vec::new())];
            }
            return;
        }
        let kinds: &[Kind] = if scope == Kind::All {
            &[
                Kind::File,
                Kind::Symbol,
                Kind::Action,
                Kind::Problem,
                Kind::Text,
            ]
        } else {
            std::slice::from_ref(&scope)
        };
        // (tier, kind order, score, item, highlight indices)
        let mut ranked: Vec<(u8, usize, u32, usize, Vec<u32>)> = Vec::new();
        let scorer = Ranker::new(&query);
        for (item_idx, item) in self.items.iter().enumerate() {
            let Some(order) = kinds.iter().position(|kind| *kind == item.kind) else {
                continue;
            };
            if query.is_empty() {
                // With an empty query, All shows recent files and actions only.
                if !(scope == Kind::All && (item.kind.prefiltered() || item.kind == Kind::Problem))
                {
                    ranked.push((0, order, 0, item_idx, Vec::new()));
                }
                continue;
            }
            if let Some((tier, score, positions)) = scorer.rank(item, &mut self.matcher) {
                ranked.push((tier, order, score, item_idx, positions));
            }
        }
        if query.is_empty() {
            ranked.sort_by_key(|entry| entry.1);
        } else {
            // Better tiers first; within a tier, files before symbols before
            // actions before text; then the fuzzy score.
            ranked.sort_by(|left, right| {
                right
                    .0
                    .cmp(&left.0)
                    .then(left.1.cmp(&right.1))
                    .then(right.2.cmp(&left.2))
                    .then(self.label_len(left.3).cmp(&self.label_len(right.3)))
            });
        }
        let mut taken = [0usize; 7];
        for (_, _, _, item_idx, positions) in ranked {
            let Some(item) = self.items.get(item_idx) else {
                continue;
            };
            let kind = item.kind;
            let cap = if scope == Kind::All {
                kind.all_cap()
            } else {
                500
            };
            if let Some(count) = taken.get_mut(kind as usize)
                && *count < cap
            {
                *count += 1;
                self.filtered.push((item_idx, positions));
            }
        }
    }

    fn set_tab(&mut self, tab: Kind) {
        // Switching tabs drops a typed prefix, which would override the tab.
        if self.query.starts_with(['>', '@', '/', ':', '!']) {
            self.query.remove(0);
        }
        self.tab = Some(tab);
        self.changed = true;
        self.refilter();
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Enter => {
                return self
                    .list
                    .selected()
                    .and_then(|row| self.filtered.get(row))
                    .and_then(|(item_idx, _)| self.items.get(*item_idx))
                    .map_or(Outcome::Pending, |item| Outcome::Pick(item.target.clone()));
            }
            KeyCode::Tab | KeyCode::BackTab if self.tab.is_some() => {
                let current = Kind::TABS
                    .iter()
                    .position(|kind| Some(*kind) == self.tab)
                    .unwrap_or(0);
                let count = Kind::TABS.len();
                let next = if key.code == KeyCode::BackTab {
                    (current + count - 1) % count
                } else {
                    (current + 1) % count
                };
                if let Some(tab) = Kind::TABS.get(next) {
                    self.set_tab(*tab);
                }
            }
            KeyCode::Up => self.list.select_previous(),
            KeyCode::Down | KeyCode::Tab => self.list.select_next(),
            KeyCode::PageUp => self.list.scroll_up_by(10),
            KeyCode::PageDown => self.list.scroll_down_by(10),
            KeyCode::Backspace => {
                self.query.pop();
                self.changed = true;
                self.refilter();
            }
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.changed = true;
                self.refilter();
            }
            KeyCode::Char(chr) if !ctrl => {
                self.query.push(chr);
                self.changed = true;
                self.refilter();
            }
            _ => {}
        }
        Outcome::Pending
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &karet_theme::Theme) {
        let rect = popup_rect(area);
        frame.render_widget(Clear, rect);
        let styles = Styles {
            surface: crate::styles::surface(theme),
            muted: theme.style(karet_core::ThemeRole::Muted),
            accent: theme
                .token_style(karet_core::TokenId::FUNCTION)
                .add_modifier(Modifier::BOLD),
        };
        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(self.title_text())
            .style(styles.surface);
        // The selected entry's full path, which a row may not have room for.
        if let Some(path) = self.selected_path() {
            let max = rect.width.saturating_sub(4) as usize;
            block = block.title_bottom(Line::styled(
                format!(" {} ", keep_end(&path, max.saturating_sub(2))),
                styles.muted,
            ));
        }
        let mut inner = block.inner(rect);
        frame.render_widget(block, rect);
        if self.tab.is_some() {
            self.render_tabs(frame, Rect { height: 1, ..inner }, &styles);
            inner = Rect {
                y: inner.y + 1,
                height: inner.height.saturating_sub(1),
                ..inner
            };
        }
        let input = Rect { height: 1, ..inner };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("> ", styles.accent),
                Span::raw(self.query.as_str()),
            ])),
            input,
        );
        let typed = u16::try_from(self.query.chars().count()).unwrap_or(u16::MAX);
        frame.set_cursor_position((input.x.saturating_add(2).saturating_add(typed), input.y));
        let list_area = Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        };
        if self.tab.is_some() && self.filtered.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::styled(
                    format!("  {}", self.empty_message()),
                    styles.muted,
                )),
                list_area,
            );
            return;
        }
        let room =
            (list_area.width as usize).saturating_sub(if self.tab.is_some() { 8 } else { 0 });
        let items: Vec<ListItem> = self
            .filtered
            .iter()
            .filter_map(|(item_idx, positions)| {
                let item = self.items.get(*item_idx)?;
                Some(self.row(item, positions, room, &styles))
            })
            .collect();
        let sel = Style::default().add_modifier(Modifier::REVERSED);
        frame.render_stateful_widget(
            List::new(items).highlight_style(sel).scroll_padding(2),
            list_area,
            &mut self.list,
        );
    }

    fn title_text(&self) -> String {
        if self.tab.is_some() {
            if self.searching.is_empty() {
                format!(" {} ", self.title)
            } else {
                format!(" {} (searching...) ", self.title)
            }
        } else {
            format!(
                " {} ({}/{}) ",
                self.title,
                self.filtered.len(),
                self.items.len()
            )
        }
    }

    /// The launcher's filter tabs, the effective one highlighted.
    fn render_tabs(&self, frame: &mut Frame, area: Rect, styles: &Styles) {
        let (scope, _) = self.scope();
        let selected = styles
            .surface
            .add_modifier(Modifier::REVERSED | Modifier::BOLD);
        let mut spans = Vec::new();
        for kind in Kind::TABS {
            let name = format!(" {} ", kind.tab_name());
            spans.push(if kind == scope {
                Span::styled(name, selected)
            } else {
                Span::styled(name, styles.muted)
            });
            spans.push(Span::raw(" "));
        }
        if scope == Kind::Line {
            spans.push(Span::styled(" Line ", selected));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// One list row, fitted into `room` columns, with the matched letters
    /// of the label highlighted.
    fn row(
        &self,
        item: &Item,
        positions: &[u32],
        room: usize,
        styles: &Styles,
    ) -> ListItem<'static> {
        let mut spans: Vec<Span> = Vec::new();
        if self.tab.is_some() {
            spans.push(Span::styled(
                format!("{:<7} ", item.kind.tag()),
                styles.muted,
            ));
        }
        // Fit the row: the detail gives way first (a path keeps its end,
        // anything else its start), then the label.
        let label_width = item.label.chars().count();
        let detail = if item.detail.is_empty() || label_width + 4 >= room {
            String::new()
        } else {
            let max = room - label_width - 2;
            if item.detail.contains('/') && !item.detail.contains(' ') {
                keep_end(&item.detail, max)
            } else {
                keep_start(&item.detail, max)
            }
        };
        let label = keep_start(&item.label, room);
        spans.extend(label.chars().enumerate().map(|(col, chr)| {
            if positions.contains(&to_u32(col)) {
                Span::styled(chr.to_string(), styles.accent)
            } else {
                Span::raw(chr.to_string())
            }
        }));
        if !detail.is_empty() {
            spans.push(Span::styled(format!("  {detail}"), styles.muted));
        }
        ListItem::new(Line::from(spans))
    }

    /// What an empty launcher list says instead.
    fn empty_message(&self) -> &'static str {
        let (scope, query) = self.scope();
        let waiting = match scope {
            Kind::All => !self.searching.is_empty(),
            kind => self.searching.contains(&kind),
        };
        match scope {
            _ if waiting => "Searching...",
            Kind::Text if query.chars().count() < 2 => {
                "Type 2+ characters to search text in all files"
            }
            Kind::Symbol if query.is_empty() => {
                "Type to search symbols (needs a running language server)"
            }
            Kind::Symbol => "No symbols (or the language server has no symbol search)",
            Kind::Line => "Type a line number, optionally :column",
            Kind::Problem if query.is_empty() => "No problems reported",
            _ => "No matches",
        }
    }

    /// The byte length of an item's label, for ordering ties shortest first.
    fn label_len(&self, item_idx: usize) -> usize {
        self.items.get(item_idx).map_or(0, |item| item.label.len())
    }
}

/// The popup's area: 70% of the screen, but on a narrow terminal nearly all of it.
fn popup_rect(area: Rect) -> Rect {
    let width = (area.width * 7 / 10)
        .max(area.width.saturating_sub(4).min(100))
        .clamp(40.min(area.width), 120.min(area.width));
    let height = (area.height * 6 / 10).max(8.min(area.height));
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + area.height / 8,
        width,
        height,
    }
}

/// The popup's colours, resolved once per frame.
struct Styles {
    surface: Style,
    muted: Style,
    accent: Style,
}

/// A count or position as `u32`, saturating (match positions are `u32`).
fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Launcher ranking in tiers, so a whole-word hit beats letters scattered
/// through a long name:
/// 3: the label starts with the query;
/// 2: the label contains it, or every query word starts a word of the label or
///    the hidden keywords (or the item came from a search: text, symbols);
/// 1: a fuzzy match on the label (on the path too when the query has a `/`).
struct Ranker {
    query: String,
    words: Vec<String>,
    pattern: Pattern,
}

impl Ranker {
    fn new(query: &str) -> Self {
        let query = query.to_lowercase();
        let words = query.split_whitespace().map(str::to_string).collect();
        let pattern = Pattern::parse(&query, CaseMatching::Smart, Normalization::Smart);
        Self {
            query,
            words,
            pattern,
        }
    }

    fn rank(&self, item: &Item, matcher: &mut Matcher) -> Option<(u8, u32, Vec<u32>)> {
        let label = item.label.to_lowercase();
        let mut buf = Vec::new();
        let mut positions = Vec::new();
        let fuzzy = self.pattern.indices(
            Utf32Str::new(&item.label, &mut buf),
            matcher,
            &mut positions,
        );
        if item.kind.prefiltered() {
            return Some((2, fuzzy.unwrap_or(0), positions));
        }
        if label.starts_with(&self.query) {
            return Some((3, fuzzy.unwrap_or(0), positions));
        }
        let word_starts = |text: &str, word: &str| {
            text.split(|chr: char| !chr.is_alphanumeric())
                .any(|part| part.starts_with(word))
        };
        let all_words = !self.words.is_empty()
            && self
                .words
                .iter()
                .all(|word| word_starts(&label, word) || word_starts(item.keywords, word));
        let in_detail =
            item.kind == Kind::Problem && item.detail.to_lowercase().contains(&self.query);
        if label.contains(&self.query) || all_words || in_detail {
            return Some((2, fuzzy.unwrap_or(0), positions));
        }
        if let Some(score) = fuzzy {
            return Some((1, score, positions));
        }
        if item.kind == Kind::File && self.query.contains('/') {
            let hay = format!("{}/{}", item.detail, item.label);
            positions.clear();
            if let Some(score) =
                self.pattern
                    .indices(Utf32Str::new(&hay, &mut buf), matcher, &mut positions)
            {
                return Some((1, score, Vec::new()));
            }
        }
        None
    }
}

impl Picker {
    /// The selected entry's file (and line), relative to the root.
    fn selected_path(&self) -> Option<String> {
        let (item_idx, _) = self.filtered.get(self.list.selected()?)?;
        let rel = |path: &PathBuf| {
            let path = self
                .root
                .as_ref()
                .and_then(|root| path.strip_prefix(root).ok())
                .unwrap_or(path);
            path.display().to_string()
        };
        match &self.items.get(*item_idx)?.target {
            Target::File(path) => Some(rel(path)),
            Target::Location(path, pos) => Some(format!("{}:{}", rel(path), pos.line + 1)),
            _ => None,
        }
    }
}

/// At most `max` chars, ending in `...` when cut.
pub fn keep_start(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{kept}...")
}

/// At most `max` chars, starting with `...` when cut: for paths, whose end matters.
pub fn keep_end(text: &str, max: usize) -> String {
    let len = text.chars().count();
    if len <= max {
        return text.to_string();
    }
    let kept: String = text.chars().skip(len - max.saturating_sub(3)).collect();
    format!("...{kept}")
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn file(path: &str) -> TestResult<Item> {
        let path = PathBuf::from(path);
        Ok(Item {
            label: path
                .file_name()
                .ok_or("no file name")?
                .to_string_lossy()
                .into_owned(),
            detail: path.parent().ok_or("no parent")?.display().to_string(),
            target: Target::File(path),
            kind: Kind::File,
            keywords: "",
        })
    }

    fn items() -> TestResult<Vec<Item>> {
        let mut items = vec![
            file("target/debug/deps/libkaret_syntax-3f2e1a.rlib")?,
            file("src/app.rs")?,
            file("src/explorer.rs")?,
        ];
        items.extend(Action::ALL.iter().map(|(action, label, _)| Item {
            label: (*label).to_string(),
            detail: String::new(),
            target: Target::Action(*action),
            kind: Kind::Action,
            keywords: action.keywords(),
        }));
        Ok(items)
    }

    /// The label of the launcher's first result for `query`.
    fn first(query: &str) -> TestResult<String> {
        let picker = Picker::launcher(items()?, Kind::All, query);
        let (item_idx, _) = picker.filtered.first().ok_or("no results")?;
        Ok(picker.items.get(*item_idx).ok_or("no item")?.label.clone())
    }

    #[test]
    fn a_whole_word_beats_scattered_letters() -> TestResult {
        // "rename" is scattered through the build artefact's name, but is
        // the start of the Rename action.
        assert_eq!(first("rename")?, "Rename Symbol");
        Ok(())
    }

    #[test]
    fn synonyms_find_actions() -> TestResult {
        assert_eq!(first("jump to source")?, "Go to Definition (or Usages)");
        Ok(())
    }

    #[test]
    fn prefixes_pick_a_scope() -> TestResult {
        let picker = Picker::launcher(items()?, Kind::All, ">app");
        assert_eq!(picker.scope(), (Kind::Action, "app"));
        assert!(picker.filtered.iter().all(|(item_idx, _)| {
            picker
                .items
                .get(*item_idx)
                .is_some_and(|item| item.kind == Kind::Action)
        }));
        let picker = Picker::launcher(items()?, Kind::All, ":12:3");
        let (item_idx, _) = picker.filtered.first().ok_or("no results")?;
        let item = picker.items.get(*item_idx).ok_or("no item")?;
        assert!(matches!(item.target, Target::Line(12, 3)));
        Ok(())
    }
}
