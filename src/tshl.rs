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
        path.extension().and_then(|e| e.to_str()),
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
    static TS: OnceLock<Option<Query>> = OnceLock::new();
    static TSX: OnceLock<Option<Query>> = OnceLock::new();
    let cell = if tsx { &TSX } else { &TS };
    cell.get_or_init(|| {
        let source = format!(
            "{}\n{}",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            tree_sitter_javascript::HIGHLIGHT_QUERY
        );
        match Query::new(&language(tsx), &source) {
            Ok(q) => Some(q),
            Err(e) => {
                crate::debug_log(&format!("typescript highlight query: {e}"));
                None
            }
        }
    })
    .as_ref()
}

/// A capture name to a theme token: the name itself, else its prefixes
/// (`function.method.call` -> `function.method` -> `function`).
fn token_for(name: &str) -> Option<TokenId> {
    let mut n = name;
    loop {
        if let Some(t) = StandardToken::from_capture_name(n) {
            return Some(t.id());
        }
        n = &n[..n.rfind('.')?];
    }
}

pub fn highlight(text: &str, path: &Path) -> Option<Highlights> {
    let tsx = path.extension().and_then(|e| e.to_str()) == Some("tsx");
    let query = query(tsx)?;
    let mut parser = Parser::new();
    parser.set_language(&language(tsx)).ok()?;
    let tree = parser.parse(text, None)?;
    let names = query.capture_names();
    // (start, end, specificity, pattern, token)
    let mut caps: Vec<(usize, usize, usize, usize, TokenId)> = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), text.as_bytes());
    while let Some(m) = matches.next() {
        for c in m.captures {
            let name = names[c.index as usize];
            if let Some(token) = token_for(name) {
                let r = c.node.byte_range();
                caps.push((
                    r.start,
                    r.end,
                    name.split('.').count(),
                    m.pattern_index,
                    token,
                ));
            }
        }
    }
    // Paint outer and weaker captures first, so inner, more specific and
    // earlier-pattern captures end up on top.
    caps.sort_by(|a, b| {
        (b.1 - b.0)
            .cmp(&(a.1 - a.0))
            .then(a.2.cmp(&b.2))
            .then(b.3.cmp(&a.3))
    });
    let mut paint: Vec<Option<TokenId>> = vec![None; text.len()];
    for (start, end, _, _, token) in caps {
        for slot in &mut paint[start..end.min(text.len())] {
            *slot = Some(token);
        }
    }
    let mut spans = Vec::new();
    let mut i = 0;
    while i < paint.len() {
        let Some(token) = paint[i] else {
            i += 1;
            continue;
        };
        let start = i;
        while i < paint.len() && paint[i] == Some(token) {
            i += 1;
        }
        spans.push(HighlightSpan {
            span: Span::new(BytePos(start), BytePos(i)),
            token,
        });
    }
    Some(Highlights::from_sorted_spans(spans))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_strings_and_comments_get_colour() {
        let text = "// note\nfunction f(): void {\n  const x = \"s\";\n  if (x) { return; }\n}\n";
        let h = highlight(text, Path::new("a.ts")).expect("highlights");
        let token_at = |needle: &str| {
            let at = text.find(needle).unwrap();
            h.all()
                .iter()
                .find(|s| s.span.start.0 <= at && at < s.span.end.0)
                .map(|s| s.token)
        };
        assert_eq!(token_at("const"), Some(StandardToken::Keyword.id()));
        assert_eq!(token_at("return"), Some(StandardToken::Keyword.id()));
        assert_eq!(token_at("\"s\""), Some(StandardToken::String.id()));
        assert_eq!(token_at("// note"), Some(StandardToken::Comment.id()));
    }
}
