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
    if let Some(w) = out.first_mut() {
        w.2 = false;
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
    let find = |from: usize, pat: &[char]| -> Option<usize> {
        (from..=chars.len().saturating_sub(pat.len())).find(|&j| chars[j..j + pat.len()] == *pat)
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() && chars[i + 1].is_ascii_punctuation() {
            plain.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '`'
            && let Some(end) = find(i + 1, &['`'])
        {
            flush(&mut plain, &mut out);
            out.push((chars[i + 1..end].iter().collect(), Style::Code));
            i = end + 1;
            continue;
        }
        if (c == '*' || c == '_') && chars.get(i + 1) == Some(&c) {
            if let Some(end) = find(i + 2, &[c, c]).filter(|e| *e > i + 2) {
                flush(&mut plain, &mut out);
                // Bold may wrap code (`**`x`**`): style the inside as its own runs.
                for (t, s) in inline(&chars[i + 2..end].iter().collect::<String>()) {
                    out.push((t, if s == Style::Plain { Style::Bold } else { s }));
                }
                i = end + 2;
                continue;
            }
        } else if (c == '*' || c == '_')
            && chars.get(i + 1).is_some_and(|n| !n.is_whitespace())
            && let Some(end) = find(i + 1, &[c]).filter(|e| *e > i + 1)
        {
            flush(&mut plain, &mut out);
            out.push((chars[i + 1..end].iter().collect(), Style::Italic));
            i = end + 1;
            continue;
        }
        if c == '['
            && let Some(close) = find(i + 1, &[']'])
            && chars.get(close + 1) == Some(&'(')
            && let Some(end) = find(close + 2, &[')'])
        {
            flush(&mut plain, &mut out);
            out.push((chars[i + 1..close].iter().collect(), Style::Link));
            i = end + 1;
            continue;
        }
        plain.push(c);
        i += 1;
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
        let w: Vec<(String, bool)> = words("The **`pathname`** prop")
            .into_iter()
            .map(|(t, _, s)| (t, s))
            .collect();
        assert_eq!(
            w,
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
