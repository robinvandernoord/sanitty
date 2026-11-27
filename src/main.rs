//! sanitty: a PyCharm-flavoured terminal IDE built on the karet-* crates.

mod allkeys;
mod app;
mod clipboard;
mod colors;
mod complete;
mod csv;
mod database;
mod doc;
mod explorer;
mod find;
mod keycheck;
mod keymap;
mod lsp;
mod markdown;
mod picker;
mod review;
mod search;
mod settings;
mod shell;
mod sig;
mod statefile;
mod stylenav;
mod styles;
mod tshl;
mod util;

use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event;
use crossterm::event::DisableBracketedPaste;
use crossterm::event::EnableBracketedPaste;
use crossterm::event::KeyboardEnhancementFlags;
use crossterm::event::PopKeyboardEnhancementFlags;
use crossterm::event::PushKeyboardEnhancementFlags;
use crossterm::execute;
use crossterm::terminal;
use karet_core::Diagnostic;
use karet_core::Location;
use karet_lsp::LspClient;
use tokio::sync::mpsc;

/// Everything the app loop reacts to.
pub enum Event {
    Term(event::Event),
    Files(Vec<PathBuf>),
    Status(String),
    /// A language server started: its key, the program it is, the client.
    LspReady(&'static str, String, Arc<LspClient>),
    LspFailed(&'static str, crate::lsp::Failure),
    /// Problems for a file from one server (its key), replacing that server's last report.
    /// The document version it describes, when known, so an older report
    /// cannot replace a newer one.
    /// `related` marks problems a server mentioned for another file in a
    /// pull answer.
    Diagnostics(&'static str, PathBuf, Option<i32>, bool, Vec<Diagnostic>),
    Locations(lsp::Nav, Vec<Location>),
    /// A rename's edits, tagged with the request's id.
    Rename(u64, lsp::Edit),
    /// Formatting edits (UTF-16 columns) for a file, computed against the
    /// buffer version given.
    /// The document's open instance and buffer version come along, so a
    /// reply cannot land on a closed and reopened copy.
    Formatted(PathBuf, u64, u64, Vec<karet_core::TextEdit>),
    /// A hover answer: file, caret position (chars), text, and whether it was
    /// asked for as a popup (Alt+H) rather than for the status line.
    Hover(PathBuf, karet_core::LineCol, Option<String>, bool),
    /// Completion candidates for the request with this generation, or why
    /// there are none.
    Completions(u64, Result<karet_core::CompletionList, String>),
    /// A completion item filled in (docs, imports): the list it belongs to,
    /// its index in that list, and the server's answer (`None` if it had
    /// nothing), or why it failed.
    CompletionResolved(
        u64,
        usize,
        Result<Option<Box<karet_core::CompletionItem>>, String>,
    ),
    /// Semantic tokens for a file at an LSP document version (UTF-16 columns).
    Semantic(PathBuf, i32, Vec<karet_core::SemanticToken>),
    /// The Terminal view's shell printed something or exited: redraw.
    ShellOutput,
    /// Async launcher results: search generation, kind, items.
    Launcher(u64, picker::Kind, Vec<picker::Item>),
    /// The sources the Review view can show, for its picker.
    ReviewSources(u64, Vec<review::git::SourceInfo>),
    /// Commits or pull requests for the Review view to pick from.
    ReviewChoices(u64, Result<review::git::Choices, String>),
    /// A diff for the Review view, or why it could not be loaded.
    ReviewLoaded(u64, Result<review::git::Loaded, String>),
    /// Database connections found for the workspace, and problems finding them.
    DbFound(Vec<database::conn::Connection>, Vec<String>),
    /// A connection opened (with its schema) or failed, for the connect
    /// with this generation.
    DbConnected(
        u64,
        Result<(database::driver::Pool, database::driver::Schema), String>,
    ),
    /// The schema read again.
    DbSchema(u64, Result<database::driver::Schema, String>),
    /// SQL ran, for the run with this generation.
    DbResult(u64, Result<database::driver::ResultSet, String>),
    Tick,
}

/// Append a line to the file named by `SANITTY_LOG`, if set. For tracing
/// language-server trouble without disturbing the screen.
pub fn debug_log(line: &str) {
    if let Some(path) = std::env::var_os("SANITTY_LOG")
        && let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

/// `$XDG_CONFIG_HOME/sanitty`.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("sanitty"))
}

/// The workspace root for a path: the enclosing git repository, else the
/// directory itself.
fn workspace_root(path: &Path) -> PathBuf {
    let dir = if path.is_dir() {
        path
    } else {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
    };
    let dir = dir.to_path_buf();
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    dir.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map_or(dir.clone(), Path::to_path_buf)
}

/// Directories never worth listing or searching, even without a `.gitignore`:
/// VCS data, build output, dependency trees and tool caches.
pub const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".idea",
];

fn index_files(root: PathBuf, tx: mpsc::UnboundedSender<Event>) {
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let files: Vec<PathBuf> = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .filter_entry(|entry| {
                !(entry.file_type().is_some_and(|kind| kind.is_dir())
                    && SKIP_DIRS.iter().any(|skip| entry.file_name() == *skip))
            })
            .build()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
            .map(ignore::DirEntry::into_path)
            .take(200_000)
            .collect();
        debug_log(&format!(
            "index: {} files in {:?}",
            files.len(),
            started.elapsed()
        ));
        let _ = tx.send(Event::Files(files));
    });
}

/// Reads terminal input on its own thread.
fn spawn_input(tx: mpsc::UnboundedSender<Event>) {
    std::thread::spawn(move || {
        loop {
            match event::poll(Duration::from_millis(30)) {
                Ok(true) => {
                    if let Ok(term_event) = event::read() {
                        // Exactly what the terminal sent, for finding out why
                        // a key does nothing (SANITTY_LOG only).
                        if let event::Event::Key(key) = &term_event {
                            debug_log(&format!("key: {key:?}"));
                        }
                        if tx.send(Event::Term(term_event)).is_err() {
                            return;
                        }
                    }
                }
                Ok(false) => {}
                Err(_) => return,
            }
        }
    });
}

fn enter_terminal(enhanced: bool) -> std::io::Result<()> {
    terminal::enable_raw_mode()?;
    execute!(
        std::io::stdout(),
        terminal::EnterAlternateScreen,
        EnableBracketedPaste
    )?;
    if enhanced {
        execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    Ok(())
}

fn leave_terminal(enhanced: bool) {
    if enhanced {
        let _ = execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        std::io::stdout(),
        DisableBracketedPaste,
        terminal::LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = terminal::disable_raw_mode();
}

/// What to open, from the arguments: a path (the current directory when
/// none), or `z <words>` for the directory zoxide knows by those words, as
/// `z` itself would go to. `z` only means zoxide when nothing called `z`
/// is here.
fn target(args: &[String]) -> Result<PathBuf, String> {
    match args {
        [] => Ok(PathBuf::from(".")),
        [first, words @ ..] if first == "z" && !Path::new("z").exists() => zoxide(words),
        [first, ..] => Ok(PathBuf::from(first)),
    }
}

/// The directory zoxide picks for `words` (`zoxide query`).
fn zoxide(words: &[String]) -> Result<PathBuf, String> {
    if words.is_empty() {
        return Err("z needs words to look for, e.g. `sanitty z project`".into());
    }
    let output = std::process::Command::new("zoxide")
        .arg("query")
        .arg("--")
        .args(words)
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => "zoxide is not installed (or not on PATH)".to_string(),
            _ => format!("could not run zoxide: {err}"),
        })?;
    let found = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() || found.is_empty() {
        return Err(format!(
            "zoxide knows no directory for \"{}\"",
            words.join(" ")
        ));
    }
    Ok(PathBuf::from(found))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `--check-keys` starts in Check Keys, for terminals that swallow the launcher key.
    let check_keys = std::env::args().any(|arg| arg == "--check-keys");
    let positional: Vec<String> = std::env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with("--"))
        .collect();
    let arg = match target(&positional) {
        Ok(path) => path,
        Err(message) => {
            eprintln!("sanitty: {message}");
            std::process::exit(1);
        }
    };
    let root = workspace_root(&arg);

    let (tx, mut rx) = mpsc::unbounded_channel();

    enter_terminal(false)?;
    let enhanced = terminal::supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        execute!(
            std::io::stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        leave_terminal(enhanced);
        hook(info);
    }));
    let mut term =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(std::io::stdout()))?;
    term.clear()?;

    let mut app = app::App::new(root.clone(), tx.clone());
    app.enhanced_keys = enhanced;
    if check_keys {
        app.check_keys();
    } else if arg.is_file() {
        app.open(&arg, None);
    } else {
        app.show_tree();
    }
    index_files(root.clone(), tx.clone());
    spawn_input(tx.clone());
    let tick_tx = tx.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(300));
        loop {
            ticker.tick().await;
            if tick_tx.send(Event::Tick).is_err() {
                break;
            }
        }
    });

    let result: anyhow::Result<()> = async {
        loop {
            term.draw(|frame| app.render(frame))?;
            let Some(incoming) = rx.recv().await else {
                break;
            };
            app.handle(incoming);
            // Drain whatever else is queued before redrawing (fast typing, pastes).
            while let Ok(queued) = rx.try_recv() {
                app.handle(queued);
            }
            if app.request.take() == Some(app::Request::Quit) {
                break;
            }
        }
        Ok(())
    }
    .await;

    leave_terminal(enhanced);
    result
}
