//! Background searches feeding the launcher: text in files and workspace symbols.
//!
//! Each search carries a generation number; a newer query bumps the shared
//! counter, which stops older text walks early and lets the app drop stale
//! results.

use std::ops::ControlFlow;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use karet_core::LineCol;
use karet_lsp::LspClient;
use karet_search::Matcher;
use karet_search::SearchQuery;
use tokio::sync::mpsc::UnboundedSender;

use crate::Event;
use crate::picker::Item;
use crate::picker::Kind;
use crate::picker::Target;

/// Most text hits collected per query.
const MAX_TEXT_HITS: usize = 300;

pub fn text(
    root: PathBuf,
    query: String,
    generation: u64,
    current: Arc<AtomicU64>,
    tx: UnboundedSender<Event>,
) {
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let search_query = SearchQuery {
            pattern: query.clone(),
            case_sensitive: query.chars().any(char::is_uppercase),
            ..Default::default()
        };
        let Ok(matcher) = Matcher::new(&search_query) else {
            let _ = tx.send(Event::Launcher(generation, Kind::Text, Vec::new()));
            return;
        };
        let mut items = Vec::new();
        // karet-search already prunes .git, target and node_modules.
        let excludes: Vec<String> = crate::SKIP_DIRS
            .iter()
            .map(|dir| format!("**/{dir}/**"))
            .collect();
        let _ = karet_search::walk_text_files(&root, &[], &excludes, |path, text| {
            if current.load(Ordering::Relaxed) != generation {
                return ControlFlow::Break(());
            }
            let mut last_line = None;
            // One hit per line: several matches on a line would list it twice.
            for found in matcher
                .find(&text)
                .into_iter()
                .filter(|found| last_line.replace(found.line) != Some(found.line))
                .take(20)
            {
                items.push(hit(&root, path, &text, found.start, found.line, found.col));
                if items.len() >= MAX_TEXT_HITS {
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        });
        if current.load(Ordering::Relaxed) == generation {
            crate::debug_log(&format!(
                "text search {query:?}: {} hits in {:?}",
                items.len(),
                started.elapsed()
            ));
            let _ = tx.send(Event::Launcher(generation, Kind::Text, items));
        }
    });
}

fn hit(root: &Path, path: &Path, text: &str, start: usize, line: u32, col: u32) -> Item {
    let (before, after) = text.split_at_checked(start).unwrap_or((text, ""));
    let line_start = before.rfind('\n').map_or(0, |idx| idx + 1);
    let line_end = after.find('\n').map_or(text.len(), |idx| start + idx);
    let line_text = text.get(line_start..line_end).unwrap_or_default();
    // LSP-style UTF-16 column, which `App::jump` expects.
    let col16 = line_text.get(..col as usize).map_or(0, |prefix| {
        u32::try_from(prefix.encode_utf16().count()).unwrap_or(u32::MAX)
    });
    let rel = path.strip_prefix(root).unwrap_or(path);
    Item {
        label: line_text.trim().chars().take(120).collect(),
        detail: format!("{}:{}", rel.display(), line + 1),
        target: Target::Location(path.to_path_buf(), LineCol::new(line, col16)),
        kind: Kind::Text,
        keywords: "",
    }
}

pub fn symbols(
    client: Arc<LspClient>,
    root: PathBuf,
    query: String,
    generation: u64,
    tx: UnboundedSender<Event>,
) {
    tokio::spawn(async move {
        let Ok(found) = client.workspace_symbols(&query).await else {
            let _ = tx.send(Event::Launcher(generation, Kind::Symbol, Vec::new()));
            return;
        };
        let items = found
            .into_iter()
            .take(200)
            .map(|symbol| {
                let rel = symbol
                    .path
                    .strip_prefix(&root)
                    .unwrap_or(&symbol.path)
                    .display()
                    .to_string();
                let detail = match (&symbol.container_name, symbol.range) {
                    (Some(container), Some(range)) if !container.is_empty() => {
                        format!("{container}  {rel}:{}", range.start.line + 1)
                    }
                    (_, Some(range)) => format!("{rel}:{}", range.start.line + 1),
                    _ => rel,
                };
                let target = match symbol.range {
                    Some(range) => Target::Location(symbol.path.clone(), range.start),
                    None => Target::File(symbol.path.clone()),
                };
                Item {
                    label: symbol.name,
                    detail,
                    target,
                    kind: Kind::Symbol,
                    keywords: "",
                }
            })
            .collect();
        let _ = tx.send(Event::Launcher(generation, Kind::Symbol, items));
    });
}
