//! Small SQL text helpers: splitting statements, telling reads from writes,
//! quoting names and values. No parser: just enough lexing to step over
//! strings, quoted names, comments and Postgres dollar quoting. MySQL's `#`
//! comments are not recognised: in Postgres `#` is an operator.

use std::ops::Range;

use super::conn::Engine;

/// The statements in `text` as byte ranges, trimmed, without their `;`.
/// Statements that are only whitespace and comments are left out.
pub fn split(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut idx = 0;
    while let Some(&byte) = bytes.get(idx) {
        idx = match byte {
            b'\'' | b'"' | b'`' => skip_quoted(bytes, idx, byte),
            b'-' if bytes.get(idx + 1) == Some(&b'-') => skip_line(bytes, idx),
            b'/' if bytes.get(idx + 1) == Some(&b'*') => skip_block(bytes, idx),
            b'$' => skip_dollar(text, idx),
            b';' => {
                push_trimmed(text, start..idx, &mut out);
                start = idx + 1;
                idx + 1
            }
            _ => idx + 1,
        };
    }
    push_trimmed(text, start..text.len(), &mut out);
    out
}

/// The statement the caret (a byte offset) is in or right after, as the
/// caret's own line would suggest: a caret after a statement's `;` on the
/// same line still means that statement.
pub fn statement_at(text: &str, caret: usize) -> Option<Range<usize>> {
    let statements = split(text);
    if let Some(found) = statements
        .iter()
        .find(|range| range.start <= caret && caret <= range.end)
    {
        return Some(found.clone());
    }
    // Between statements: the one before, when it ends on the caret's line;
    // else the next one.
    let line_start = text
        .get(..caret)
        .and_then(|before| before.rfind('\n'))
        .map_or(0, |pos| pos + 1);
    statements
        .iter()
        .rev()
        .find(|range| range.end <= caret && range.end >= line_start)
        .or_else(|| statements.iter().find(|range| range.start >= caret))
        .or_else(|| statements.last())
        .cloned()
}

/// Whether every statement in `sql` only reads. Anything unknown counts as
/// a write, so it is shown and confirmed before it runs.
pub fn reads_only(sql: &str) -> bool {
    let statements = split(sql);
    !statements.is_empty()
        && statements.iter().all(|range| {
            let words = words(sql.get(range.clone()).unwrap_or(""));
            let first = words.first().map_or("", String::as_str);
            match first {
                "select" | "show" | "describe" | "desc" | "values" | "table" => {
                    !words.iter().any(|word| word == "into")
                        && !words.iter().any(|word| word == "for")
                }
                "explain" => !words
                    .iter()
                    .any(|word| word == "analyze" || word == "analyse"),
                "with" => !words.iter().any(|word| {
                    matches!(
                        word.as_str(),
                        "insert" | "update" | "delete" | "merge" | "into"
                    )
                }),
                "pragma" => !sql.get(range.clone()).unwrap_or("").contains('='),
                _ => false,
            }
        })
}

/// The lower-cased words of a statement, outside strings, quoted names and
/// comments.
fn words(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut idx = 0;
    let mut word = String::new();
    while let Some(&byte) = bytes.get(idx) {
        let next = match byte {
            b'\'' | b'"' | b'`' => skip_quoted(bytes, idx, byte),
            b'-' if bytes.get(idx + 1) == Some(&b'-') => skip_line(bytes, idx),
            b'/' if bytes.get(idx + 1) == Some(&b'*') => skip_block(bytes, idx),
            b'$' => skip_dollar(sql, idx),
            _ if byte.is_ascii_alphanumeric() || byte == b'_' => {
                word.push(char::from(byte.to_ascii_lowercase()));
                idx += 1;
                continue;
            }
            _ => idx + 1,
        };
        if !word.is_empty() {
            out.push(std::mem::take(&mut word));
        }
        idx = next;
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

fn push_trimmed(text: &str, range: Range<usize>, out: &mut Vec<Range<usize>>) {
    let Some(part) = text.get(range.clone()) else {
        return;
    };
    if words(part).is_empty() {
        return;
    }
    let lead = part.len() - part.trim_start().len();
    let trail = part.len() - part.trim_end().len();
    out.push(range.start + lead..range.end - trail);
}

/// Past a quoted string or name; a doubled quote stays inside. A backslash
/// escapes the next character in single-quoted strings (MySQL's default,
/// and harmless elsewhere for finding the end).
fn skip_quoted(bytes: &[u8], start: usize, quote: u8) -> usize {
    let mut idx = start + 1;
    while let Some(&byte) = bytes.get(idx) {
        if byte == b'\\' && quote == b'\'' {
            idx += 2;
        } else if byte == quote {
            if bytes.get(idx + 1) == Some(&quote) {
                idx += 2;
            } else {
                return idx + 1;
            }
        } else {
            idx += 1;
        }
    }
    bytes.len()
}

fn skip_line(bytes: &[u8], start: usize) -> usize {
    bytes
        .iter()
        .skip(start)
        .position(|&byte| byte == b'\n')
        .map_or(bytes.len(), |pos| start + pos + 1)
}

fn skip_block(bytes: &[u8], start: usize) -> usize {
    let mut idx = start + 2;
    while idx + 1 < bytes.len() {
        if bytes.get(idx) == Some(&b'*') && bytes.get(idx + 1) == Some(&b'/') {
            return idx + 2;
        }
        idx += 1;
    }
    bytes.len()
}

/// Past a Postgres dollar-quoted string (`$$...$$`, `$tag$...$tag$`); a `$`
/// that does not start one (`$1`) is one character.
fn skip_dollar(text: &str, start: usize) -> usize {
    let rest = text.get(start + 1..).unwrap_or("");
    let tag_len = rest
        .find(|chr: char| !(chr.is_ascii_alphanumeric() || chr == '_'))
        .unwrap_or(rest.len());
    let starts_with_digit = rest.starts_with(|chr: char| chr.is_ascii_digit());
    if starts_with_digit || rest.get(tag_len..tag_len + 1) != Some("$") {
        return start + 1;
    }
    let tag = text.get(start..start + tag_len + 2).unwrap_or("$$");
    let body = start + tag.len();
    text.get(body..)
        .and_then(|after| after.find(tag))
        .map_or(text.len(), |pos| body + pos + tag.len())
}

/// A table or column name, quoted for the engine.
pub fn ident(engine: Engine, name: &str) -> String {
    match engine {
        Engine::MySql => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// A value as a string literal; the server converts it to the column's type.
pub fn literal(engine: Engine, value: Option<&str>) -> String {
    match value {
        None => "NULL".into(),
        Some(text) => {
            let text = text.replace('\'', "''");
            // MySQL treats a backslash in a string as an escape by default.
            let text = if engine == Engine::MySql {
                text.replace('\\', "\\\\")
            } else {
                text
            };
            format!("'{text}'")
        }
    }
}

/// `schema.table`, quoted; SQLite's `main` is left out.
pub fn table_name(engine: Engine, schema: &str, table: &str) -> String {
    if engine == Engine::Sqlite || schema.is_empty() {
        ident(engine, table)
    } else {
        format!("{}.{}", ident(engine, schema), ident(engine, table))
    }
}

/// `UPDATE` for one cell of a row found by its primary key.
pub fn update_cell(
    engine: Engine,
    table: &str,
    (column, value): (&str, Option<&str>),
    key: &[(&str, Option<&str>)],
) -> String {
    let filter: Vec<String> = key
        .iter()
        .map(|(name, value)| match value {
            Some(_) => format!("{} = {}", ident(engine, name), literal(engine, *value)),
            None => format!("{} IS NULL", ident(engine, name)),
        })
        .collect();
    format!(
        "UPDATE {table} SET {} = {} WHERE {}",
        ident(engine, column),
        literal(engine, value),
        filter.join(" AND ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts(text: &str) -> Vec<&str> {
        split(text)
            .into_iter()
            .filter_map(|range| text.get(range))
            .collect()
    }

    #[test]
    fn splits_outside_strings_comments_and_dollar_quotes() {
        let text = "select ';' as a; -- one; two\nselect \"x;y\" from t /* ; */;\n\
                    create function f() returns int as $body$ select 1; $body$ language sql;\n\
                    select $1;  ;  -- only a comment;\n";
        assert_eq!(
            parts(text),
            [
                "select ';' as a",
                "-- one; two\nselect \"x;y\" from t /* ; */",
                "create function f() returns int as $body$ select 1; $body$ language sql",
                "select $1",
            ]
        );
        assert_eq!(parts("select 'it''s; fine'"), ["select 'it''s; fine'"]);
    }

    #[test]
    fn finds_the_statement_under_the_caret() {
        let text = "select 1;\nselect 2; \n\nselect 3";
        let stmt_at = |caret| statement_at(text, caret).and_then(|range| text.get(range));
        assert_eq!(stmt_at(0), Some("select 1"));
        assert_eq!(stmt_at(9), Some("select 1"));
        assert_eq!(stmt_at(20), Some("select 2"));
        // On the empty line: the next statement.
        assert_eq!(stmt_at(21), Some("select 3"));
        assert_eq!(stmt_at(text.len()), Some("select 3"));
        assert_eq!(statement_at("  ", 1), None);
    }

    #[test]
    fn tells_reads_from_writes() {
        assert!(reads_only("select * from t; show tables"));
        assert!(reads_only("with x as (select 1) select * from x"));
        assert!(reads_only("explain select 1"));
        assert!(reads_only("select 'delete' from t"));
        assert!(!reads_only("select * into backup from t"));
        assert!(!reads_only("select * from t for update"));
        assert!(!reads_only("explain analyze delete from t"));
        assert!(!reads_only(
            "with gone as (delete from t returning *) select * from gone"
        ));
        assert!(!reads_only("select 1; update t set a = 1"));
        assert!(!reads_only("pragma journal_mode = wal"));
        assert!(!reads_only("vacuum"));
        assert!(!reads_only(""));
    }

    #[test]
    fn builds_a_quoted_update() {
        let sql = update_cell(
            Engine::Postgres,
            &table_name(Engine::Postgres, "public", "user"),
            ("e\"mail", Some("o'neil@example.nl")),
            &[("id", Some("41"))],
        );
        assert_eq!(
            sql,
            "UPDATE \"public\".\"user\" SET \"e\"\"mail\" = 'o''neil@example.nl' WHERE \"id\" = '41'"
        );
        let sql = update_cell(
            Engine::MySql,
            &table_name(Engine::MySql, "shop", "item"),
            ("path", Some("C:\\x")),
            &[("a", Some("1")), ("b", None)],
        );
        assert_eq!(
            sql,
            "UPDATE `shop`.`item` SET `path` = 'C:\\\\x' WHERE `a` = '1' AND `b` IS NULL"
        );
    }
}
