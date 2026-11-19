//! Go to definition and find usages for style variables, without a language
//! server: CSS custom properties (`--brand`, used as `var(--brand)`), SCSS
//! (`$brand`) and Less (`@brand`) variables.
//!
//! CSS language servers only look inside the current file, but these are
//! usually defined in a shared stylesheet, so this searches the whole project.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use karet_core::LineCol;
use karet_core::Location;
use karet_core::Range;
use tokio::sync::mpsc::UnboundedSender;

use crate::Event;
use crate::lsp::Nav;

/// Files that can define or use style variables.
fn is_style_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("css" | "scss" | "sass" | "less" | "html" | "htm" | "vue" | "svelte")
    )
}

/// The style variable under `col` (chars) in `line`: `--name`, `$name` or `@name`.
pub fn variable_at(path: &Path, line: &str, col: usize) -> Option<String> {
    if !is_style_file(path) {
        return None;
    }
    let chars: Vec<char> = line.chars().collect();
    let part = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
    let mut start = col.min(chars.len());
    while start > 0 && part(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col.min(chars.len());
    while end < chars.len() && part(chars[end]) {
        end += 1;
    }
    let word: String = chars[start..end].iter().collect();
    if let Some(name) = word
        .strip_prefix("--")
        .filter(|n| !n.is_empty() && !n.starts_with('-'))
    {
        return Some(format!("--{name}"));
    }
    let sigil = start.checked_sub(1).map(|i| chars[i]);
    match sigil {
        Some(s @ ('$' | '@')) if !word.is_empty() && !word.starts_with('-') => {
            Some(format!("{s}{word}"))
        }
        _ => None,
    }
}

/// Search the project (open buffers win over the files on disk) and answer
/// with `Event::Locations`, as a language server would.
pub fn search(
    root: PathBuf,
    name: String,
    nav: Nav,
    open: HashMap<PathBuf, String>,
    tx: UnboundedSender<Event>,
) {
    std::thread::spawn(move || {
        let mut found = Vec::new();
        let files = ignore::WalkBuilder::new(&root)
            .hidden(false)
            .filter_entry(|e| {
                !(e.file_type().is_some_and(|t| t.is_dir())
                    && crate::SKIP_DIRS.iter().any(|d| e.file_name() == *d))
            })
            .build()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()) && is_style_file(e.path()));
        for entry in files {
            let path = entry.path().to_path_buf();
            let text = match open.get(&path) {
                Some(t) => t.clone(),
                None => match std::fs::read_to_string(&path) {
                    Ok(t) => t,
                    Err(_) => continue,
                },
            };
            for (at, len, is_definition) in occurrences(&text, &name) {
                let wanted = match nav {
                    Nav::Definition | Nav::Implementation => is_definition,
                    Nav::Usages => true,
                };
                if wanted {
                    let end = LineCol::new(at.line, at.col + len);
                    found.push(Location {
                        path: path.clone(),
                        range: Range { start: at, end },
                    });
                }
            }
        }
        // Columns are chars; style files are ASCII in practice, where chars
        // and the UTF-16 columns `Event::Locations` carries are the same.
        let _ = tx.send(Event::Locations(nav, found));
    });
}

/// Every whole-name occurrence of `name`: (start, length, is it a definition).
/// A definition is the name followed by `:` (`--brand: red`, `$brand: red`).
fn occurrences(text: &str, name: &str) -> Vec<(LineCol, u32, bool)> {
    let mut out = Vec::new();
    let len = name.chars().count();
    for (line_no, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let needle: Vec<char> = name.chars().collect();
        let part = |c: char| c.is_alphanumeric() || c == '-' || c == '_';
        let mut i = 0;
        while i + len <= chars.len() {
            if chars[i..i + len] == needle[..] {
                // `.btn--brand` (a BEM class) is not `--brand`.
                let before_ok = i == 0 || !part(chars[i - 1]);
                let after_ok = chars.get(i + len).is_none_or(|c| !part(*c));
                if before_ok && after_ok {
                    let rest: String = chars[i + len..].iter().collect();
                    let is_definition = rest.trim_start().starts_with(':');
                    out.push((
                        LineCol::new(line_no as u32, i as u32),
                        len as u32,
                        is_definition,
                    ));
                    i += len;
                    continue;
                }
            }
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_variable_under_the_caret() {
        let css = Path::new("a.css");
        assert_eq!(
            variable_at(css, "  color: var(--brand-dark);", 16).as_deref(),
            Some("--brand-dark")
        );
        assert_eq!(variable_at(css, "  color: red;", 4), None);
        let scss = Path::new("a.scss");
        assert_eq!(
            variable_at(scss, "a { color: $primary; }", 13).as_deref(),
            Some("$primary")
        );
    }

    #[test]
    fn tells_definitions_from_usages() {
        let text = ":root { --brand: red; --brand-dark: maroon; }\na { color: var(--brand); }\n.btn--brand {}";
        let found = occurrences(text, "--brand");
        assert_eq!(found.len(), 2);
        assert!(found[0].2);
        assert!(!found[1].2);
    }
}
