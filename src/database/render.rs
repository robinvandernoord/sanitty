//! Drawing the Database view: the title, the schema tree, the query pane's
//! frame, the result grid and the view's popups, and the hint bar's keys.

use std::fmt::Write as _;

use karet_core::ThemeRole;
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
use ratatui::widgets::Wrap;

use super::Database;
use super::Focus;
use super::Node;
use super::Phase;
use super::Popup;
use super::grid::Grid;
use super::schema_names;

impl Database {
    pub fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match &self.popup {
            Some(Popup::Connections { .. }) => {
                return vec![("Enter", "Connect"), ("Up/Down", "Move"), ("Esc", "Close")];
            }
            Some(Popup::Confirm { confirm, .. }) => return confirm.hints(),
            Some(Popup::Edit { .. }) => {
                return vec![("Enter", "Write"), ("Tab", "NULL"), ("Esc", "Cancel")];
            }
            None => {}
        }
        if !matches!(self.phase, Phase::Ready) {
            return vec![("c", "Connections"), ("r", "Find again")];
        }
        match self.focus {
            Focus::Tree if self.tree.filtering => {
                vec![("Type", "Filter"), ("Enter", "Done"), ("Esc", "Clear")]
            }
            Focus::Tree => vec![
                ("Enter", "Open table"),
                ("Right/Left", "Expand"),
                ("s", "Select in query"),
                ("/", "Filter"),
                ("c", "Connections"),
                ("r", "Refresh"),
                ("Tab", "Next pane"),
            ],
            Focus::Query => vec![("Shift+Tab", "Next pane")],
            Focus::Grid => vec![
                ("Enter", "Edit cell"),
                ("y", "Copy value"),
                ("r", "Run again"),
                ("Tab", "Next pane"),
            ],
        }
    }

    /// Draw the view. Gives back where the query editor goes, when it is
    /// shown, for the app to draw the file there; the app then draws
    /// [`Database::render_popup`] over both.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        top: Rect,
        body: Rect,
        theme: &Theme,
    ) -> Option<Rect> {
        let base = crate::styles::surface(theme);
        let muted = theme.style(ThemeRole::Muted);
        self.render_title(frame, top, theme);
        let message = match &self.phase {
            Phase::Finding => Some("Looking for databases...".to_string()),
            Phase::Connecting => Some("Connecting...".to_string()),
            Phase::Idle(text) => Some(text.clone()),
            Phase::Ready => None,
        };
        if let Some(text) = message {
            frame.render_widget(
                Paragraph::new(Line::styled(format!("  {text}"), muted))
                    .style(base)
                    .wrap(Wrap { trim: false }),
                body,
            );
            return None;
        }
        let tree_width = (body.width / 4).clamp(24, 44).min(body.width / 2);
        let tree_area = Rect {
            width: tree_width,
            ..body
        };
        let right = Rect {
            x: body.x.saturating_add(tree_width),
            width: body.width.saturating_sub(tree_width),
            ..body
        };
        let query_height = (right.height * 2 / 5).max(5).min(right.height);
        let query_area = Rect {
            height: query_height,
            ..right
        };
        let grid_area = Rect {
            y: right.y.saturating_add(query_height),
            height: right.height.saturating_sub(query_height),
            ..right
        };
        self.render_tree(frame, tree_area, theme);
        let name = self
            .current
            .and_then(|idx| self.connections.get(idx))
            .map_or_else(String::new, |conn| conn.name.clone());
        let query_block = self
            .pane(Focus::Query, format!(" {name}.sql "), theme)
            .title(Line::styled(" Alt+Enter run ", muted).right_aligned());
        let query_inner = query_block.inner(query_area);
        frame.render_widget(query_block, query_area);
        self.render_grid(frame, grid_area, theme);
        Some(query_inner)
    }

    pub(super) fn pane(&self, focus: Focus, title: String, theme: &Theme) -> Block<'static> {
        let base = crate::styles::surface(theme);
        let border = if self.focus == focus {
            base
        } else {
            theme.style(ThemeRole::Muted)
        };
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(border)
            .title(title)
            .style(base)
    }

    pub(super) fn render_title(&self, frame: &mut Frame, top: Rect, theme: &Theme) {
        let mut text = String::from(" Database");
        if let Some(conn) = self.current.and_then(|idx| self.connections.get(idx)) {
            let _ = write!(text, "   {} ({})", conn.name, conn.describe());
        }
        if let Some(schema) = &self.schema {
            let count = schema.tables.len();
            let _ = write!(
                text,
                "   {count} {}",
                if count == 1 { "table" } else { "tables" }
            );
        }
        if self.running() {
            text.push_str("   (running...)");
        }
        frame.render_widget(Paragraph::new(text).style(crate::styles::bar(theme)), top);
    }

    pub(super) fn render_tree(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let muted = theme.style(ThemeRole::Muted);
        let title = match &self.tree.filter {
            Some(filter) => format!(" /{filter} "),
            None => " schema ".to_string(),
        };
        let block = self.pane(Focus::Tree, title, theme);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let nodes = self.nodes();
        let height = usize::from(inner.height);
        self.tree.page = height.saturating_sub(1).max(1);
        if self.tree.selected < self.tree.top {
            self.tree.top = self.tree.selected;
        } else if height > 0 && self.tree.selected >= self.tree.top + height {
            self.tree.top = self.tree.selected + 1 - height;
        }
        let flat = self
            .schema
            .as_ref()
            .is_none_or(|schema| schema_names(schema).len() <= 1);
        let width = usize::from(inner.width);
        let type_style = theme.token_style(karet_core::StandardToken::Type.id());
        let lines: Vec<Line> = nodes
            .iter()
            .enumerate()
            .skip(self.tree.top)
            .take(height)
            .map(|(pos, node)| {
                let indent = |level: usize| "  ".repeat(if flat { level - 1 } else { level });
                let arrow = |open: bool| if open { "v " } else { "> " };
                let mut spans = match node {
                    Node::Schema(name) => vec![Span::raw(format!(
                        "{}{name}",
                        arrow(self.tree.open.contains(name))
                    ))],
                    Node::Table(idx) => {
                        let table = self.table(*idx);
                        let open = table.is_some_and(|table| {
                            self.tree
                                .open
                                .contains(&format!("{}.{}", table.schema, table.name))
                        });
                        let name = table.map_or("", |table| table.name.as_str());
                        let mut spans =
                            vec![Span::raw(format!("{}{}{name}", indent(1), arrow(open)))];
                        if table.is_some_and(|table| table.view) {
                            spans.push(Span::styled(" view", muted));
                        }
                        spans
                    }
                    Node::Column(table, column) => {
                        let column = self
                            .table(*table)
                            .and_then(|table| table.columns.get(*column));
                        let (name, sql_type, key) = column.map_or(("", "", false), |column| {
                            (
                                column.name.as_str(),
                                column.sql_type.as_str(),
                                column.primary,
                            )
                        });
                        let mark = if key { "* " } else { "  " };
                        vec![
                            Span::raw(format!("{}{mark}{name} ", indent(2))),
                            Span::styled(sql_type.to_string(), type_style),
                        ]
                    }
                };
                let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
                spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
                let line = Line::from(spans);
                if pos == self.tree.selected {
                    selected(line, theme, self.focus == Focus::Tree)
                } else {
                    line
                }
            })
            .collect();
        if lines.is_empty() {
            let text = if self.tree.filter.is_some() {
                "  No table matches"
            } else {
                "  No tables"
            };
            frame.render_widget(Paragraph::new(Line::styled(text, muted)), inner);
        } else {
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }

    pub(super) fn render_grid(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let error_style = theme.style(ThemeRole::DiagnosticError);
        let title = match (&self.error, &self.grid) {
            (Some(_), _) => " error ".to_string(),
            (None, Some(grid)) => format!(" {} ", grid.summary()),
            (None, None) => " result ".to_string(),
        };
        let mut block = self.pane(Focus::Grid, title, theme);
        if let Some(grid) = &self.grid
            && grid.result.origins.iter().any(Option::is_some)
            && self.error.is_none()
        {
            block = block.title(
                Line::styled(" Enter edits a cell ", theme.style(ThemeRole::Muted)).right_aligned(),
            );
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if let Some(err) = &self.error {
            frame.render_widget(
                Paragraph::new(Line::styled(err.clone(), error_style)).wrap(Wrap { trim: false }),
                inner,
            );
            return;
        }
        let focused = self.focus == Focus::Grid;
        if let Some(grid) = &mut self.grid {
            grid.render(frame, inner, theme, focused);
        }
    }

    pub fn render_popup(&mut self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let Some(popup) = &mut self.popup else { return };
        match popup {
            Popup::Connections { selected } => {
                let lines: Vec<Line> = self
                    .connections
                    .iter()
                    .enumerate()
                    .map(|(idx, conn)| {
                        let line = Line::from(vec![
                            Span::raw(format!(" {} ", conn.name)),
                            Span::styled(
                                format!("{} ", conn.describe()),
                                theme.style(ThemeRole::Muted),
                            ),
                        ]);
                        if idx == *selected {
                            line.style(Style::default().add_modifier(Modifier::REVERSED))
                        } else {
                            line
                        }
                    })
                    .collect();
                popup_box(frame, area, " Connections ", lines, theme);
            }
            Popup::Confirm { confirm, .. } => confirm.render(frame, area, theme),
            Popup::Edit { text, null } => {
                let column = self
                    .grid
                    .as_ref()
                    .and_then(Grid::column_name)
                    .unwrap_or("value");
                let value = if *null {
                    Line::styled("NULL", theme.style(ThemeRole::Muted))
                } else {
                    Line::raw(format!("{text}\u{2588}"))
                };
                popup_box(frame, area, &format!(" {column} "), vec![value], theme);
            }
        }
    }
}

/// A centred box with a border and these lines.
pub fn popup_box(frame: &mut Frame, area: Rect, title: &str, lines: Vec<Line>, theme: &Theme) {
    let base = crate::styles::surface(theme);
    let wanted = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(40)
        .max(title.chars().count() + 4)
        .saturating_add(4);
    let width = u16::try_from(wanted.max(50))
        .unwrap_or(u16::MAX)
        .min(area.width.saturating_sub(4));
    let height = u16::try_from(lines.len().saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 3,
        width,
        height,
    };
    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title.to_string())
        .style(base);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        rect,
    );
}

pub(super) fn selected<'a>(line: Line<'a>, theme: &Theme, focused: bool) -> Line<'a> {
    let colour = theme.style(ThemeRole::Selection).fg;
    match colour {
        Some(colour) if focused => {
            let spans: Vec<Span> = line
                .spans
                .into_iter()
                .map(|span| {
                    let style = span.style.bg(colour);
                    span.style(style)
                })
                .collect();
            Line::from(spans)
        }
        None if focused => line.style(Style::default().add_modifier(Modifier::REVERSED)),
        _ => line.style(Style::default().add_modifier(Modifier::BOLD)),
    }
}
