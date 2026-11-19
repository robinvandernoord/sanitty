//! Colour for type signatures from hover text (`bound method X.f(a: int) -> str`,
//! `fn parse(s: &str) -> Result<T>`), in the bottom line and the docs panel.
//!
//! Signatures are fragments, not valid source (`bound method ...` does not
//! parse as Python), so a small tokenizer suits them better than tree-sitter.

use karet_core::StandardToken;
use karet_theme::Theme;
use ratatui::style::Style;
use ratatui::text::Span;

const KEYWORDS: &[&str] = &[
    "def",
    "class",
    "async",
    "await",
    "lambda",
    "bound",
    "method",
    "property",
    "module",
    "function",
    "variable",
    "fn",
    "pub",
    "let",
    "mut",
    "const",
    "static",
    "impl",
    "struct",
    "enum",
    "trait",
    "type",
    "where",
    "dyn",
    "ref",
    "crate",
    "Self",
    "interface",
    "readonly",
    "var",
    "export",
    "import",
    "from",
    "as",
    "in",
    "is",
    "not",
    "and",
    "or",
];

const BUILTIN_TYPES: &[&str] = &[
    "int",
    "str",
    "bool",
    "float",
    "bytes",
    "list",
    "dict",
    "set",
    "tuple",
    "object",
    "complex",
    "frozenset",
    "bytearray",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "f32",
    "f64",
    "char",
    "string",
    "number",
    "boolean",
    "any",
    "unknown",
    "never",
    "void",
    "undefined",
    "null",
];

fn style(theme: &Theme, token: StandardToken) -> Style {
    theme.token_style(token.id())
}

/// Spans for one line of signature text, on top of `base`.
pub fn spans(line: &str, theme: &Theme, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    let push = |out: &mut Vec<Span<'static>>, text: String, s: Option<Style>| {
        out.push(Span::styled(text, s.map_or(base, |s| base.patch(s))));
    };
    while i < chars.len() {
        let c = chars[i];
        if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let next = chars[i..].iter().find(|c| **c != ' ').copied();
            let token = if matches!(word.as_str(), "None" | "True" | "False") {
                Some(StandardToken::Constant)
            } else if KEYWORDS.contains(&word.as_str()) {
                Some(StandardToken::Keyword)
            } else if next == Some('(') {
                Some(StandardToken::Function)
            } else if word.starts_with(char::is_uppercase) || BUILTIN_TYPES.contains(&word.as_str())
            {
                Some(StandardToken::Type)
            } else if next == Some(':') || next == Some('=') {
                Some(StandardToken::Parameter)
            } else {
                None
            };
            push(&mut out, word, token.map(|t| style(theme, t)));
        } else if c == '"' || c == '\'' {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != c {
                i += 1;
            }
            i = (i + 1).min(chars.len());
            push(
                &mut out,
                chars[start..i].iter().collect(),
                Some(style(theme, StandardToken::String)),
            );
        } else if c.is_ascii_digit() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            push(
                &mut out,
                chars[start..i].iter().collect(),
                Some(style(theme, StandardToken::Number)),
            );
        } else {
            let start = i;
            // Always take at least one char, so odd characters cannot stall.
            i += 1;
            while i < chars.len()
                && !(chars[i].is_alphanumeric()
                    || chars[i] == '_'
                    || chars[i] == '"'
                    || chars[i] == '\'')
            {
                i += 1;
            }
            // Operators as the editor colours them; brackets and commas plain.
            let text: String = chars[start..i].iter().collect();
            for (part, is_op) in split_operators(&text) {
                push(
                    &mut out,
                    part,
                    is_op.then(|| style(theme, StandardToken::Operator)),
                );
            }
        }
    }
    out
}

/// Split punctuation into runs of operator characters and the rest.
fn split_operators(text: &str) -> Vec<(String, bool)> {
    let is_op = |c: char| "|=-><*+&!~^%/".contains(c);
    let mut out: Vec<(String, bool)> = Vec::new();
    for c in text.chars() {
        match out.last_mut() {
            Some((run, op)) if *op == is_op(c) => run.push(c),
            _ => out.push((c.to_string(), is_op(c))),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_a_python_signature() {
        let theme = Theme::dark();
        let line = "bound method X.get(gid: str, referer: str | None = None) -> Payload | None";
        let spans = spans(line, &theme, Style::default());
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, line);
        let find = |w: &str| spans.iter().find(|s| s.content == w).unwrap().style;
        assert_eq!(find("get"), style(&theme, StandardToken::Function));
        assert_eq!(find("gid"), style(&theme, StandardToken::Parameter));
        assert_eq!(find("str"), style(&theme, StandardToken::Type));
        assert_eq!(find("Payload"), style(&theme, StandardToken::Type));
        assert_eq!(find("None"), style(&theme, StandardToken::Constant));
        assert_eq!(find("bound"), style(&theme, StandardToken::Keyword));
    }

    #[test]
    fn odd_characters_do_not_stall() {
        let theme = Theme::dark();
        assert_eq!(
            spans("x\u{b2} \u{2192} y", &theme, Style::default())
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>(),
            "x\u{b2} \u{2192} y"
        );
    }
}
