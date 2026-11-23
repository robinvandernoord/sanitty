//! TypeScript highlighting with tree-sitter directly.
//!
//! tree-sitter-typescript's highlights query only holds what TypeScript adds to
//! JavaScript; editors run it together with tree-sitter-javascript's query.
//! karet uses the TypeScript part alone, so `const`, `if`, `return`, strings
//! and comments in `.ts` files stay uncoloured. This combines both queries over
//! the TypeScript grammar and flattens the result the way karet does: the
//! innermost capture wins, then the more specific name, then the earlier pattern.

use std::path::Path;
use std::sync::OnceLock;

use karet_core::BytePos;
use karet_core::HighlightSpan;
use karet_core::Highlights;
use karet_core::Span;
use karet_core::StandardToken;
use karet_core::TokenId;
use streaming_iterator::StreamingIterator;
use tree_sitter::Language;
use tree_sitter::Parser;
use tree_sitter::Query;
use tree_sitter::QueryCursor;

/// Whether this file is TypeScript (`.ts`, `.mts`, `.cts`, `.tsx`).
pub fn handles(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("ts" | "mts" | "cts" | "tsx")
    )
}

fn language(tsx: bool) -> Language {
    if tsx {
        tree_sitter_typescript::LANGUAGE_TSX.into()
    } else {
        tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
    }
}

/// The combined query, compiled once per grammar. TypeScript's own patterns
/// come first so they rank as earlier patterns.
fn query(tsx: bool) -> Option<&'static Query> {
    static TYPESCRIPT: OnceLock<Option<Query>> = OnceLock::new();
    static TSX: OnceLock<Option<Query>> = OnceLock::new();
    let cell = if tsx { &TSX } else { &TYPESCRIPT };
    cell.get_or_init(|| {
        let source = format!(
            "{}\n{}",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            tree_sitter_javascript::HIGHLIGHT_QUERY
        );
        match Query::new(&language(tsx), &source) {
            Ok(compiled) => Some(compiled),
            Err(err) => {
                crate::debug_log(&format!("typescript highlight query: {err}"));
                None
            }
        }
    })
    .as_ref()
}

/// A capture name to a theme token: the name itself, else its prefixes
/// (`function.method.call` -> `function.method` -> `function`).
fn token_for(name: &str) -> Option<TokenId> {
    let mut rest = name;
    loop {
        if let Some(token) = StandardToken::from_capture_name(rest) {
            return Some(token.id());
        }
        rest = rest.rsplit_once('.')?.0;
    }
}

pub fn highlight(text: &str, path: &Path) -> Option<Highlights> {
    let tsx = path.extension().and_then(|ext| ext.to_str()) == Some("tsx");
    let query = query(tsx)?;
    let mut parser = Parser::new();
    parser.set_language(&language(tsx)).ok()?;
    let tree = parser.parse(text, None)?;
    let names = query.capture_names();
    // (start, end, specificity, pattern, token)
    let mut caps: Vec<(usize, usize, usize, usize, TokenId)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    while let Some(matched) = matches.next() {
        for capture in matched.captures {
            let Some(name) = names.get(capture.index as usize) else {
                continue;
            };
            if let Some(token) = token_for(name) {
                let range = capture.node.byte_range();
                caps.push((
                    range.start,
                    range.end,
                    name.split('.').count(),
                    matched.pattern_index,
                    token,
                ));
            }
        }
    }
    // Paint outer and weaker captures first, so inner, more specific and
    // earlier-pattern captures end up on top.
    caps.sort_by(|lhs, rhs| {
        (rhs.1 - rhs.0)
            .cmp(&(lhs.1 - lhs.0))
            .then(lhs.2.cmp(&rhs.2))
            .then(rhs.3.cmp(&lhs.3))
    });
    let mut paint: Vec<Option<TokenId>> = vec![None; text.len()];
    for (start, end, _, _, token) in caps {
        for slot in paint
            .get_mut(start..end.min(text.len()))
            .into_iter()
            .flatten()
        {
            *slot = Some(token);
        }
    }
    let mut spans = Vec::new();
    let mut offset = 0;
    for run in paint.chunk_by(PartialEq::eq) {
        let start = offset;
        offset += run.len();
        if let Some(&Some(token)) = run.first() {
            spans.push(HighlightSpan {
                span: Span::new(BytePos(start), BytePos(offset)),
                token,
            });
        }
    }
    Some(Highlights::from_sorted_spans(spans))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_strings_and_comments_get_colour() -> Result<(), Box<dyn std::error::Error>> {
        let text = "// note\nfunction f(): void {\n  const x = \"s\";\n  if (x) { return; }\n}\n";
        let highlights = highlight(text, Path::new("a.ts")).ok_or("no highlights")?;
        let token_at = |needle: &str| -> Option<TokenId> {
            let pos = text.find(needle)?;
            highlights
                .all()
                .iter()
                .find(|entry| entry.span.start.0 <= pos && pos < entry.span.end.0)
                .map(|entry| entry.token)
        };
        assert_eq!(token_at("const"), Some(StandardToken::Keyword.id()));
        assert_eq!(token_at("return"), Some(StandardToken::Keyword.id()));
        assert_eq!(token_at("\"s\""), Some(StandardToken::String.id()));
        assert_eq!(token_at("// note"), Some(StandardToken::Comment.id()));
        Ok(())
    }
}
