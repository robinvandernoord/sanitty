//! Actions and the key bindings that trigger them.
//!
//! One default key per action, built on Alt: T3's terminal claims many Ctrl
//! keys and may claim more. Only the universal Ctrl keys (copy, cut, paste,
//! undo, save, select all, find) and `PyCharm`'s F-keys remain. Every default
//! arrived in both T3 and Ghostty in the key check (`sanitty --check-keys`).
//!
//! Users override bindings in `$XDG_CONFIG_HOME/sanitty/keymap.toml`:
//!
//! ```toml
//! [bindings]
//! "f12" = "goto-definition"
//! "alt+n" = "goto-file"
//! "ctrl+w" = "none"          # unbind
//! ```

use std::collections::HashMap;
use std::fmt;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use serde::Deserialize;

/// Everything a key can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    /// Unbinds a key in the user keymap.
    None,
    Quit,
    Save,
    Launcher,
    GotoFile,
    GotoSymbol,
    GotoLine,
    SearchInFiles,
    RecentFiles,
    Review,
    PreviousFile,
    ToggleExplorer,
    AllKeys,
    CheckKeys,
    Find,
    Replace,
    FindNext,
    FindPrev,
    GotoDefinition,
    GotoImplementation,
    FindUsages,
    QuickDoc,
    Problems,
    NextProblem,
    PrevProblem,
    Rename,
    DuplicateLine,
    DeleteLine,
    ToggleComment,
    Reformat,
    Complete,
    Undo,
    Redo,
    Copy,
    CopyLine,
    Cut,
    Paste,
    SelectAll,
    CloseFile,
    NavBack,
    NavForward,
    Shell,
}

impl Action {
    /// Every user-facing action, in cheat-sheet order, with its label and group.
    pub const ALL: &[(Action, &str, &str)] = &[
        (Action::Launcher, "Search Everywhere", "Navigate"),
        (Action::GotoFile, "Go to File", "Navigate"),
        (Action::GotoSymbol, "Go to Symbol", "Navigate"),
        (Action::GotoLine, "Go to Line", "Navigate"),
        (Action::RecentFiles, "Recent Files", "Navigate"),
        (
            Action::Review,
            "Review Changes (diff and comments)",
            "Navigate",
        ),
        (Action::PreviousFile, "Switch to Previous File", "Navigate"),
        (Action::ToggleExplorer, "Project Tree", "Navigate"),
        (Action::NavBack, "Navigate Back", "Navigate"),
        (Action::NavForward, "Navigate Forward", "Navigate"),
        (Action::Find, "Find in File", "Search"),
        (Action::Replace, "Replace in File", "Search"),
        (Action::FindNext, "Find Next", "Search"),
        (Action::FindPrev, "Find Previous", "Search"),
        (Action::SearchInFiles, "Find in Files", "Search"),
        (
            Action::GotoDefinition,
            "Go to Definition (or Usages)",
            "Code",
        ),
        (Action::GotoImplementation, "Go to Implementation", "Code"),
        (Action::FindUsages, "Find Usages", "Code"),
        (
            Action::QuickDoc,
            "Quick Documentation (type and docs)",
            "Code",
        ),
        (Action::Rename, "Rename Symbol", "Code"),
        (Action::Problems, "Problems (all files)", "Code"),
        (
            Action::NextProblem,
            "Next Problem (error or warning)",
            "Code",
        ),
        (Action::PrevProblem, "Previous Problem", "Code"),
        (Action::ToggleComment, "Comment with Line Comment", "Code"),
        (Action::Reformat, "Reformat File", "Code"),
        (Action::Complete, "Code Completion", "Code"),
        (Action::DuplicateLine, "Duplicate Line", "Edit"),
        (Action::DeleteLine, "Delete Line", "Edit"),
        (Action::Undo, "Undo", "Edit"),
        (Action::Redo, "Redo", "Edit"),
        (Action::Copy, "Copy (quits without a selection)", "Edit"),
        (Action::CopyLine, "Copy Line", "Edit"),
        (Action::Cut, "Cut", "Edit"),
        (Action::Paste, "Paste", "Edit"),
        (Action::SelectAll, "Select All", "Edit"),
        (Action::Save, "Save", "File"),
        (Action::CloseFile, "Close File", "File"),
        (Action::AllKeys, "All Keys", "App"),
        (
            Action::CheckKeys,
            "Check Keys (which keys reach sanitty)",
            "App",
        ),
        (Action::Shell, "Shell (suspend to $SHELL)", "App"),
        (Action::Quit, "Quit", "App"),
    ];

    /// Other words people search for, so the launcher finds "jump to source"
    /// or "references" without knowing `PyCharm`'s exact label.
    pub fn keywords(self) -> &'static str {
        match self {
            Action::Launcher => "search everywhere launcher anything",
            Action::GotoFile => "open file find file",
            Action::GotoSymbol => "symbol class function method definition",
            Action::GotoLine => "line number jump",
            Action::SearchInFiles => "grep search text project find in path",
            Action::RecentFiles => "recent switcher history",
            Action::Review => "review diff changes git branch compare comments code review",
            Action::PreviousFile => "previous last file toggle switch back",
            Action::ToggleExplorer => "project tree files sidebar explorer browse",
            Action::AllKeys => "help keys shortcuts keymap keybindings cheat sheet",
            Action::CheckKeys => "check keys test keyboard terminal",
            Action::Find => "find search",
            Action::Replace => "replace substitute",
            Action::FindNext => "next match occurrence",
            Action::FindPrev => "previous match occurrence",
            Action::GotoDefinition => "definition declaration jump to source goto navigate",
            Action::GotoImplementation => "implementation subclass override jump",
            Action::FindUsages => "usages references callers who uses",
            Action::Rename => "rename refactor symbol variable",
            Action::QuickDoc => "type hover docs documentation info signature quick",
            Action::Problems => "problems errors warnings diagnostics list lint",
            Action::NextProblem | Action::PrevProblem => {
                "problem error warning diagnostic lint issue highlighted"
            }
            Action::ToggleComment => "comment uncomment",
            Action::Reformat => "format formatter reformat code style ruff rustfmt tidy",
            Action::Complete => "complete completion autocomplete suggest suggestions intellisense",
            Action::DuplicateLine => "duplicate clone copy line",
            Action::DeleteLine => "delete remove kill line",
            Action::Undo => "undo revert",
            Action::Redo => "redo",
            Action::Copy | Action::CopyLine => "copy clipboard",
            Action::Cut => "cut clipboard",
            Action::Paste => "paste clipboard",
            Action::SelectAll => "select all",
            Action::Save => "save write",
            Action::CloseFile => "close file tab buffer",
            Action::NavBack => "back history previous location",
            Action::NavForward => "forward history next location",
            Action::Shell => "shell terminal bash console",
            Action::Quit => "quit exit",
            Action::None => "",
        }
    }

    pub fn label(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(action, ..)| *action == self)
            .map_or("", |(_, label, _)| label)
    }
}

/// A normalized key chord: modifiers plus a key code, with letters lowercased and
/// SHIFT made explicit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub mods: KeyModifiers,
    pub code: KeyCode,
}

impl Chord {
    /// Normalize a terminal key event so `Ctrl+Shift+N` matches however the
    /// terminal spelled it (`N` with SHIFT, `N` without, or `n` with SHIFT).
    pub fn from_event(event: &KeyEvent) -> Self {
        let mut mods =
            event.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match event.code {
            KeyCode::Char(chr) if chr.is_ascii_uppercase() => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Char(chr.to_ascii_lowercase())
            }
            KeyCode::BackTab => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Tab
            }
            other => other,
        };
        Self { mods, code }
    }

    /// Whether a terminal without the kitty keyboard protocol can send this
    /// chord: those fold Ctrl+Shift+X into Ctrl+X and Ctrl+Tab into Tab.
    pub fn survives_legacy(&self) -> bool {
        let ctrl = self.mods.contains(KeyModifiers::CONTROL);
        !(ctrl && (self.mods.contains(KeyModifiers::SHIFT) || self.code == KeyCode::Tab))
    }

    /// Parse `"ctrl+shift+n"`, `"alt+f7"`, `"shift+f6"`, `"ctrl+alt+left"`.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let mut mods = KeyModifiers::NONE;
        let mut code = None;
        for part in spec.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "meta" | "option" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                key => {
                    code = Some(
                        parse_code(key)
                            .ok_or_else(|| format!("unknown key {key:?} in {spec:?}"))?,
                    );
                }
            }
        }
        let code = code.ok_or_else(|| format!("no key in {spec:?}"))?;
        Ok(Self { mods, code })
    }
}

fn parse_code(key: &str) -> Option<KeyCode> {
    Some(match key {
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "space" => KeyCode::Char(' '),
        "slash" => KeyCode::Char('/'),
        "plus" => KeyCode::Char('+'),
        _ if key.len() > 1 && key.starts_with('f') => {
            KeyCode::F(key.strip_prefix('f')?.parse().ok()?)
        }
        _ if key.chars().count() == 1 => KeyCode::Char(key.chars().next()?),
        _ => return None,
    })
}

impl fmt::Display for Chord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            formatter.write_str("Ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            formatter.write_str("Alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            formatter.write_str("Shift+")?;
        }
        match self.code {
            KeyCode::Char(' ') => formatter.write_str("Space"),
            KeyCode::Char(chr) => write!(formatter, "{}", chr.to_ascii_uppercase()),
            KeyCode::F(num) => write!(formatter, "F{num}"),
            other => write!(formatter, "{other:?}"),
        }
    }
}

/// Default bindings: exactly one key per action. New ones come from the free
/// keys in PLAN.md (keys that arrived in both terminals), preferring Alt.
pub const DEFAULTS: &[(&str, Action)] = &[
    // Navigate
    ("alt+k", Action::Launcher),
    ("alt+n", Action::GotoFile),
    ("alt+s", Action::GotoSymbol),
    ("alt+g", Action::GotoLine),
    ("alt+e", Action::RecentFiles),
    ("alt+9", Action::Review),
    ("alt+1", Action::ToggleExplorer),
    ("alt+,", Action::NavBack),
    ("alt+.", Action::NavForward),
    // Search
    ("ctrl+f", Action::Find),
    ("alt+r", Action::Replace),
    ("f3", Action::FindNext),
    ("shift+f3", Action::FindPrev),
    ("alt+f", Action::SearchInFiles),
    // Code
    ("alt+b", Action::GotoDefinition),
    ("alt+i", Action::GotoImplementation),
    ("alt+f7", Action::FindUsages),
    ("alt+h", Action::QuickDoc),
    ("f2", Action::Rename),
    ("alt+6", Action::Problems),
    ("f8", Action::NextProblem),
    ("shift+f8", Action::PrevProblem),
    ("alt+/", Action::ToggleComment),
    ("alt+l", Action::Reformat),
    ("alt+j", Action::Complete),
    // Edit
    ("alt+d", Action::DuplicateLine),
    ("alt+y", Action::DeleteLine),
    ("ctrl+z", Action::Undo),
    ("alt+z", Action::Redo),
    ("ctrl+c", Action::Copy),
    ("ctrl+x", Action::Cut),
    ("ctrl+v", Action::Paste),
    ("ctrl+a", Action::SelectAll),
    // File and app
    ("ctrl+s", Action::Save),
    ("alt+w", Action::CloseFile),
    ("f1", Action::AllKeys),
    ("alt+t", Action::Shell),
    ("alt+q", Action::Quit),
];

#[derive(Deserialize, Default)]
struct UserKeymap {
    #[serde(default)]
    bindings: HashMap<String, Action>,
}

/// The resolved chord table.
pub struct Keymap {
    map: HashMap<Chord, Action>,
}

impl Keymap {
    /// Defaults overlaid with the user's `keymap.toml`. Problems are returned as
    /// messages, never fatal: a broken user file leaves the defaults in place.
    pub fn load() -> (Self, Vec<String>) {
        let mut map = HashMap::new();
        for (spec, action) in DEFAULTS {
            if let Ok(chord) = Chord::parse(spec) {
                map.insert(chord, *action);
            }
        }
        let mut problems = Vec::new();
        if let Some(path) = crate::config_dir().map(|dir| dir.join("keymap.toml"))
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            match toml::from_str::<UserKeymap>(&text) {
                Ok(user) => {
                    for (spec, action) in user.bindings {
                        match Chord::parse(&spec) {
                            Ok(chord) if action == Action::None => {
                                map.remove(&chord);
                            }
                            Ok(chord) => {
                                map.insert(chord, action);
                            }
                            Err(err) => problems.push(format!("keymap.toml: {err}")),
                        }
                    }
                }
                Err(err) => problems.push(format!("{}: {err}", path.display())),
            }
        }
        (Self { map }, problems)
    }

    pub fn lookup(&self, event: &KeyEvent) -> Option<Action> {
        let chord = Chord::from_event(event);
        if let Some(action) = self.map.get(&chord) {
            return Some(*action);
        }
        // Legacy terminals send Ctrl+/ as Ctrl+7 or Ctrl+_.
        if chord.mods == KeyModifiers::CONTROL && matches!(chord.code, KeyCode::Char('7' | '_')) {
            return self
                .map
                .get(&Chord {
                    mods: KeyModifiers::CONTROL,
                    code: KeyCode::Char('/'),
                })
                .copied();
        }
        None
    }

    /// Every chord bound to `action`, defaults first, for display.
    pub fn chords_for(&self, action: Action) -> Vec<Chord> {
        let mut out: Vec<Chord> = DEFAULTS
            .iter()
            .filter(|(_, bound)| *bound == action)
            .filter_map(|(spec, _)| Chord::parse(spec).ok())
            .filter(|chord| self.map.get(chord) == Some(&action))
            .collect();
        for (chord, bound) in &self.map {
            if *bound == action && !out.contains(chord) {
                out.push(*chord);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_letters_normalize() -> Result<(), String> {
        let upper = Chord::from_event(&KeyEvent::new(KeyCode::Char('N'), KeyModifiers::CONTROL));
        let shifted = Chord::from_event(&KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(upper, shifted);
        assert_eq!(upper, Chord::parse("ctrl+shift+n")?);
        Ok(())
    }

    #[test]
    fn parses_function_keys() -> Result<(), String> {
        assert_eq!(Chord::parse("alt+f7")?.code, KeyCode::F(7));
        Ok(())
    }
}
