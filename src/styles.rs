//! Shared styles.
//!
//! `Theme::style(role)` sets only the foreground colour, also for roles that
//! name a background. These helpers build proper fg-on-bg pairs so text never
//! falls back to the terminal's own colours (which may be a light theme).

use karet_core::ThemeRole;
use karet_theme::Theme;
use ratatui::style::Color;
use ratatui::style::Style;

fn colour(theme: &Theme, role: ThemeRole) -> Color {
    theme.style(role).fg.unwrap_or(Color::Reset)
}

/// Normal text on the editor background: the base for every surface.
pub fn surface(theme: &Theme) -> Style {
    Style::default()
        .fg(colour(theme, ThemeRole::Foreground))
        .bg(colour(theme, ThemeRole::Background))
}

/// The status bar.
pub fn bar(theme: &Theme) -> Style {
    Style::default()
        .fg(colour(theme, ThemeRole::StatusBarForeground))
        .bg(colour(theme, ThemeRole::StatusBarBackground))
}
