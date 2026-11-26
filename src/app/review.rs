//! The Review view in the app: switching to it and back, handing it keys,
//! and loading its diffs off the app loop.

use crossterm::event::KeyEvent;

use super::App;
use super::Overlay;
use super::Screen;
use super::database::InView;
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

    /// What an action does in the Review view, which is read-only: the
    /// view's and the app's own keys work, going to a file or a search
    /// leaves it, and editing or code features say they do not apply. Every
    /// action is listed, so a new one needs a decision (as in the Database
    /// view, [`App::in_db_view`]).
    pub(super) fn in_review_view(action: Action) -> InView {
        match action {
            Action::Review
            | Action::Database
            | Action::Quit
            | Action::AllKeys
            | Action::CheckKeys
            | Action::Shell
            | Action::Copy
            | Action::None => InView::Keep,
            Action::Launcher
            | Action::GotoFile
            | Action::GotoSymbol
            | Action::GotoLine
            | Action::SearchInFiles
            | Action::RecentFiles
            | Action::PreviousFile
            | Action::ToggleExplorer
            | Action::Problems
            | Action::NavBack
            | Action::NavForward => InView::Leave,
            Action::Save
            | Action::Find
            | Action::Replace
            | Action::FindNext
            | Action::FindPrev
            | Action::GotoDefinition
            | Action::GotoImplementation
            | Action::FindUsages
            | Action::QuickDoc
            | Action::Rename
            | Action::NextProblem
            | Action::PrevProblem
            | Action::ToggleComment
            | Action::Reformat
            | Action::Complete
            | Action::RunSql
            | Action::DuplicateLine
            | Action::DeleteLine
            | Action::AddCaretAbove
            | Action::AddCaretBelow
            | Action::AddNextOccurrence
            | Action::RemoveLastCaret
            | Action::SelectAllOccurrences
            | Action::CaretsAtLineEnds
            | Action::Undo
            | Action::Redo
            | Action::CopyLine
            | Action::Cut
            | Action::Paste
            | Action::SelectAll
            | Action::CloseFile => InView::Unavailable,
        }
    }

    /// Keys while the Review view is on screen. True when it used the key;
    /// false lets it run as an action (one that leaves the view does so).
    /// While the comment box or a list is open, it gets the keys.
    pub(super) fn review_key(&mut self, key: &KeyEvent, action: Option<Action>) -> bool {
        if !self.in_review() || !matches!(self.overlay, Overlay::None) {
            return false;
        }
        let Some(review) = &mut self.review else {
            return false;
        };
        let popup = review.has_popup();
        if let Some(action) = action {
            match Self::in_review_view(action) {
                // Ctrl+C is the view's: out of a box, else out of the view.
                InView::Keep if action == Action::Copy => {}
                InView::Keep => return false,
                InView::Leave if !popup => {
                    self.screen = Screen::Code;
                    return false;
                }
                InView::Unavailable if !popup => {
                    self.status = format!("{} does not work in the Review view", action.label());
                    return true;
                }
                // The box or list gets it (it ignores Ctrl and Alt chords).
                InView::Leave | InView::Unavailable => {}
            }
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
