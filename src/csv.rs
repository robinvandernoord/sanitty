//! Rainbow CSV: a colour per column, like `bat` does for `.csv` files.
//!
//! A small RFC 4180 style tokenizer, not a tree-sitter grammar: fields are split
//! on the detected separator, double-quoted fields may contain separators and line
//! breaks, and `""` inside quotes is an escaped quote.

use std::path::Path;

use karet_core::BytePos;
use karet_core::HighlightSpan;
use karet_core::Highlights;
use karet_core::Span;
use karet_core::StandardToken;
use karet_core::TokenId;

/// Theme token classes the columns cycle through. Picked for distinct hues in
/// most themes; the actual colours come from the active theme.
const PALETTE: [StandardToken; 8] = [
    StandardToken::String,
    StandardToken::Type,
    StandardToken::Keyword,
    StandardToken::Tag,
    StandardToken::Constant,
    StandardToken::Function,
    StandardToken::Attribute,
    StandardToken::Namespace,
];

/// Separators are dimmed so the columns stand out.
const SEPARATOR: StandardToken = StandardToken::Comment;

/// Whether a path should be shown as a table.
pub fn is_table(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("csv" | "tsv" | "psv")
    )
}

/// What the tokenizer learned about a table, for the status line.
#[derive(Default)]
pub struct Table {
    pub separator: u8,
    /// Field names from the first record.
    pub header: Vec<String>,
    /// Per line: whether it starts inside a quoted field, and the column index
    /// at its start (non-zero only when a quoted field spans lines).
    line_start: Vec<(bool, u32)>,
    /// Rows that do not fit the header, and an unclosed quote.
    pub problems: Vec<Problem>,
}

/// A structural problem, as a byte range in the text.
pub struct Problem {
    pub start: usize,
    pub end: usize,
    pub message: String,
    pub error: bool,
}

impl Table {
    /// The zero-based column at `byte` within `line`, whose text is `text`.
    pub fn column_at(&self, line: usize, text: &str, byte: usize) -> u32 {
        let (mut quoted, mut col) = self.line_start.get(line).copied().unwrap_or((false, 0));
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < byte.min(bytes.len()) {
            let b = bytes[i];
            if quoted {
                if b == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        i += 1;
                    } else {
                        quoted = false;
                    }
                }
            } else if b == b'"' {
                quoted = true;
            } else if b == self.separator {
                col += 1;
            }
            i += 1;
        }
        col
    }

    /// A status-line label such as `col 4: aanleverroute`.
    pub fn describe(&self, col: u32) -> String {
        match self
            .header
            .get(col as usize)
            .map(|h| h.trim())
            .filter(|h| !h.is_empty())
        {
            Some(name) => format!("col {}: {name}", col + 1),
            None if col as usize >= self.header.len() => format!("col {} (no header)", col + 1),
            None => format!("col {}", col + 1),
        }
    }
}

/// Pick the separator from the extension, else from the first lines: the
/// candidate that appears most often, outside quotes, with the same count on
/// every sampled line wins.
pub fn detect_separator(path: &Path, text: &str) -> u8 {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("tsv") => return b'\t',
        Some("psv") => return b'|',
        _ => {}
    }
    let sample: Vec<&str> = text.lines().filter(|l| !l.is_empty()).take(20).collect();
    let mut best = (b',', 0usize, false);
    for sep in *b",;\t|" {
        let counts: Vec<usize> = sample.iter().map(|l| count_unquoted(l, sep)).collect();
        let first = counts.first().copied().unwrap_or(0);
        let consistent = first > 0 && counts.iter().all(|c| *c == first);
        // Consistency beats frequency; then more separators per line wins.
        if (consistent, first) > (best.2, best.1) {
            best = (sep, first, consistent);
        }
    }
    best.0
}

fn count_unquoted(line: &str, sep: u8) -> usize {
    let mut quoted = false;
    line.bytes()
        .filter(|b| {
            if *b == b'"' {
                quoted = !quoted;
            }
            !quoted && *b == sep
        })
        .count()
}

/// Tokenize the whole text into column-coloured spans.
pub fn highlight(text: &str, separator: u8) -> (Highlights, Table) {
    let bytes = text.as_bytes();
    let mut spans = Vec::new();
    let mut table = Table {
        separator,
        header: parse_header(text, separator),
        line_start: vec![(false, 0)],
        problems: Vec::new(),
    };
    let width = table.header.len();
    let mut col: u32 = 0;
    let mut quoted = false;
    let mut start = 0;
    // Per record: where it starts, where its first field past the header's
    // width starts, and where the last quote opened.
    let mut record_start = 0;
    let mut extra_start: Option<usize> = None;
    let mut quote_start = 0;
    let push = |spans: &mut Vec<HighlightSpan>, a: usize, b: usize, token: TokenId| {
        if b > a {
            spans.push(HighlightSpan {
                span: Span::new(BytePos(a), BytePos(b)),
                token,
            });
        }
    };
    let colour = |col: u32| PALETTE[col as usize % PALETTE.len()].id();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if quoted {
            if b == b'"' {
                if bytes.get(i + 1) == Some(&b'"') {
                    i += 1;
                } else {
                    quoted = false;
                }
            } else if b == b'\n' {
                // A line break inside quotes: close the span on this line so it
                // never covers a newline, and resume on the next one.
                push(&mut spans, start, i, colour(col));
                start = i + 1;
                table.line_start.push((true, col));
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            quoted = true;
            quote_start = i;
        } else if b == separator || b == b'\n' {
            let end = if b == b'\n' && i > start && bytes[i - 1] == b'\r' {
                i - 1
            } else {
                i
            };
            push(&mut spans, start, end, colour(col));
            if b == separator {
                push(&mut spans, i, i + 1, SEPARATOR.id());
                col += 1;
                if col as usize == width {
                    extra_start = Some(i + 1);
                }
            } else {
                check_record(
                    &mut table.problems,
                    width,
                    record_start,
                    end,
                    col as usize + 1,
                    extra_start,
                );
                col = 0;
                extra_start = None;
                record_start = i + 1;
                table.line_start.push((false, 0));
            }
            start = i + 1;
        }
        i += 1;
    }
    push(&mut spans, start, bytes.len(), colour(col));
    if quoted {
        table.problems.push(Problem {
            start: quote_start,
            end: bytes.len(),
            message: "Quote is never closed: the rest of the file is one field".into(),
            error: true,
        });
    } else {
        let end = bytes.len() - usize::from(bytes.ends_with(b"\r"));
        check_record(
            &mut table.problems,
            width,
            record_start,
            end,
            col as usize + 1,
            extra_start,
        );
    }
    (Highlights::from_sorted_spans(spans), table)
}

/// A row with more or fewer fields than the header. Extra fields are marked
/// themselves; a short row is marked as a whole.
fn check_record(
    problems: &mut Vec<Problem>,
    width: usize,
    start: usize,
    end: usize,
    fields: usize,
    extra_start: Option<usize>,
) {
    if end <= start && fields == 1 {
        return; // a blank line
    }
    if fields > width {
        let n = fields - width;
        problems.push(Problem {
            start: extra_start.unwrap_or(start),
            end,
            message: format!(
                "{n} more {} than the header ({fields} instead of {width})",
                if n == 1 { "field" } else { "fields" }
            ),
            error: false,
        });
    } else if fields < width {
        problems.push(Problem {
            start,
            end,
            message: format!(
                "{fields} of {width} fields: this row is missing {}",
                width - fields
            ),
            error: false,
        });
    }
}

/// The first record's fields, unquoted.
fn parse_header(text: &str, separator: u8) -> Vec<String> {
    let sep = separator as char;
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' => quoted = !quoted,
            '\n' if !quoted => break,
            '\r' if !quoted => {}
            c if c == sep && !quoted => fields.push(std::mem::take(&mut field)),
            c => field.push(c),
        }
    }
    fields.push(field);
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_semicolons() {
        let text = "a;b;c\n1;\"x, y\";3\n";
        assert_eq!(detect_separator(Path::new("f.csv"), text), b';');
    }

    #[test]
    fn quoted_separator_stays_in_column() {
        let text = "naam,status,route\nAnglia,\"Aangeschreven, geen reactie\",Onbekend\n";
        let (_, table) = highlight(text, b',');
        assert_eq!(table.header, ["naam", "status", "route"]);
        let line = "Anglia,\"Aangeschreven, geen reactie\",Onbekend";
        let byte = line.find("Onbekend").unwrap();
        assert_eq!(table.column_at(1, line, byte), 2);
    }

    #[test]
    fn multiline_field_carries_column() {
        let text = "a,b,c\n1,\"two\nlines\",3\n";
        let (_, table) = highlight(text, b',');
        assert_eq!(table.column_at(2, "lines\",3", 7), 2);
    }

    #[test]
    fn reports_rows_that_do_not_fit_the_header() {
        let text = "a,b,c\n1,2,3\n1,2,3,4\n1,2\n\n";
        let (_, table) = highlight(text, b',');
        let found: Vec<(&str, &str)> = table
            .problems
            .iter()
            .map(|p| (&text[p.start..p.end], p.message.as_str()))
            .collect();
        assert_eq!(
            found.len(),
            2,
            "{:?}",
            found.iter().map(|f| f.1).collect::<Vec<_>>()
        );
        assert_eq!(found[0].0, "4");
        assert_eq!(found[1].0, "1,2");
    }

    #[test]
    fn reports_an_unclosed_quote() {
        let (_, table) = highlight("a,b\n1,\"open\n", b',');
        assert!(table.problems.iter().any(|p| p.error));
    }
}
