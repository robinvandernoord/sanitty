//! The Terminal view in the app (Alt+T): switching to it and back, and
//! handing it keys. The shell itself is in `crate::shell`.

use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;

use super::App;
use super::Screen;
use crate::keymap::Action;
use crate::shell::Shell;

impl App {
    /// Alt+T: the Terminal view, or back to the view it was opened from.
    /// The shell keeps running in between.
    pub(super) fn toggle_terminal(&mut self) {
        if self.screen == Screen::Terminal {
            match self.terminal_return {
                Screen::Database => self.toggle_database(),
                screen => self.screen = screen,
            }
            return;
        }
        let from = self.screen;
        self.close_overlay();
        self.completion = None;
        // The Database view puts its query file on screen; leaving it
        // puts the code back first.
        if from == Screen::Database {
            self.leave_database();
        }
        self.terminal_return = from;
        self.screen = Screen::Terminal;
        if self.shell.is_none() {
            self.start_shell();
        }
    }

    fn start_shell(&mut self) {
        // The real size comes with the first draw.
        match Shell::spawn(&self.root, 24, 80, self.tx.clone()) {
            Ok(shell) => self.shell = Some(shell),
            Err(err) => self.status = format!("Terminal: {err}"),
        }
    }

    /// Keys while the Terminal view is on screen: Alt+T goes back, every
    /// other key goes to the shell (Enter starts a new one once it exited).
    /// True when the view used the key.
    pub(super) fn terminal_key(&mut self, key: &KeyEvent, action: Option<Action>) -> bool {
        if self.screen != Screen::Terminal {
            return false;
        }
        if action == Some(Action::Shell) {
            self.toggle_terminal();
            return true;
        }
        match &mut self.shell {
            Some(shell) if shell.exited() => {
                if key.code == KeyCode::Enter {
                    self.shell = None;
                    self.start_shell();
                }
            }
            Some(shell) => shell.key(key),
            None if key.code == KeyCode::Enter => self.start_shell(),
            None => {}
        }
        true
    }

    /// The hint bar in the Terminal view.
    pub(super) fn terminal_hints(&self) -> Vec<(String, &'static str)> {
        vec![
            (self.display_key(Action::Shell), "Back"),
            ("Shift+PgUp/PgDn".to_string(), "Scroll back"),
            ("Any other key".to_string(), "Goes to the shell"),
        ]
    }

    /// Pasted text goes to the shell. True when the view took it.
    pub(super) fn terminal_paste(&mut self, text: &str) -> bool {
        if self.screen != Screen::Terminal {
            return false;
        }
        if let Some(shell) = &mut self.shell
            && !shell.exited()
        {
            shell.paste(text);
        }
        true
    }
}
