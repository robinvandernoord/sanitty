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
    let mut pos = 0;
    let push = |out: &mut Vec<Span<'static>>, text: String, token_style: Option<Style>| {
        out.push(Span::styled(
            text,
            token_style.map_or(base, |patch| base.patch(patch)),
        ));
    };
    while let Some(&chr) = chars.get(pos) {
        let start = pos;
        if chr.is_alphabetic() || chr == '_' {
            pos = run_end(&chars, pos, |next| next.is_alphanumeric() || next == '_');
            let word = collect(&chars, start, pos);
            let next = chars.iter().skip(pos).find(|next| **next != ' ').copied();
            let token = word_token(&word, next);
            push(&mut out, word, token.map(|token| style(theme, token)));
        } else if chr == '"' || chr == '\'' {
            pos = run_end(&chars, pos + 1, |next| next != chr);
            pos = (pos + 1).min(chars.len());
            push(
                &mut out,
                collect(&chars, start, pos),
                Some(style(theme, StandardToken::String)),
            );
        } else if chr.is_ascii_digit() {
            pos = run_end(&chars, pos, |next| {
                next.is_ascii_alphanumeric() || next == '.'
            });
            push(
                &mut out,
                collect(&chars, start, pos),
                Some(style(theme, StandardToken::Number)),
            );
        } else {
            // Always take at least one char, so odd characters cannot stall.
            pos = run_end(&chars, pos + 1, |next| {
                !(next.is_alphanumeric() || next == '_' || next == '"' || next == '\'')
            });
            // Operators as the editor colours them; brackets and commas plain.
            let text = collect(&chars, start, pos);
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

/// The index of the first char at or after `from` that fails `keep`, or the
/// length of `chars` when they all pass.
fn run_end(chars: &[char], from: usize, keep: impl Fn(char) -> bool) -> usize {
    from + chars
        .iter()
        .skip(from)
        .take_while(|chr| keep(**chr))
        .count()
}

/// The chars from `start` up to `end` as a string.
fn collect(chars: &[char], start: usize, end: usize) -> String {
    chars.get(start..end).unwrap_or_default().iter().collect()
}

/// The token class of an identifier, given the first non-space char after it.
fn word_token(word: &str, next: Option<char>) -> Option<StandardToken> {
    if matches!(word, "None" | "True" | "False") {
        Some(StandardToken::Constant)
    } else if KEYWORDS.contains(&word) {
        Some(StandardToken::Keyword)
    } else if next == Some('(') {
        Some(StandardToken::Function)
    } else if word.starts_with(char::is_uppercase) || BUILTIN_TYPES.contains(&word) {
        Some(StandardToken::Type)
    } else if next == Some(':') || next == Some('=') {
        Some(StandardToken::Parameter)
    } else {
        None
    }
}

/// Split punctuation into runs of operator characters and the rest.
fn split_operators(text: &str) -> Vec<(String, bool)> {
    let is_op = |chr: char| "|=-><*+&!~^%/".contains(chr);
    let mut out: Vec<(String, bool)> = Vec::new();
    for chr in text.chars() {
        match out.last_mut() {
            Some((run, run_is_op)) if *run_is_op == is_op(chr) => run.push(chr),
            _ => out.push((chr.to_string(), is_op(chr))),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_a_python_signature() -> Result<(), Box<dyn std::error::Error>> {
        let theme = Theme::dark();
        let line = "bound method X.get(gid: str, referer: str | None = None) -> Payload | None";
        let spans = spans(line, &theme, Style::default());
        let text: String = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(text, line);
        let find = |word: &str| {
            spans
                .iter()
                .find(|span| span.content == word)
                .map(|span| span.style)
                .ok_or_else(|| format!("no span for {word}"))
        };
        assert_eq!(find("get")?, style(&theme, StandardToken::Function));
        assert_eq!(find("gid")?, style(&theme, StandardToken::Parameter));
        assert_eq!(find("str")?, style(&theme, StandardToken::Type));
        assert_eq!(find("Payload")?, style(&theme, StandardToken::Type));
        assert_eq!(find("None")?, style(&theme, StandardToken::Constant));
        assert_eq!(find("bound")?, style(&theme, StandardToken::Keyword));
        Ok(())
    }

    #[test]
    fn odd_characters_do_not_stall() {
        let theme = Theme::dark();
        assert_eq!(
            spans("x\u{b2} \u{2192} y", &theme, Style::default())
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>(),
            "x\u{b2} \u{2192} y"
        );
    }
}
