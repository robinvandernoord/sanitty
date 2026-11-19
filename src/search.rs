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
        let q = SearchQuery {
            pattern: query.clone(),
            case_sensitive: query.chars().any(char::is_uppercase),
            ..Default::default()
        };
        let Ok(matcher) = Matcher::new(&q) else {
            return;
        };
        let mut items = Vec::new();
        // karet-search already prunes .git, target and node_modules.
        let excludes: Vec<String> = crate::SKIP_DIRS
            .iter()
            .map(|d| format!("**/{d}/**"))
            .collect();
        let _ = karet_search::walk_text_files(&root, &[], &excludes, |path, text| {
            if current.load(Ordering::Relaxed) != generation {
                return ControlFlow::Break(());
            }
            let mut last_line = None;
            // One hit per line: several matches on a line would list it twice.
            for m in matcher
                .find(&text)
                .into_iter()
                .filter(|m| last_line.replace(m.line) != Some(m.line))
                .take(20)
            {
                items.push(hit(&root, path, &text, m.start, m.line, m.col));
                if items.len() >= MAX_TEXT_HITS {
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        });
        if current.load(Ordering::Relaxed) == generation {
            let _ = tx.send(Event::Launcher(generation, Kind::Text, items));
        }
    });
}

fn hit(root: &Path, path: &Path, text: &str, start: usize, line: u32, col: u32) -> Item {
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let line_end = text[start..].find('\n').map_or(text.len(), |i| start + i);
    let line_text = &text[line_start..line_end];
    // LSP-style UTF-16 column, which `App::jump` expects.
    let col16 = line_text
        .get(..col as usize)
        .map_or(0, |p| p.encode_utf16().count() as u32);
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
            return;
        };
        let items = found
            .into_iter()
            .take(200)
            .map(|s| {
                let rel = s
                    .path
                    .strip_prefix(&root)
                    .unwrap_or(&s.path)
                    .display()
                    .to_string();
                let detail = match (&s.container_name, s.range) {
                    (Some(c), Some(r)) if !c.is_empty() => {
                        format!("{c}  {rel}:{}", r.start.line + 1)
                    }
                    (_, Some(r)) => format!("{rel}:{}", r.start.line + 1),
                    _ => rel,
                };
                let target = match s.range {
                    Some(r) => Target::Location(s.path.clone(), r.start),
                    None => Target::File(s.path.clone()),
                };
                Item {
                    label: s.name,
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
