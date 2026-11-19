//! Actions and the key bindings that trigger them.
//!
//! Defaults follow PyCharm. Ghostty speaks the kitty keyboard protocol, so chords
//! like `Ctrl+Shift+N` arrive intact there; terminals without it (xterm.js based
//! ones such as the T3 integrated terminal) fold `Ctrl+Shift+N` into `Ctrl+N`, so
//! every shifted chord also gets a fallback that survives legacy encoding.
//!
//! Users override bindings in `$XDG_CONFIG_HOME/sanitty/keymap.toml`:
//!
//! ```toml
//! [bindings]
//! "ctrl+b" = "goto-definition"
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
    /// or "references" without knowing PyCharm's exact label.
    pub fn keywords(self) -> &'static str {
        match self {
            Action::Launcher => "search everywhere launcher anything",
            Action::GotoFile => "open file find file",
            Action::GotoSymbol => "symbol class function method definition",
            Action::GotoLine => "line number jump",
            Action::SearchInFiles => "grep search text project find in path",
            Action::RecentFiles => "recent switcher history",
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
            .find(|(a, ..)| *a == self)
            .map_or("", |(_, l, _)| l)
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
    pub fn from_event(ev: &KeyEvent) -> Self {
        let mut mods =
            ev.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match ev.code {
            KeyCode::Char(c) if c.is_ascii_uppercase() => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Char(c.to_ascii_lowercase())
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
                    )
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
        k if k.len() > 1 && k.starts_with('f') => KeyCode::F(k[1..].parse().ok()?),
        k if k.chars().count() == 1 => KeyCode::Char(k.chars().next()?),
        _ => return None,
    })
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            f.write_str("Ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            f.write_str("Alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            f.write_str("Shift+")?;
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char(c) => write!(f, "{}", c.to_ascii_uppercase()),
            KeyCode::F(n) => write!(f, "F{n}"),
            other => write!(f, "{other:?}"),
        }
    }
}

/// Default bindings. The first entry for an action is the one shown in the hint
/// bar and All Keys: it arrives in both T3's terminal and Ghostty (see the key
/// check reports). Later entries are PyCharm keys kept for terminals that do
/// deliver them.
pub const DEFAULTS: &[(&str, Action)] = &[
    ("alt+q", Action::Quit),
    ("ctrl+shift+q", Action::Quit),
    ("ctrl+s", Action::Save),
    ("ctrl+k", Action::Launcher),
    ("alt+n", Action::GotoFile),
    ("ctrl+shift+n", Action::GotoFile),
    ("ctrl+alt+shift+n", Action::GotoSymbol),
    ("ctrl+g", Action::GotoLine),
    ("alt+f", Action::SearchInFiles),
    ("ctrl+shift+f", Action::SearchInFiles),
    ("ctrl+e", Action::RecentFiles),
    ("alt+1", Action::ToggleExplorer),
    ("f1", Action::AllKeys),
    ("ctrl+f", Action::Find),
    ("ctrl+r", Action::Replace),
    ("f3", Action::FindNext),
    ("shift+f3", Action::FindPrev),
    ("alt+b", Action::GotoDefinition),
    ("ctrl+b", Action::GotoDefinition),
    ("alt+i", Action::GotoImplementation),
    ("ctrl+alt+b", Action::GotoImplementation),
    ("alt+f7", Action::FindUsages),
    ("alt+h", Action::QuickDoc),
    ("alt+6", Action::Problems),
    ("f8", Action::NextProblem),
    ("shift+f8", Action::PrevProblem),
    ("f2", Action::Rename),
    ("shift+f6", Action::Rename),
    ("ctrl+d", Action::DuplicateLine),
    ("ctrl+y", Action::DeleteLine),
    ("ctrl+/", Action::ToggleComment),
    ("ctrl+z", Action::Undo),
    ("ctrl+shift+z", Action::Redo),
    ("alt+z", Action::Redo),
    ("ctrl+c", Action::Copy),
    ("ctrl+x", Action::Cut),
    ("ctrl+v", Action::Paste),
    ("ctrl+a", Action::SelectAll),
    ("alt+w", Action::CloseFile),
    ("ctrl+f4", Action::CloseFile),
    ("ctrl+alt+left", Action::NavBack),
    ("ctrl+alt+right", Action::NavForward),
    ("alt+f12", Action::Shell),
    ("alt+t", Action::Shell),
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
        if let Some(path) = crate::config_dir().map(|d| d.join("keymap.toml"))
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
                            Err(e) => problems.push(format!("keymap.toml: {e}")),
                        }
                    }
                }
                Err(e) => problems.push(format!("{}: {e}", path.display())),
            }
        }
        (Self { map }, problems)
    }

    pub fn lookup(&self, ev: &KeyEvent) -> Option<Action> {
        let chord = Chord::from_event(ev);
        if let Some(a) = self.map.get(&chord) {
            return Some(*a);
        }
        // Legacy terminals send Ctrl+/ as Ctrl+7 or Ctrl+_.
        if chord.mods == KeyModifiers::CONTROL
            && matches!(chord.code, KeyCode::Char('7') | KeyCode::Char('_'))
        {
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
            .filter(|(_, a)| *a == action)
            .filter_map(|(s, _)| Chord::parse(s).ok())
            .filter(|c| self.map.get(c) == Some(&action))
            .collect();
        for (c, a) in &self.map {
            if *a == action && !out.contains(c) {
                out.push(*c);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_letters_normalize() {
        let a = Chord::from_event(&KeyEvent::new(KeyCode::Char('N'), KeyModifiers::CONTROL));
        let b = Chord::from_event(&KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(a, b);
        assert_eq!(a, Chord::parse("ctrl+shift+n").unwrap());
    }

    #[test]
    fn parses_function_keys() {
        assert_eq!(Chord::parse("alt+f7").unwrap().code, KeyCode::F(7));
    }
}
