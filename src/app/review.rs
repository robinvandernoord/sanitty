//! The Review view in the app: switching to it and back, handing it keys,
//! and loading its diffs off the app loop.

use crossterm::event::KeyEvent;

use super::App;
use super::Overlay;
use crate::Event;
use crate::clipboard::Via;
use crate::keymap::Action;
use crate::review::Outcome;
use crate::review::Review;
use crate::review::git;

impl App {
    /// Alt+9: open the Review view, or go back to the code.
    pub(super) fn toggle_review(&mut self) {
        if self.in_review {
            self.in_review = false;
            return;
        }
        self.close_overlay();
        self.completion = None;
        self.in_review = true;
        let root = self.root.clone();
        let review = self.review.get_or_insert_with(|| Review::new(root));
        let outcome = review.open();
        self.review_outcome(outcome);
    }

    /// Keys while the Review view is on screen. True when it used the key.
    /// Global keys that still work there: the view's own key, Quit and All
    /// Keys, and anything while a popup (All Keys) is open.
    pub(super) fn review_key(&mut self, key: &KeyEvent, action: Option<Action>) -> bool {
        if !self.in_review || !matches!(self.overlay, Overlay::None) {
            return false;
        }
        if matches!(
            action,
            Some(Action::Review | Action::Quit | Action::AllKeys)
        ) {
            return false;
        }
        let Some(review) = &mut self.review else {
            return false;
        };
        let outcome = review.handle_key(key);
        self.review_outcome(outcome);
        true
    }

    pub(super) fn review_outcome(&mut self, outcome: Outcome) {
        let Some(review) = &self.review else { return };
        let root = review.root().to_path_buf();
        match outcome {
            Outcome::Pending => {}
            Outcome::Status(text) => self.status = text,
            Outcome::Copy(text, status) => {
                // Also kept as a file: the way to get it when the terminal
                // does not pass the clipboard on (some do not, over SSH).
                let file = crate::review::state::export_file(&root)
                    .filter(|path| std::fs::write(path, &text).is_ok());
                let via = self.set_clipboard(text);
                self.status = match (via, file) {
                    (Via::Program(_), _) => status,
                    (Via::Terminal, Some(path)) => format!(
                        "{status} Sent through the terminal; if it did not arrive, it is in {}",
                        path.display()
                    ),
                    (Via::Terminal, None) => format!("{status} Sent through the terminal"),
                };
            }
            Outcome::Leave => self.in_review = false,
            Outcome::Load(source) => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(Event::ReviewLoaded(git::load(&root, source)));
                });
            }
            Outcome::Fetch(entry) => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let list = match entry {
                        git::Entry::Prs => git::prs(&root),
                        _ => git::commits(&root),
                    };
                    let _ = tx.send(Event::ReviewChoices(list));
                });
            }
            Outcome::Sources => {
                let tx = self.tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(Event::ReviewSources(git::sources(&root)));
                });
            }
        }
    }
}
