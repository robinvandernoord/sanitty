//! The project tree popup (Alt+1): a left-hand panel that only exists while open.
//!
//! Typing searches the visible entries, as `PyCharm`'s speed search does: the
//! selection jumps to the best match, Up/Down step between matches, and
//! expanding a folder (Right or Enter) starts a fresh search inside it. So
//! `py4` Right `ap` Right `adm` walks to `py4web/apps/_admin`.

use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
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

struct Row {
    path: PathBuf,
    depth: usize,
    is_dir: bool,
}

pub struct Explorer {
    root: PathBuf,
    expanded: HashSet<PathBuf>,
    rows: Vec<Row>,
    list: ListState,
    /// The speed-search text, empty when not searching.
    query: String,
}

pub enum Outcome {
    Pending,
    Close,
    Open(PathBuf),
}

impl Explorer {
    /// Open with the tree expanded down to `reveal` (the active file).
    pub fn new(root: &Path, reveal: Option<&Path>) -> Self {
        let mut expanded = HashSet::new();
        expanded.insert(root.to_path_buf());
        let mut explorer = Self {
            root: root.to_path_buf(),
            expanded,
            rows: Vec::new(),
            list: ListState::default(),
            query: String::new(),
        };
        explorer.list.select(Some(0));
        explorer.reveal(reveal);
        explorer
    }

    /// Re-read the tree (files may have changed since it was last open) and
    /// select `path`, expanding its folders; without one, keep the selection
    /// where it was, so reopening the tree finds it as it was left.
    pub fn reveal(&mut self, path: Option<&Path>) {
        let keep = self.selected().map(|row| row.path.clone());
        if let Some(mut folder) = path.and_then(Path::parent) {
            while folder.starts_with(&self.root) {
                self.expanded.insert(folder.to_path_buf());
                match folder.parent() {
                    Some(parent) => folder = parent,
                    None => break,
                }
            }
        }
        self.query.clear();
        self.rebuild();
        let target = path.map(Path::to_path_buf).or(keep);
        let idx = target
            .and_then(|wanted| self.rows.iter().position(|row| row.path == wanted))
            .unwrap_or(0);
        self.list.select(Some(idx));
    }

    fn rebuild(&mut self) {
        self.rows.clear();
        let root = self.root.clone();
        self.walk(&root, 0);
    }

    fn walk(&mut self, dir: &Path, depth: usize) {
        // `ignore` gives gitignore-aware listing without recursing.
        let mut entries: Vec<(PathBuf, bool)> = ignore::WalkBuilder::new(dir)
            .max_depth(Some(1))
            .hidden(false)
            .filter_entry(|entry| entry.file_name() != ".git")
            .build()
            .filter_map(Result::ok)
            .filter(|entry| entry.depth() == 1)
            .map(|entry| {
                (
                    entry.path().to_path_buf(),
                    entry.file_type().is_some_and(|kind| kind.is_dir()),
                )
            })
            .collect();
        // Folders first, then by name.
        entries.sort_by(|(left_path, left_dir), (right_path, right_dir)| {
            right_dir
                .cmp(left_dir)
                .then_with(|| left_path.file_name().cmp(&right_path.file_name()))
        });
        for (path, is_dir) in entries {
            let open = is_dir && self.expanded.contains(&path);
            self.rows.push(Row {
                path: path.clone(),
                depth,
                is_dir,
            });
            if open {
                self.walk(&path, depth + 1);
            }
        }
    }

    fn selected(&self) -> Option<&Row> {
        self.list.selected().and_then(|idx| self.rows.get(idx))
    }

    fn toggle(&mut self, expand: Option<bool>) {
        let Some(row) = self.selected() else { return };
        if !row.is_dir {
            if expand == Some(false) {
                self.select_parent();
            }
            return;
        }
        let path = row.path.clone();
        let is_open = self.expanded.contains(&path);
        match expand {
            Some(true) if is_open => self.list.select_next(),
            Some(false) if !is_open => self.select_parent(),
            _ => {
                if is_open {
                    self.expanded.remove(&path);
                } else {
                    self.expanded.insert(path.clone());
                }
                self.rebuild();
                let idx = self.rows.iter().position(|row| row.path == path);
                self.list.select(idx);
            }
        }
    }

    fn name(row: &Row) -> String {
        row.path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().to_lowercase())
    }

    /// How well a row matches the query: 3 prefix, 2 substring, 1 subsequence.
    fn tier(&self, row: &Row) -> u8 {
        let name = Self::name(row);
        let query = self.query.to_lowercase();
        if name.starts_with(&query) {
            3
        } else if name.contains(&query) {
            2
        } else {
            let mut chars = name.chars();
            u8::from(query.chars().all(|wanted| chars.any(|chr| chr == wanted)))
        }
    }

    /// Select the best match, the first one at or after the selection.
    fn search(&mut self) {
        if self.query.is_empty() {
            return;
        }
        let best = self
            .rows
            .iter()
            .map(|row| self.tier(row))
            .max()
            .unwrap_or(0);
        if best == 0 {
            return;
        }
        let start = self.list.selected().unwrap_or(0);
        let count = self.rows.len();
        if let Some(idx) = (0..count)
            .map(|offset| (start + offset) % count)
            .find(|idx| {
                self.rows
                    .get(*idx)
                    .is_some_and(|row| self.tier(row) == best)
            })
        {
            self.list.select(Some(idx));
        }
    }

    /// Up/Down while searching: the previous or next row that matches at all.
    fn step_match(&mut self, forward: bool) {
        let count = self.rows.len();
        let start = self.list.selected().unwrap_or(0);
        let found = (1..count)
            .map(|offset| {
                if forward {
                    (start + offset) % count
                } else {
                    (start + count - offset) % count
                }
            })
            .find(|idx| self.rows.get(*idx).is_some_and(|row| self.tier(row) > 0));
        if let Some(idx) = found {
            self.list.select(Some(idx));
        }
    }

    fn select_parent(&mut self) {
        let Some(parent) = self
            .selected()
            .and_then(|row| row.path.parent().map(Path::to_path_buf))
        else {
            return;
        };
        if let Some(idx) = self.rows.iter().position(|row| row.path == parent) {
            self.list.select(Some(idx));
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let searching = !self.query.is_empty();
        // Moving into or out of a folder starts a new search there.
        if matches!(key.code, KeyCode::Right | KeyCode::Left | KeyCode::Enter) {
            self.query.clear();
        }
        match key.code {
            KeyCode::Esc if searching => self.query.clear(),
            KeyCode::Esc => return Outcome::Close,
            KeyCode::Backspace => {
                self.query.pop();
                self.search();
            }
            KeyCode::Up if searching => self.step_match(false),
            KeyCode::Down if searching => self.step_match(true),
            KeyCode::Char(chr) if plain && (chr != ' ' || searching) => {
                self.query.push(chr);
                self.search();
            }
            KeyCode::Up => self.list.select_previous(),
            KeyCode::Down => self.list.select_next(),
            KeyCode::PageUp => self.list.scroll_up_by(15),
            KeyCode::PageDown => self.list.scroll_down_by(15),
            KeyCode::Home => self.list.select_first(),
            KeyCode::End => self.list.select_last(),
            KeyCode::Right => self.toggle(Some(true)),
            KeyCode::Left => self.toggle(Some(false)),
            KeyCode::Enter | KeyCode::Char(' ') => {
                if let Some(row) = self.selected()
                    && !row.is_dir
                {
                    return Outcome::Open(row.path.clone());
                }
                self.toggle(None);
            }
            _ => {}
        }
        Outcome::Pending
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, theme: &karet_theme::Theme) {
        // Wide enough for the longest visible name, within 30 columns and
        // 60% of the screen.
        let longest = self
            .rows
            .iter()
            .map(|row| {
                row.depth * 2
                    + 2
                    + row
                        .path
                        .file_name()
                        .map_or(0, |name| name.to_string_lossy().chars().count())
            })
            .max()
            .unwrap_or(0);
        let width = u16::try_from(longest)
            .unwrap_or(u16::MAX)
            .saturating_add(4)
            .clamp(
                30.min(area.width),
                (area.width * 6 / 10).max(30.min(area.width)),
            );
        let rect = Rect { width, ..area };
        frame.render_widget(Clear, rect);
        let surface = crate::styles::surface(theme);
        let muted = theme.style(karet_core::ThemeRole::Muted);
        let name = self.root.file_name().map_or_else(
            || self.root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let accent = theme
            .token_style(karet_core::TokenId::FUNCTION)
            .add_modifier(Modifier::BOLD);
        let mut block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(format!(" {name} "))
            .style(surface);
        if !self.query.is_empty() {
            block = block.title_bottom(Line::from(vec![
                Span::raw(" search: "),
                Span::styled(format!("{} ", self.query), accent),
            ]));
        } else if let Some(row) = self.selected() {
            // The selected entry's path, in case its name is cut off.
            let rel = row
                .path
                .strip_prefix(&self.root)
                .unwrap_or(&row.path)
                .display()
                .to_string();
            let max = usize::from(width).saturating_sub(6);
            block = block.title_bottom(Line::styled(
                format!(" {} ", crate::picker::keep_end(&rel, max)),
                muted,
            ));
        }
        let name_room = usize::from(width).saturating_sub(2);
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|row| {
                let file = row
                    .path
                    .file_name()
                    .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
                let marker = if row.is_dir {
                    if self.expanded.contains(&row.path) {
                        "v "
                    } else {
                        "> "
                    }
                } else {
                    "  "
                };
                let mut style = if row.is_dir {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                if !self.query.is_empty() && self.tier(row) >= 2 {
                    style = style.patch(accent);
                }
                let file =
                    crate::picker::keep_start(&file, name_room.saturating_sub(row.depth * 2 + 2));
                ListItem::new(Line::from(vec![
                    Span::raw("  ".repeat(row.depth)),
                    Span::styled(marker, muted),
                    Span::styled(file, style),
                ]))
            })
            .collect();
        let selected = Style::default().add_modifier(Modifier::REVERSED);
        frame.render_stateful_widget(
            List::new(items)
                .block(block)
                .highlight_style(selected)
                .scroll_padding(3),
            rect,
            &mut self.list,
        );
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    use super::*;

    fn press(explorer: &mut Explorer, code: KeyCode) {
        let _ = explorer.handle_key(&KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn typed(explorer: &mut Explorer, text: &str) {
        for chr in text.chars() {
            press(explorer, KeyCode::Char(chr));
        }
    }

    fn selected_path(explorer: &Explorer) -> Result<PathBuf, &'static str> {
        explorer
            .selected()
            .map(|row| row.path.clone())
            .ok_or("nothing selected")
    }

    /// Speed search through nested folders: py4 Right ap Right adm.
    #[test]
    fn speed_search_walks_down_the_tree() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("sanitty-tree-{}", std::process::id()));
        for dir in [
            "py4web/apps/admin",
            "py4web/apps/_default",
            "pyproject",
            "docs",
        ] {
            std::fs::create_dir_all(root.join(dir))?;
        }
        std::fs::write(root.join("py4web/apps/admin/__init__.py"), "")?;
        let mut explorer = Explorer::new(&root, None);

        typed(&mut explorer, "py4");
        assert_eq!(selected_path(&explorer)?, root.join("py4web"));
        press(&mut explorer, KeyCode::Right);
        typed(&mut explorer, "ap");
        assert_eq!(selected_path(&explorer)?, root.join("py4web/apps"));
        press(&mut explorer, KeyCode::Right);
        typed(&mut explorer, "adm");
        let found = selected_path(&explorer)?;
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(found, root.join("py4web/apps/admin"));
        Ok(())
    }
}
