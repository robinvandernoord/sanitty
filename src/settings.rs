//! Settings from `$XDG_CONFIG_HOME/sanitty/settings.toml`, read once at
//! start (like the keymap). Every key is optional:
//!
//! ```toml
//! [editor]
//! tab_size = 4          # spaces Tab inserts
//!
//! [python]
//! server = "ty"         # ty | basedpyright | pyright | pylsp
//! ruff = true           # ruff alongside, for lint warnings and formatting
//!
//! [database]
//! table_rows = 500      # rows when opening a table from the tree
//! max_rows = 10000      # rows a result keeps
//!
//! [lsp.go]              # add or change a language server; see `lsp::registry`
//! command = "gopls"
//! extensions = ["go"]
//! ```
//!
//! A file that does not parse, or a value out of range, is reported and
//! the defaults stay: a typo never stops sanitty.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub editor: Editor,
    pub python: Python,
    pub database: Database,
    /// Language servers added or changed, by name.
    pub lsp: BTreeMap<String, LspEntry>,
}

/// An `[lsp.<name>]` section; every field is optional here, and what a
/// server needs is checked when the registry is built (`lsp::registry`).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LspEntry {
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub extensions: Option<Vec<String>>,
    pub label: Option<String>,
    pub language_id: Option<String>,
    pub language_ids: BTreeMap<String, String>,
    pub linter: Option<bool>,
    pub enabled: Option<bool>,
    pub install: Option<String>,
    pub init_options: Option<toml::Table>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Editor {
    pub tab_size: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Python {
    /// The preferred server; the others follow in the usual order when it
    /// is not installed.
    pub server: PythonServer,
    pub ruff: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PythonServer {
    /// Astral's ty, written `ty` in the file.
    #[serde(rename = "ty")]
    AstralTy,
    Basedpyright,
    Pyright,
    Pylsp,
}

impl PythonServer {
    /// The usual order: ty first (fast, beta), then the fallbacks.
    pub const ALL: [Self; 4] = [
        Self::AstralTy,
        Self::Basedpyright,
        Self::Pyright,
        Self::Pylsp,
    ];

    /// The command and its arguments.
    pub fn command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::AstralTy => ("ty", &["server"]),
            Self::Basedpyright => ("basedpyright-langserver", &["--stdio"]),
            Self::Pyright => ("pyright-langserver", &["--stdio"]),
            Self::Pylsp => ("pylsp", &[]),
        }
    }

    /// The preferred server first, then the rest in the usual order.
    pub fn order(self) -> Vec<Self> {
        std::iter::once(self)
            .chain(Self::ALL.into_iter().filter(|server| *server != self))
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Database {
    pub table_rows: usize,
    pub max_rows: usize,
}

impl Default for Editor {
    fn default() -> Self {
        Self { tab_size: 4 }
    }
}

impl Default for Python {
    fn default() -> Self {
        Self {
            server: PythonServer::AstralTy,
            ruff: true,
        }
    }
}

impl Default for Database {
    fn default() -> Self {
        Self {
            table_rows: 500,
            max_rows: 10_000,
        }
    }
}

impl Settings {
    /// Parse a settings file. Values out of range are reported and set back
    /// to their defaults; a file that does not parse gives all defaults.
    pub fn parse(text: &str) -> (Self, Vec<String>) {
        let mut settings = match toml::from_str::<Self>(text) {
            Ok(settings) => settings,
            Err(err) => {
                let message = err.message().to_string();
                return (Self::default(), vec![format!("settings.toml: {message}")]);
            }
        };
        let mut problems = Vec::new();
        let defaults = Self::default();
        let mut check = |name: &str,
                         value: &mut usize,
                         range: std::ops::RangeInclusive<usize>,
                         default: usize| {
            if !range.contains(value) {
                problems.push(format!(
                    "settings.toml: {name} must be {} to {}; using {default}",
                    range.start(),
                    range.end()
                ));
                *value = default;
            }
        };
        check(
            "editor.tab_size",
            &mut settings.editor.tab_size,
            1..=16,
            defaults.editor.tab_size,
        );
        check(
            "database.table_rows",
            &mut settings.database.table_rows,
            1..=100_000,
            defaults.database.table_rows,
        );
        check(
            "database.max_rows",
            &mut settings.database.max_rows,
            1..=1_000_000,
            defaults.database.max_rows,
        );
        problems.extend(crate::lsp::Registry::build(&settings).1);
        (settings, problems)
    }
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();

/// Read the settings file, once, at start. Problems come back as messages
/// for the status line.
pub fn load() -> Vec<String> {
    let path = crate::config_dir().map(|dir| dir.join("settings.toml"));
    let (settings, problems) = match path.and_then(|path| std::fs::read_to_string(path).ok()) {
        Some(text) => Settings::parse(&text),
        None => (Settings::default(), Vec::new()),
    };
    let _ = SETTINGS.set(settings);
    problems
}

/// The settings in effect (the defaults until [`load`] ran, as in tests).
pub fn get() -> &'static Settings {
    SETTINGS.get_or_init(Settings::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_keys_keep_their_defaults() {
        let (settings, problems) = Settings::parse("[python]\nserver = \"basedpyright\"\n");
        assert_eq!(problems.len(), 0);
        assert_eq!(settings.python.server, PythonServer::Basedpyright);
        assert!(settings.python.ruff);
        assert_eq!(settings.editor.tab_size, 4);
        assert_eq!(settings.database, Database::default());
    }

    #[test]
    fn mistakes_are_reported_and_defaults_used() {
        let (settings, problems) = Settings::parse("[editor]\ntab_size = 0\n");
        assert_eq!(settings.editor.tab_size, 4);
        assert!(problems.iter().any(|problem| problem.contains("tab_size")));
        let (settings, problems) = Settings::parse("[editor]\ntabsize = 2\n");
        assert_eq!(settings, Settings::default());
        assert!(problems.iter().any(|problem| problem.contains("tabsize")));
        let (_, problems) = Settings::parse("[python]\nserver = \"jedi\"\n");
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn the_preferred_server_comes_first() {
        assert_eq!(
            PythonServer::Pyright.order(),
            [
                PythonServer::Pyright,
                PythonServer::AstralTy,
                PythonServer::Basedpyright,
                PythonServer::Pylsp
            ]
        );
    }
}
