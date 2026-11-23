//! Just enough inline Markdown for hover docs: `code`, **bold**, *italic*,
//! [links](url) shown as their text, and backslash escapes.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    Bold,
    Italic,
    Code,
    Link,
}

/// One prose line as styled words, each with whether a space came before it,
/// ready to be wrapped.
pub fn words(line: &str) -> Vec<(String, Style, bool)> {
    let mut out: Vec<(String, Style, bool)> = Vec::new();
    let mut gap = false;
    for (text, style) in inline(line) {
        // A space at the end of one run separates it from the next.
        let mut space = gap || text.starts_with(char::is_whitespace);
        gap = text.ends_with(char::is_whitespace);
        let mut first = true;
        for word in text.split_whitespace() {
            // A run that starts mid-word (`**x**y`) glues to the word before.
            out.push((word.to_string(), style, if first { space } else { true }));
            first = false;
            space = true;
        }
    }
    if let Some(first_word) = out.first_mut() {
        first_word.2 = false;
    }
    out
}

/// Split a line into styled runs.
pub fn inline(line: &str) -> Vec<(String, Style)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<(String, Style)> = Vec::new();
    let mut plain = String::new();
    let flush = |plain: &mut String, out: &mut Vec<(String, Style)>| {
        if !plain.is_empty() {
            out.push((std::mem::take(plain), Style::Plain));
        }
    };
    // First index at or after `from` where `pat` (never empty) occurs.
    let find = |from: usize, pat: &[char]| -> Option<usize> {
        chars
            .get(from..)?
            .windows(pat.len())
            .position(|window| window == pat)
            .map(|pos| pos + from)
    };
    // The characters in `start..end` as a string.
    let collect = |start: usize, end: usize| -> String {
        chars.get(start..end).unwrap_or_default().iter().collect()
    };
    let mut idx = 0;
    while let Some(&chr) = chars.get(idx) {
        if chr == '\\'
            && let Some(&escaped) = chars
                .get(idx + 1)
                .filter(|next| next.is_ascii_punctuation())
        {
            plain.push(escaped);
            idx += 2;
            continue;
        }
        if chr == '`'
            && let Some(end) = find(idx + 1, &['`'])
        {
            flush(&mut plain, &mut out);
            out.push((collect(idx + 1, end), Style::Code));
            idx = end + 1;
            continue;
        }
        if (chr == '*' || chr == '_') && chars.get(idx + 1) == Some(&chr) {
            if let Some(end) = find(idx + 2, &[chr, chr]).filter(|&pos| pos > idx + 2) {
                flush(&mut plain, &mut out);
                // Bold may wrap code (`**`x`**`): style the inside as its own runs.
                for (text, style) in inline(&collect(idx + 2, end)) {
                    let style = if style == Style::Plain {
                        Style::Bold
                    } else {
                        style
                    };
                    out.push((text, style));
                }
                idx = end + 2;
                continue;
            }
        } else if (chr == '*' || chr == '_')
            && chars.get(idx + 1).is_some_and(|next| !next.is_whitespace())
            && let Some(end) = find(idx + 1, &[chr]).filter(|&pos| pos > idx + 1)
        {
            flush(&mut plain, &mut out);
            out.push((collect(idx + 1, end), Style::Italic));
            idx = end + 1;
            continue;
        }
        if chr == '['
            && let Some(close) = find(idx + 1, &[']'])
            && chars.get(close + 1) == Some(&'(')
            && let Some(end) = find(close + 2, &[')'])
        {
            flush(&mut plain, &mut out);
            out.push((collect(idx + 1, close), Style::Link));
            idx = end + 1;
            continue;
        }
        plain.push(chr);
        idx += 1;
    }
    flush(&mut plain, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_mdn_hover() {
        let runs = inline(
            "The **`pathname`** property, see [MDN Reference](https://developer.mozilla.org/x).",
        );
        assert_eq!(
            runs,
            [
                ("The ".to_string(), Style::Plain),
                ("pathname".to_string(), Style::Code),
                (" property, see ".to_string(), Style::Plain),
                ("MDN Reference".to_string(), Style::Link),
                (".".to_string(), Style::Plain),
            ]
        );
    }

    #[test]
    fn keeps_spaces_between_runs() {
        let pairs: Vec<(String, bool)> = words("The **`pathname`** prop")
            .into_iter()
            .map(|(text, _, space)| (text, space))
            .collect();
        assert_eq!(
            pairs,
            [
                ("The".into(), false),
                ("pathname".into(), true),
                ("prop".into(), true)
            ]
        );
    }

    #[test]
    fn italic_and_escapes() {
        assert_eq!(
            inline("*@param* a\\_b"),
            [
                ("@param".to_string(), Style::Italic),
                (" a_b".to_string(), Style::Plain)
            ]
        );
    }
}
