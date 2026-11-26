//! The Review view in the app: switching to it and back, handing it keys,
//! and loading its diffs off the app loop.

use crossterm::event::KeyEvent;

use super::App;
use super::Overlay;
use super::Screen;
use crate::Event;
use crate::clipboard::Via;
use crate::keymap::Action;
use crate::review::Outcome;
use crate::review::Review;
use crate::review::git;

impl App {
    /// Alt+9: open the Review view, or go back to the code.
    pub(super) fn toggle_review(&mut self) {
        if self.in_review() {
            self.screen = Screen::Code;
            return;
        }
        self.close_overlay();
        self.completion = None;
        self.screen = Screen::Review;
        let root = self.root.clone();
        let review = self.review.get_or_insert_with(|| Review::new(root));
        let message = review.take_message();
        let outcome = review.open();
        self.review_outcome(outcome);
        if let Some(message) = message {
            self.status = message;
        }
    }

    /// Keys while the Review view is on screen. True when it used the key.
    /// Global keys that still work there: the view's own key, the Database
    /// view's, Quit and All Keys, and anything while a popup (All Keys) is
    /// open.
    pub(super) fn review_key(&mut self, key: &KeyEvent, action: Option<Action>) -> bool {
        if !self.in_review() || !matches!(self.overlay, Overlay::None) {
            return false;
        }
        if matches!(
            action,
            Some(Action::Review | Action::Database | Action::Quit | Action::AllKeys)
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

    /// Answers from git for the view. One for an older request (another
    /// source picked since, or its list closed) is dropped.
    pub(super) fn review_event(&mut self, event: Event) {
        let Some(review) = &mut self.review else {
            return;
        };
        match event {
            Event::ReviewSources(generation, sources) if review.is_current(generation) => {
                review.pick(sources);
            }
            Event::ReviewChoices(generation, result) if review.is_current(generation) => {
                let outcome = review.choices(result);
                self.review_outcome(outcome);
            }
            Event::ReviewLoaded(generation, result) if review.is_current(generation) => {
                if let Some(message) = review.loaded(result) {
                    self.status = message;
                }
            }
            _ => {}
        }
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
            Outcome::Leave => self.screen = Screen::Code,
            Outcome::Load(source) => {
                let tx = self.tx.clone();
                let generation = self.review.as_mut().map_or(0, Review::request);
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(Event::ReviewLoaded(generation, git::load(&root, source)));
                });
            }
            Outcome::Fetch(entry) => {
                let tx = self.tx.clone();
                let generation = self.review.as_mut().map_or(0, Review::request);
                tokio::task::spawn_blocking(move || {
                    let list = match entry {
                        git::Entry::Prs => git::prs(&root),
                        _ => git::commits(&root),
                    };
                    let _ = tx.send(Event::ReviewChoices(generation, list));
                });
            }
            Outcome::Sources => {
                let tx = self.tx.clone();
                let generation = self.review.as_mut().map_or(0, Review::request);
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(Event::ReviewSources(generation, git::sources(&root)));
                });
            }
        }
    }
}
