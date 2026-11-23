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
        path.extension().and_then(|ext| ext.to_str()),
        Some("css" | "scss" | "sass" | "less" | "html" | "htm" | "vue" | "svelte")
    )
}

/// The style variable under `col` (chars) in `line`: `--name`, `$name` or `@name`.
pub fn variable_at(path: &Path, line: &str, col: usize) -> Option<String> {
    if !is_style_file(path) {
        return None;
    }
    let chars: Vec<char> = line.chars().collect();
    let caret = col.min(chars.len());
    let start = caret
        - chars
            .iter()
            .take(caret)
            .rev()
            .take_while(|chr| is_name_part(**chr))
            .count();
    let end = caret
        + chars
            .iter()
            .skip(caret)
            .take_while(|chr| is_name_part(**chr))
            .count();
    let word: String = chars.get(start..end).unwrap_or_default().iter().collect();
    if let Some(name) = word
        .strip_prefix("--")
        .filter(|name| !name.is_empty() && !name.starts_with('-'))
    {
        return Some(format!("--{name}"));
    }
    let sigil = start.checked_sub(1).and_then(|idx| chars.get(idx)).copied();
    match sigil {
        Some(prefix @ ('$' | '@')) if !word.is_empty() && !word.starts_with('-') => {
            Some(format!("{prefix}{word}"))
        }
        _ => None,
    }
}

/// Whether `chr` can be part of a variable name.
fn is_name_part(chr: char) -> bool {
    chr.is_alphanumeric() || chr == '-' || chr == '_'
}

/// A count or index as `u32`, saturating for absurdly long lines.
fn to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
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
            .filter_entry(|entry| {
                !(entry.file_type().is_some_and(|kind| kind.is_dir())
                    && crate::SKIP_DIRS.iter().any(|dir| entry.file_name() == *dir))
            })
            .build()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_some_and(|kind| kind.is_file()) && is_style_file(entry.path())
            });
        for entry in files {
            let path = entry.path().to_path_buf();
            let text = match open.get(&path) {
                Some(text) => text.clone(),
                None => match std::fs::read_to_string(&path) {
                    Ok(text) => text,
                    Err(_) => continue,
                },
            };
            for (start, len, is_definition) in occurrences(&text, &name) {
                let wanted = match nav {
                    Nav::Definition | Nav::Implementation => is_definition,
                    Nav::Usages => true,
                };
                if wanted {
                    let end = LineCol::new(start.line, start.col + len);
                    found.push(Location {
                        path: path.clone(),
                        range: Range { start, end },
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
        let mut idx = 0;
        while idx + len <= chars.len() {
            if chars.get(idx..idx + len) == Some(needle.as_slice()) {
                // `.btn--brand` (a BEM class) is not `--brand`.
                let before_ok = idx
                    .checked_sub(1)
                    .and_then(|prev| chars.get(prev))
                    .is_none_or(|chr| !is_name_part(*chr));
                let after_ok = chars.get(idx + len).is_none_or(|chr| !is_name_part(*chr));
                if before_ok && after_ok {
                    let is_definition = chars
                        .iter()
                        .skip(idx + len)
                        .find(|chr| !chr.is_whitespace())
                        == Some(&':');
                    out.push((
                        LineCol::new(to_u32(line_no), to_u32(idx)),
                        to_u32(len),
                        is_definition,
                    ));
                    idx += len;
                    continue;
                }
            }
            idx += 1;
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
        let definitions: Vec<bool> = found
            .iter()
            .map(|(_, _, is_definition)| *is_definition)
            .collect();
        assert_eq!(definitions, [true, false]);
    }
}
