//! Check Keys: a guided test of which shortcuts reach sanitty.
//!
//! Terminals (T3's in particular) swallow some chords, and terminals without
//! the kitty keyboard protocol fold others (`Ctrl+Shift+N` arrives as
//! `Ctrl+N`). This screen asks for every bound chord and for a long list of
//! free ones, family by family, records what actually arrived, and writes a
//! Markdown report: which bound keys work, and which free keys are usable
//! for future bindings.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

use crate::keymap::Action;
use crate::keymap::Chord;
use crate::keymap::DEFAULTS;

/// Every chord to check, by family. Within a family the keys follow the
/// keyboard, left to right and top to bottom: number row, then the letter rows
/// with the punctuation at their ends, then Space and the keys around the
/// arrows. Bound and free keys are mixed; the bindings come from the keymap.
///
/// Only `Shift+Insert` and `Ctrl+Alt+Delete` are left out: one pastes, the
/// other can log you out. Keys a terminal or desktop may act on are in
/// `RISKY`, with a warning on their step.
const FAMILIES: &[(&str, &[&str])] = &[
    (
        "Function keys",
        &[
            "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12",
        ],
    ),
    (
        "Shift + function keys",
        &[
            "shift+f1",
            "shift+f2",
            "shift+f3",
            "shift+f4",
            "shift+f5",
            "shift+f6",
            "shift+f7",
            "shift+f8",
            "shift+f9",
            "shift+f10",
            "shift+f11",
            "shift+f12",
        ],
    ),
    (
        "Ctrl + function keys",
        &[
            "ctrl+f1", "ctrl+f2", "ctrl+f3", "ctrl+f4", "ctrl+f5", "ctrl+f6", "ctrl+f7", "ctrl+f8",
            "ctrl+f9", "ctrl+f10", "ctrl+f11", "ctrl+f12",
        ],
    ),
    (
        "Alt + function keys",
        &[
            "alt+f1", "alt+f2", "alt+f3", "alt+f4", "alt+f5", "alt+f6", "alt+f7", "alt+f8",
            "alt+f9", "alt+f10", "alt+f11", "alt+f12",
        ],
    ),
    (
        "Ctrl + keys",
        &[
            "ctrl+1",
            "ctrl+2",
            "ctrl+3",
            "ctrl+4",
            "ctrl+5",
            "ctrl+6",
            "ctrl+7",
            "ctrl+8",
            "ctrl+9",
            "ctrl+0",
            "ctrl+-",
            "ctrl+=",
            "ctrl+backspace", //
            "ctrl+q",
            "ctrl+w",
            "ctrl+e",
            "ctrl+r",
            "ctrl+t",
            "ctrl+y",
            "ctrl+u",
            "ctrl+i",
            "ctrl+o",
            "ctrl+p",
            "ctrl+[",
            "ctrl+]",
            "ctrl+\\", //
            "ctrl+a",
            "ctrl+s",
            "ctrl+d",
            "ctrl+f",
            "ctrl+g",
            "ctrl+h",
            "ctrl+j",
            "ctrl+k",
            "ctrl+l",
            "ctrl+;",
            "ctrl+'",
            "ctrl+enter", //
            "ctrl+z",
            "ctrl+x",
            "ctrl+c",
            "ctrl+v",
            "ctrl+b",
            "ctrl+n",
            "ctrl+m",
            "ctrl+,",
            "ctrl+.",
            "ctrl+/", //
            "ctrl+space",
            "ctrl+insert",
            "ctrl+delete",
            "ctrl+home",
            "ctrl+end",
            "ctrl+pageup",
            "ctrl+pagedown",
            "ctrl+up",
            "ctrl+down",
            "ctrl+left",
            "ctrl+right",
        ],
    ),
    (
        "Ctrl + Shift + letter",
        &[
            "ctrl+shift+q",
            "ctrl+shift+w",
            "ctrl+shift+e",
            "ctrl+shift+r",
            "ctrl+shift+t",
            "ctrl+shift+y",
            "ctrl+shift+u",
            "ctrl+shift+i",
            "ctrl+shift+o",
            "ctrl+shift+p", //
            "ctrl+shift+a",
            "ctrl+shift+s",
            "ctrl+shift+d",
            "ctrl+shift+f",
            "ctrl+shift+g",
            "ctrl+shift+h",
            "ctrl+shift+j",
            "ctrl+shift+k",
            "ctrl+shift+l", //
            "ctrl+shift+z",
            "ctrl+shift+x",
            "ctrl+shift+c",
            "ctrl+shift+v",
            "ctrl+shift+b",
            "ctrl+shift+n",
            "ctrl+shift+m",
        ],
    ),
    (
        "Alt + keys",
        &[
            "alt+1",
            "alt+2",
            "alt+3",
            "alt+4",
            "alt+5",
            "alt+6",
            "alt+7",
            "alt+8",
            "alt+9",
            "alt+0",
            "alt+-",
            "alt+=",
            "alt+backspace", //
            "alt+q",
            "alt+w",
            "alt+e",
            "alt+r",
            "alt+t",
            "alt+y",
            "alt+u",
            "alt+i",
            "alt+o",
            "alt+p",
            "alt+[",
            "alt+]",
            "alt+\\", //
            "alt+a",
            "alt+s",
            "alt+d",
            "alt+f",
            "alt+g",
            "alt+h",
            "alt+j",
            "alt+k",
            "alt+l",
            "alt+;",
            "alt+'",
            "alt+enter", //
            "alt+z",
            "alt+x",
            "alt+c",
            "alt+v",
            "alt+b",
            "alt+n",
            "alt+m",
            "alt+,",
            "alt+.",
            "alt+/", //
            "alt+space",
            "alt+up",
            "alt+down",
            "alt+left",
            "alt+right",
        ],
    ),
    (
        "Ctrl + Alt + keys",
        &[
            "ctrl+alt+q",
            "ctrl+alt+w",
            "ctrl+alt+e",
            "ctrl+alt+r",
            "ctrl+alt+t",
            "ctrl+alt+y",
            "ctrl+alt+u",
            "ctrl+alt+i",
            "ctrl+alt+o",
            "ctrl+alt+p", //
            "ctrl+alt+a",
            "ctrl+alt+s",
            "ctrl+alt+d",
            "ctrl+alt+f",
            "ctrl+alt+g",
            "ctrl+alt+h",
            "ctrl+alt+j",
            "ctrl+alt+k",
            "ctrl+alt+l", //
            "ctrl+alt+z",
            "ctrl+alt+x",
            "ctrl+alt+c",
            "ctrl+alt+v",
            "ctrl+alt+b",
            "ctrl+alt+n",
            "ctrl+alt+shift+n",
            "ctrl+alt+m", //
            "ctrl+alt+up",
            "ctrl+alt+down",
            "ctrl+alt+left",
            "ctrl+alt+right",
        ],
    ),
    (
        "Shift + other keys",
        &["shift+tab", "shift+enter", "shift+delete"],
    ),
];

/// Keys a terminal, T3 or the desktop may act on, with what to expect. If one
/// does something, undo it, come back and press Esc; progress is saved after
/// every key.
const RISKY: &[(&str, &str)] = &[
    ("f11", "fullscreen in some terminals"),
    ("alt+f1", "opens a desktop menu on some desktops"),
    ("alt+f2", "opens a run dialog on some desktops"),
    ("alt+f3", "may open a desktop search"),
    (
        "alt+f4",
        "CLOSES THE WINDOW on most desktops; your progress is saved",
    ),
    ("alt+f7", "moves the window on some desktops"),
    ("alt+f8", "resizes the window on some desktops"),
    ("alt+f10", "maximises the window on some desktops"),
    ("ctrl+-", "zooms out in some terminals"),
    ("ctrl+=", "zooms in in some terminals"),
    ("ctrl+0", "resets the zoom in some terminals"),
    ("ctrl+q", "T3 asks whether to quit; answer no"),
    ("ctrl+w", "may close the tab"),
    ("ctrl+t", "may open a new tab"),
    ("ctrl+n", "may open a new window"),
    (
        "ctrl+[",
        "is Esc to most terminals; it will show as arriving as Esc",
    ),
    ("ctrl+h", "is Backspace to most terminals"),
    ("ctrl+enter", "fullscreen in Ghostty"),
    ("ctrl+,", "opens the terminal's settings in Ghostty"),
    ("ctrl+shift+q", "quits Ghostty"),
    ("ctrl+shift+w", "closes the tab in Ghostty"),
    ("ctrl+shift+t", "opens a tab in Ghostty"),
    ("ctrl+shift+n", "opens a window in Ghostty"),
    ("ctrl+shift+c", "copies in most terminals"),
    (
        "ctrl+shift+v",
        "pastes in most terminals (a paste is ignored here)",
    ),
    (
        "ctrl+shift+u",
        "starts Unicode input on GTK desktops; press Esc twice",
    ),
    (
        "ctrl+shift+j",
        "pastes a file path in Ghostty (the paste is ignored here)",
    ),
    ("alt+space", "opens the window menu on some desktops"),
    (
        "alt+[",
        "starts an escape sequence in older terminals; may need a second key",
    ),
    ("ctrl+alt+t", "opens a terminal on Ubuntu"),
    ("ctrl+alt+l", "locks the screen on some desktops"),
    ("ctrl+alt+d", "shows the desktop on some desktops"),
    ("ctrl+alt+up", "switches workspace on some desktops"),
    ("ctrl+alt+down", "switches workspace on some desktops"),
    ("ctrl+alt+left", "switches workspace on some desktops"),
    ("ctrl+alt+right", "switches workspace on some desktops"),
];

/// Keys the editor itself uses (not in the keymap), so they are not free.
const EDITOR: &[(&str, &str)] = &[
    ("ctrl+backspace", "editor: delete word"),
    ("ctrl+delete", "editor: delete word"),
    ("ctrl+home", "editor: start of file"),
    ("ctrl+end", "editor: end of file"),
    ("ctrl+left", "editor: word left"),
    ("ctrl+right", "editor: word right"),
];

fn lookup(table: &[(&str, &'static str)], chord: Chord) -> Option<&'static str> {
    table
        .iter()
        .find(|(spec, _)| Chord::parse(spec).is_ok_and(|parsed| parsed == chord))
        .map(|(_, text)| *text)
}

/// Keys closer together than this are a paste or a terminal's macro, not
/// someone pressing a key: only the first of them counts.
const BURST: Duration = Duration::from_millis(40);

/// Width of one key cell in the family grid, padding included.
const COLUMN_WIDTH: usize = 34;

enum Answer {
    Works,
    /// What arrived instead, as shown (`Ctrl+N`).
    ArrivedAs(String),
    Missing,
}

struct Step {
    family: &'static str,
    chord: Chord,
    /// What the keymap binds it to; empty for a free key.
    actions: Vec<Action>,
    /// What the editor uses it for, when it does.
    editor: Option<&'static str>,
    /// What a terminal or desktop may do with it.
    risk: Option<&'static str>,
}

pub struct KeyCheck {
    steps: Vec<Step>,
    results: Vec<Answer>,
    /// Where the report is saved (after every step, so a closed window loses
    /// nothing), once saving has worked.
    pub saved: Option<PathBuf>,
    /// When the last key arrived, to drop bursts (see `BURST`).
    last_key: Option<Instant>,
    /// How many steps were picked up from an unfinished earlier run.
    resumed: usize,
}

pub enum Outcome {
    Pending,
    /// A step was answered; the report so far is ready to save.
    Progress(String),
    /// The run is over; the report text is ready to save and copy.
    Finished(String),
    Close,
}

impl KeyCheck {
    pub fn new() -> Self {
        let mut steps: Vec<Step> = Vec::new();
        for (family, keys) in FAMILIES {
            for spec in *keys {
                let Ok(chord) = Chord::parse(spec) else {
                    continue;
                };
                if steps.iter().any(|step| step.chord == chord) {
                    continue;
                }
                let actions = DEFAULTS
                    .iter()
                    .filter(|(spec, _)| Chord::parse(spec).is_ok_and(|parsed| parsed == chord))
                    .map(|(_, action)| *action)
                    .collect();
                steps.push(Step {
                    family,
                    chord,
                    actions,
                    editor: lookup(EDITOR, chord),
                    risk: lookup(RISKY, chord),
                });
            }
        }
        let mut check = Self {
            steps,
            results: Vec::new(),
            saved: None,
            last_key: None,
            resumed: 0,
        };
        check.resume();
        check
    }

    /// Where this terminal's report goes: `~/.config/sanitty/keycheck-<terminal>.md`.
    pub fn report_path() -> Option<PathBuf> {
        let name = Self::terminal_name().replace(['/', ' '], "-");
        crate::config_dir().map(|dir| dir.join(format!("keycheck-{name}.md")))
    }

    /// Pick up an unfinished run from its saved report: the answers to the
    /// first steps, in order, as far as they go. A finished report starts over.
    fn resume(&mut self) {
        let Some(text) = Self::report_path().and_then(|path| std::fs::read_to_string(path).ok())
        else {
            return;
        };
        if text.contains(&format!("{0} of {0} keys checked", self.steps.len())) {
            return;
        }
        let mut answers = std::collections::HashMap::new();
        for line in text.lines() {
            let cells: Vec<&str> = line.split(" | ").collect();
            if let [key, _, result] = cells.as_slice() {
                let key = key.trim_start_matches("| ");
                let result = match result.trim_end_matches(" |") {
                    "ok" => Answer::Works,
                    "does not arrive" => Answer::Missing,
                    other => match other.strip_prefix("arrives as ") {
                        Some(got) => Answer::ArrivedAs(got.to_string()),
                        None => continue,
                    },
                };
                answers.insert(key.to_string(), result);
            }
        }
        for step in &self.steps {
            match answers.remove(&step.chord.to_string()) {
                Some(result) => self.results.push(result),
                None => break,
            }
        }
        self.resumed = self.results.len();
    }

    /// The first step of the group that `step` is in.
    fn group_start(&self, step: usize) -> usize {
        let family = self.steps.get(step).map(|found| found.family);
        let mut start = step.min(self.steps.len());
        while start > 0 && self.steps.get(start - 1).map(|found| found.family) == family {
            start -= 1;
        }
        start
    }

    fn done(&self) -> bool {
        self.results.len() >= self.steps.len()
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        if self.done() {
            return if key.code == KeyCode::Esc || key.code == KeyCode::Enter {
                Outcome::Close
            } else {
                Outcome::Pending
            };
        }
        let now = Instant::now();
        let burst = self.last_key.is_some_and(|last| now - last < BURST);
        self.last_key = Some(now);
        if burst {
            return Outcome::Pending;
        }
        // Plain Left, PageUp and q are never asked for, so they steer: back
        // one step, back to the start of the group (or the one before), stop.
        if key.modifiers == KeyModifiers::NONE {
            match key.code {
                KeyCode::Left => {
                    self.results.pop();
                    return Outcome::Progress(self.report());
                }
                KeyCode::PageUp => {
                    let here = self.results.len();
                    let mut back_to = self.group_start(here);
                    if back_to == here && here > 0 {
                        back_to = self.group_start(here - 1);
                    }
                    self.results.truncate(back_to);
                    return Outcome::Progress(self.report());
                }
                KeyCode::Char('q') => return Outcome::Close,
                _ => {}
            }
        }
        let Some(expected) = self.steps.get(self.results.len()).map(|step| step.chord) else {
            return Outcome::Pending;
        };
        // Esc means "nothing arrived"; Backspace also goes back one step.
        // Except where the asked key may itself arrive as one of them
        // (Ctrl+[ as Esc, Ctrl+H as Backspace): then it is the answer.
        let asks = |code: KeyCode, letter: char| {
            expected.code == code
                || (expected.mods == KeyModifiers::CONTROL
                    && expected.code == KeyCode::Char(letter))
        };
        let result = match key.code {
            KeyCode::Esc if key.modifiers == KeyModifiers::NONE && asks(KeyCode::Esc, '[') => {
                Answer::ArrivedAs(Chord::from_event(key).to_string())
            }
            KeyCode::Esc => Answer::Missing,
            KeyCode::Backspace
                if key.modifiers == KeyModifiers::NONE && !asks(KeyCode::Backspace, 'h') =>
            {
                self.results.pop();
                return Outcome::Progress(self.report());
            }
            _ => {
                let got = Chord::from_event(key);
                if got == expected {
                    Answer::Works
                } else {
                    Answer::ArrivedAs(got.to_string())
                }
            }
        };
        self.results.push(result);
        if self.done() {
            Outcome::Finished(self.report())
        } else {
            Outcome::Progress(self.report())
        }
    }

    /// Which terminal this is, as far as the environment tells.
    /// `TERM_PROGRAM` wins (tmux, ghostty), then T3's own variables, then `TERM`.
    pub fn terminal_name() -> String {
        if let Ok(program) = std::env::var("TERM_PROGRAM")
            && !program.is_empty()
        {
            return program;
        }
        if std::env::vars().any(|(name, _)| name.starts_with("T3CODE_")) {
            return "t3".into();
        }
        std::env::var("TERM").unwrap_or_else(|_| "unknown".into())
    }

    fn describe(result: &Answer) -> String {
        match result {
            Answer::Works => "ok".to_string(),
            Answer::ArrivedAs(chord) => format!("arrives as {chord}"),
            Answer::Missing => "does not arrive".to_string(),
        }
    }

    fn uses(step: &Step) -> String {
        if let Some(editor) = step.editor {
            editor.to_string()
        } else if step.actions.is_empty() {
            "free".to_string()
        } else {
            let names: Vec<&str> = step.actions.iter().map(|action| action.label()).collect();
            names.join(", ")
        }
    }

    pub fn report(&self) -> String {
        let mut out = format!(
            "# sanitty key check: {}\n\nTERM={}, TERM_PROGRAM={}\n\n{} of {} keys checked.\n",
            Self::terminal_name(),
            std::env::var("TERM").unwrap_or_default(),
            std::env::var("TERM_PROGRAM").unwrap_or_default(),
            self.results.len(),
            self.steps.len(),
        );
        let answered: Vec<(&Step, &Answer)> = self.steps.iter().zip(&self.results).collect();

        // The summaries first: what needs attention, and what is free to use.
        let broken: Vec<String> = answered
            .iter()
            .filter(|(step, result)| !step.actions.is_empty() && !matches!(result, Answer::Works))
            .map(|(step, result)| {
                format!(
                    "- {} ({}): {}",
                    step.chord,
                    Self::uses(step),
                    Self::describe(result)
                )
            })
            .collect();
        out.push_str("\n## Bound keys that do not work here\n\n");
        if broken.is_empty() {
            out.push_str("None.\n");
        } else {
            out.push_str(&broken.join("\n"));
            out.push('\n');
        }

        out.push_str("\n## Free keys that work here\n");
        let mut family = "";
        let mut any = false;
        for (step, result) in &answered {
            if !step.actions.is_empty() || step.editor.is_some() || !matches!(result, Answer::Works)
            {
                continue;
            }
            if step.family == family {
                out.push_str(", ");
            } else {
                family = step.family;
                let _ = write!(out, "\n- {family}: ");
            }
            out.push_str(&step.chord.to_string());
            any = true;
        }
        out.push_str(if any { "\n" } else { "\nNone yet.\n" });

        // Then every key, by family.
        let mut family = "";
        for (step, result) in &answered {
            if step.family != family {
                family = step.family;
                let _ = write!(
                    out,
                    "\n## {family}\n\n| Key | Used for | Result |\n|---|---|---|\n"
                );
            }
            let _ = writeln!(
                out,
                "| {} | {} | {} |",
                step.chord,
                Self::uses(step),
                Self::describe(result)
            );
        }
        out
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &karet_theme::Theme) {
        frame.render_widget(Clear, area);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(" Check Keys ")
            .style(crate::styles::surface(theme));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let current = self.results.len();
        let mut lines = if let Some(step) = self.steps.get(current) {
            self.step_lines(step, current, theme)
        } else {
            self.done_lines(theme)
        };
        lines.push(Line::from(""));
        self.push_family(&mut lines, current, inner, theme);
        frame.render_widget(Paragraph::new(lines), inner);
    }

    /// The prompt for the step being asked, with its warnings.
    fn step_lines(
        &self,
        step: &Step,
        current: usize,
        theme: &karet_theme::Theme,
    ) -> Vec<Line<'static>> {
        let base = crate::styles::surface(theme);
        let muted = theme.style(karet_core::ThemeRole::Muted);
        let warn = theme.style(karet_core::ThemeRole::DiagnosticWarning);
        let mut lines = vec![
            Line::from(vec![
                Span::raw(format!(
                    "  Step {} of {}: press  ",
                    current + 1,
                    self.steps.len()
                )),
                Span::styled(
                    step.chord.to_string(),
                    base.add_modifier(Modifier::BOLD | Modifier::REVERSED),
                ),
                Span::styled(format!("   ({})", Self::uses(step)), muted),
            ]),
            Line::styled(
                "  Nothing happens? Esc marks it as not arriving.   Left: back one   PageUp: back a group   q: stop (saved; run again to continue)",
                muted,
            ),
        ];
        if self.resumed > 0 && current >= self.resumed {
            lines.push(Line::styled(
                format!(
                    "  Continuing an unfinished run: {} keys were already done. Delete the report to start over.",
                    self.resumed
                ),
                muted,
            ));
        }
        if let Some(risk) = step.risk {
            lines.push(Line::styled(
                format!("  Careful: {} {risk}.", step.chord),
                warn,
            ));
            lines.push(Line::styled(
                "  If it does something, undo it, come back here and press Esc. Progress is saved after every key.",
                warn,
            ));
        }
        lines
    }

    /// The closing message once every step is answered.
    fn done_lines(&self, theme: &karet_theme::Theme) -> Vec<Line<'static>> {
        let muted = theme.style(karet_core::ThemeRole::Muted);
        let saved = match &self.saved {
            Some(path) => Line::from(format!("  Done. Report saved to {}", path.display())),
            None => Line::from("  Done. The report could not be saved to ~/.config/sanitty/"),
        };
        vec![
            saved,
            Line::styled(
                "  It was also sent to the clipboard, if this terminal allows that (OSC 52).",
                muted,
            ),
            Line::styled("  Esc or Enter closes this screen.", muted),
        ]
    }

    /// The current family's keys (the last family once done), in columns
    /// that fill the room left below `lines`.
    fn push_family(
        &self,
        lines: &mut Vec<Line<'static>>,
        current: usize,
        inner: Rect,
        theme: &karet_theme::Theme,
    ) {
        let base = crate::styles::surface(theme);
        let family = self
            .steps
            .get(current)
            .or(self.steps.last())
            .map_or("", |step| step.family);
        let index = FAMILIES
            .iter()
            .position(|(name, _)| *name == family)
            .unwrap_or(0);
        lines.push(Line::styled(
            format!("  {family}  (group {} of {})", index + 1, FAMILIES.len()),
            base.add_modifier(Modifier::BOLD),
        ));
        let cols = (usize::from(inner.width) / COLUMN_WIDTH).max(1);
        let rows_avail = usize::from(inner.height).saturating_sub(lines.len()).max(1);
        let entries = self.family_entries(family, theme);
        let per_col = entries.len().div_ceil(cols).min(rows_avail);
        for row in 0..per_col {
            let mut spans = Vec::new();
            for col in 0..cols {
                if let Some(entry) = entries.get(col * per_col + row) {
                    spans.extend(
                        entry
                            .spans
                            .iter()
                            .cloned()
                            .map(|span| span.patch_style(entry.style)),
                    );
                }
            }
            lines.push(Line::from(spans));
        }
    }

    /// One padded cell per key of `family`, marked with its answer so far.
    fn family_entries(&self, family: &str, theme: &karet_theme::Theme) -> Vec<Line<'static>> {
        let muted = theme.style(karet_core::ThemeRole::Muted);
        let good = theme.style(karet_core::ThemeRole::DiagnosticHint);
        let warn = theme.style(karet_core::ThemeRole::DiagnosticWarning);
        let bad = theme.style(karet_core::ThemeRole::DiagnosticError);
        self.steps
            .iter()
            .zip(self.results.iter().map(Some).chain(std::iter::repeat(None)))
            .filter(|(step, _)| step.family == family)
            .map(|(step, result)| {
                let (mark, style, note) = match result {
                    Some(Answer::Works) => ("ok", good, String::new()),
                    Some(Answer::ArrivedAs(chord)) => ("as", warn, format!(" -> {chord}")),
                    Some(Answer::Missing) => ("--", bad, String::new()),
                    None => ("  ", muted, String::new()),
                };
                let free = if step.actions.is_empty() && step.editor.is_none() {
                    " (free)"
                } else {
                    ""
                };
                let text = format!("{mark} {}{note}{free}", step.chord);
                Line::styled(
                    format!(
                        "  {:<pad$}",
                        text.chars().take(COLUMN_WIDTH - 3).collect::<String>(),
                        pad = COLUMN_WIDTH - 2
                    ),
                    style,
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new default binding must be added to a family, or it goes unchecked.
    #[test]
    fn every_default_binding_is_checked() -> Result<(), Box<dyn std::error::Error>> {
        let check = KeyCheck::new();
        for (spec, _) in DEFAULTS {
            let chord = Chord::parse(spec).map_err(|err| format!("{spec}: {err}"))?;
            assert!(
                check.steps.iter().any(|step| step.chord == chord),
                "{spec} is bound but not in FAMILIES"
            );
        }
        Ok(())
    }

    #[test]
    fn every_listed_key_parses_once() -> Result<(), Box<dyn std::error::Error>> {
        let mut seen = Vec::new();
        for (_, keys) in FAMILIES {
            for spec in *keys {
                let chord = Chord::parse(spec).map_err(|err| format!("{spec}: {err}"))?;
                assert!(!seen.contains(&chord), "{spec} is listed twice");
                seen.push(chord);
            }
        }
        Ok(())
    }
}
