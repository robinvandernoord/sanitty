//! Code completion and documentation for SQL, from the connection's schema:
//! there is no SQL language server. Knows the tables a statement uses
//! (`from`, `join`, `update`, `into`, with their aliases), so `u.` lists the
//! columns of `auth_user u`.

use std::fmt::Write as _;

use karet_core::CompletionItems;
use karet_core::CompletionKind;
use karet_core::Markup;
use karet_core::MarkupKind;

use super::conn::Engine;
use super::driver::Schema;
use super::driver::Table;

/// Keywords offered alongside the names.
const KEYWORDS: &[&str] = &[
    "select",
    "from",
    "where",
    "and",
    "or",
    "not",
    "null",
    "is",
    "in",
    "like",
    "ilike",
    "between",
    "exists",
    "join",
    "left join",
    "inner join",
    "right join",
    "full join",
    "on",
    "using",
    "group by",
    "order by",
    "having",
    "limit",
    "offset",
    "distinct",
    "as",
    "case",
    "when",
    "then",
    "else",
    "end",
    "insert into",
    "values",
    "update",
    "set",
    "delete from",
    "returning",
    "with",
    "union",
    "union all",
    "asc",
    "desc",
    "count",
    "coalesce",
    "now()",
    "true",
    "false",
    "create table",
    "alter table",
    "drop table",
    "explain",
];

/// Words after which a name is a table.
const TABLE_WORDS: &[&str] = &["from", "join", "update", "into", "table"];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    /// A name or keyword, unquoted.
    Word(String),
    Dot,
    Comma,
    Other,
}

/// The statement's tokens, strings and comments left out. Strings,
/// comments and quoting are found by the same rules as everywhere else
/// ([`super::sql::skip`]); a quoted name is a word without its quotes.
fn tokens(engine: Engine, sql: &str) -> Vec<Token> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut idx = 0;
    while let Some(&byte) = bytes.get(idx) {
        match byte {
            b'"' | b'`' => {
                let end = super::sql::skip(engine, sql, idx).unwrap_or(bytes.len());
                let inner = sql
                    .get(idx + 1..end.saturating_sub(1).max(idx + 1))
                    .unwrap_or("");
                let quote = char::from(byte).to_string();
                out.push(Token::Word(inner.replace(&quote.repeat(2), &quote)));
                idx = end;
            }
            b'.' => {
                out.push(Token::Dot);
                idx += 1;
            }
            b',' => {
                out.push(Token::Comma);
                idx += 1;
            }
            _ => {
                if let Some(end) = super::sql::skip(engine, sql, idx) {
                    idx = end;
                    continue;
                }
                let rest = sql.get(idx..).unwrap_or("");
                let word_len = rest
                    .char_indices()
                    .find(|(_, chr)| !is_name_char(*chr))
                    .map_or(rest.len(), |(pos, _)| pos);
                if word_len > 0 {
                    out.push(Token::Word(rest.get(..word_len).unwrap_or("").to_string()));
                    idx += word_len;
                } else {
                    let step = rest.chars().next().map_or(1, char::len_utf8);
                    if !rest.starts_with(char::is_whitespace) {
                        out.push(Token::Other);
                    }
                    idx += step;
                }
            }
        }
    }
    out
}

fn is_name_char(chr: char) -> bool {
    chr.is_alphanumeric() || chr == '_' || chr == '$'
}

fn is_keyword(word: &str) -> bool {
    matches!(
        word.to_ascii_lowercase().as_str(),
        "where"
            | "join"
            | "left"
            | "right"
            | "inner"
            | "outer"
            | "full"
            | "cross"
            | "on"
            | "using"
            | "group"
            | "order"
            | "having"
            | "limit"
            | "offset"
            | "set"
            | "values"
            | "select"
            | "union"
            | "returning"
            | "natural"
            | "lateral"
            | "window"
            | "as"
            | "from"
            | "default"
            | "for"
    )
}

/// The tables a statement uses, with the alias each goes by (the table's
/// own name when it has none): (alias, table).
fn table_refs<'a>(schema: &'a Schema, engine: Engine, sql: &str) -> Vec<(String, &'a Table)> {
    let tokens = tokens(engine, sql);
    let mut out = Vec::new();
    let mut idx = 0;
    while let Some(token) = tokens.get(idx) {
        idx += 1;
        let Token::Word(word) = token else { continue };
        if !TABLE_WORDS.contains(&word.to_ascii_lowercase().as_str()) {
            continue;
        }
        // One name, or a comma list after `from`.
        while let Some(Token::Word(first)) = tokens.get(idx) {
            let mut name = (None, first.clone());
            idx += 1;
            if tokens.get(idx) == Some(&Token::Dot)
                && let Some(Token::Word(second)) = tokens.get(idx + 1)
            {
                name = (Some(first.clone()), second.clone());
                idx += 2;
            }
            if let Some(Token::Word(word)) = tokens.get(idx)
                && word.eq_ignore_ascii_case("as")
            {
                idx += 1;
            }
            let alias = match tokens.get(idx) {
                Some(Token::Word(alias)) if !is_keyword(alias) => {
                    idx += 1;
                    alias.clone()
                }
                _ => name.1.clone(),
            };
            if let Some(table) = find_table(schema, name.0.as_deref(), &name.1) {
                out.push((alias, table));
            }
            if tokens.get(idx) != Some(&Token::Comma) {
                break;
            }
            idx += 1;
        }
    }
    out
}

/// How many times a statement uses `table` (in `from`, `join`, `update`,
/// `into`): more than once is a self-join.
pub fn table_uses(schema: &Schema, engine: Engine, sql: &str, table: &Table) -> usize {
    table_refs(schema, engine, sql)
        .iter()
        .filter(|(_, used)| used.schema == table.schema && used.name == table.name)
        .count()
}

/// A table by name, case-insensitively; with a schema name, in that schema.
fn find_table<'a>(schema: &'a Schema, in_schema: Option<&str>, name: &str) -> Option<&'a Table> {
    let mut same = schema.tables.iter().filter(|table| {
        table.name.eq_ignore_ascii_case(name)
            && in_schema.is_none_or(|wanted| table.schema.eq_ignore_ascii_case(wanted))
    });
    let first = same.next()?;
    // The usual default schema wins when several schemas have the name.
    Some(
        std::iter::once(first)
            .chain(same)
            .find(|table| matches!(table.schema.as_str(), "public" | "main"))
            .unwrap_or(first),
    )
}

/// The word being typed and the name before a `.` in front of it, if any,
/// from the text before the caret.
fn word_before(before: &str) -> (Option<String>, String) {
    let word: String = before
        .chars()
        .rev()
        .take_while(|chr| is_name_char(*chr))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let rest = before.get(..before.len() - word.len()).unwrap_or("");
    let qualifier = rest.strip_suffix('.').map(|rest| {
        let trimmed = rest.trim_end_matches(['"', '`']);
        trimmed
            .chars()
            .rev()
            .take_while(|chr| is_name_char(*chr))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>()
    });
    (qualifier.filter(|name| !name.is_empty()), word)
}

/// What can go at the caret (a byte offset in `text`).
pub fn completions(
    schema: &Schema,
    engine: Engine,
    text: &str,
    caret: usize,
) -> Vec<CompletionItem> {
    let statement = super::sql::statement_at(engine, text, caret)
        .and_then(|range| text.get(range))
        .unwrap_or("");
    let refs = table_refs(schema, engine, statement);
    let (qualifier, _) = word_before(text.get(..caret).unwrap_or(""));
    let mut items = Vec::new();
    if let Some(qualifier) = qualifier {
        // `alias.` or `table.`: that table's columns; `schema.`: its tables.
        let table = refs
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(&qualifier))
            .map(|(_, table)| *table)
            .or_else(|| find_table(schema, None, &qualifier));
        if let Some(table) = table {
            items.extend(column_items(table, engine, false));
        } else {
            items.extend(
                schema
                    .tables
                    .iter()
                    .filter(|table| table.schema.eq_ignore_ascii_case(&qualifier))
                    .map(|table| table_item(table, engine, false)),
            );
        }
        return items;
    }
    let mut seen: Vec<&str> = Vec::new();
    for (_, table) in &refs {
        if !seen.contains(&table.name.as_str()) {
            seen.push(&table.name);
            items.extend(column_items(table, engine, refs.len() > 1));
        }
    }
    let schemas = schema
        .tables
        .iter()
        .map(|table| table.schema.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    items.extend(
        schema
            .tables
            .iter()
            .map(|table| table_item(table, engine, schemas.len() > 1)),
    );
    items.extend(KEYWORDS.iter().map(|word| {
        let mut item = CompletionItem::new(*word, CompletionKind::Keyword);
        item.sort_text = Some(format!("2{word}"));
        item
    }));
    items
}

fn column_items(table: &Table, engine: Engine, with_table: bool) -> Vec<CompletionItem> {
    table
        .columns
        .iter()
        .map(|column| {
            let mut item = CompletionItem::new(column.name.clone(), CompletionKind::Field);
            item.insert_text = quote_if_needed(engine, &column.name);
            item.detail = Some(if with_table {
                format!("{}  {}", column.sql_type, table.name)
            } else {
                column.sql_type.clone()
            });
            item.documentation = Some(markdown(column_doc(table, &column.name)));
            item.sort_text = Some(format!("0{}", column.name));
            item
        })
        .collect()
}

fn table_item(table: &Table, engine: Engine, with_schema: bool) -> CompletionItem {
    let kind = if table.view {
        CompletionKind::Interface
    } else {
        CompletionKind::Struct
    };
    let mut item = CompletionItem::new(table.name.clone(), kind);
    item.insert_text = quote_if_needed(engine, &table.name);
    let what = if table.view { "view" } else { "table" };
    item.detail = Some(if with_schema {
        format!("{what}  {}", table.schema)
    } else {
        what.to_string()
    });
    item.documentation = Some(markdown(table_doc(table)));
    item.sort_text = Some(format!("1{}", table.name));
    item
}

/// Names the server would fold or misread go in quotes: anything but
/// lower-case letters, digits and `_` (or, in MySQL, other than a plain name).
fn quote_if_needed(engine: Engine, name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|chr| chr.is_ascii_alphabetic() || chr == '_')
        && name.chars().all(|chr| {
            chr.is_ascii_digit()
                || chr == '_'
                || (chr.is_ascii_alphabetic()
                    && (engine == Engine::MySql || chr.is_ascii_lowercase()))
        });
    if plain {
        name.to_string()
    } else {
        super::sql::ident(engine, name)
    }
}

fn markdown(value: String) -> Markup {
    Markup {
        kind: MarkupKind::Markdown,
        value,
    }
}

/// A table as its definition, in a fenced SQL block (the documentation
/// popup colours it as code and shows names exactly).
fn table_doc(table: &Table) -> String {
    let what = if table.view { "view" } else { "table" };
    let mut out = format!("```sql\n{what} {}.{} (\n", table.schema, table.name);
    let width = table
        .columns
        .iter()
        .map(|column| column.name.chars().count())
        .max()
        .unwrap_or(0);
    for (idx, column) in table.columns.iter().enumerate() {
        let comma = if idx + 1 < table.columns.len() {
            ","
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {:width$} {}{}{comma}",
            column.name,
            column.sql_type,
            column_notes(column)
        );
    }
    out.push_str(")\n```\n");
    out
}

fn column_doc(table: &Table, name: &str) -> String {
    let Some(column) = table.columns.iter().find(|column| column.name == name) else {
        return String::new();
    };
    format!(
        "```sql\n{}.{} {}{}\n```\n",
        table.name,
        column.name,
        column.sql_type,
        column_notes(column)
    )
}

/// ` primary key not null`, as in a table definition.
fn column_notes(column: &super::driver::Column) -> String {
    let mut out = String::new();
    if column.primary {
        out.push_str(" primary key");
    }
    if !column.nullable {
        out.push_str(" not null");
    }
    out
}

/// What the name under the caret is: a table (its columns) or a column (its
/// type), as Markdown for the documentation popup, and one line for the
/// status bar.
pub fn describe(
    schema: &Schema,
    engine: Engine,
    text: &str,
    caret: usize,
) -> Option<(String, String)> {
    let after: String = text
        .get(caret..)
        .unwrap_or("")
        .chars()
        .take_while(|chr| is_name_char(*chr))
        .collect();
    let before = text.get(..caret).unwrap_or("");
    let (qualifier, start) = word_before(before);
    let word = format!("{start}{after}");
    if word.is_empty() {
        return None;
    }
    let statement = super::sql::statement_at(engine, text, caret)
        .and_then(|range| text.get(range))
        .unwrap_or("");
    let refs = table_refs(schema, engine, statement);
    let alias_table = |name: &str| {
        refs.iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(name))
            .map(|(_, table)| *table)
            .or_else(|| find_table(schema, None, name))
    };
    // A column of a named table or alias.
    if let Some(qualifier) = &qualifier
        && let Some(table) = alias_table(qualifier)
        && let Some(column) = table
            .columns
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(&word))
    {
        let line = format!("{}.{}: {}", table.name, column.name, column.sql_type);
        return Some((column_doc(table, &column.name), line));
    }
    // A table (or an alias of one).
    if let Some(table) = find_table(schema, qualifier.as_deref(), &word).or_else(|| {
        refs.iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(&word))
            .map(|(_, table)| *table)
    }) {
        let line = format!(
            "{}.{}: {} with {} columns",
            table.schema,
            table.name,
            if table.view { "view" } else { "table" },
            table.columns.len()
        );
        return Some((table_doc(table), line));
    }
    // A column of one of the statement's tables.
    refs.iter().find_map(|(_, table)| {
        let column = table
            .columns
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(&word))?;
        let line = format!("{}.{}: {}", table.name, column.name, column.sql_type);
        Some((column_doc(table, &column.name), line))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::driver::Column;

    fn schema() -> Schema {
        let table = |name: &str, columns: &[&str]| Table {
            schema: "public".into(),
            name: name.into(),
            view: false,
            columns: columns
                .iter()
                .map(|column| Column {
                    name: (*column).into(),
                    sql_type: "text".into(),
                    nullable: true,
                    primary: *column == "id",
                })
                .collect(),
        };
        Schema {
            tables: vec![
                table("auth_user", &["id", "email"]),
                table("shop_order", &["id", "user_id", "Total"]),
            ],
        }
    }

    fn labels(items: &[CompletionItem], kind: CompletionKind) -> Vec<&str> {
        items
            .iter()
            .filter(|item| item.kind == kind)
            .map(|item| item.label.as_str())
            .collect()
    }

    #[test]
    fn finds_tables_and_aliases() {
        let schema = schema();
        let refs = table_refs(
            &schema,
            Engine::Postgres,
            "select * from auth_user u, public.shop_order as o join auth_user on o.user_id = 1",
        );
        let found: Vec<(&str, &str)> = refs
            .iter()
            .map(|(alias, table)| (alias.as_str(), table.name.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("u", "auth_user"),
                ("o", "shop_order"),
                ("auth_user", "auth_user")
            ]
        );
    }

    #[test]
    fn counts_a_self_join() {
        let schema = schema();
        let Some(user) = schema.tables.first() else {
            return;
        };
        let count = |sql| table_uses(&schema, Engine::Postgres, sql, user);
        assert_eq!(count("select * from auth_user"), 1);
        assert_eq!(
            count("select * from auth_user a join auth_user b on a.id = b.id"),
            2
        );
        assert_eq!(count("select * from shop_order"), 0);
    }

    #[test]
    fn completes_columns_after_an_alias() {
        let schema = schema();
        let text = "select o. from shop_order o";
        let items = completions(&schema, Engine::Postgres, text, 9);
        assert_eq!(
            labels(&items, CompletionKind::Field),
            ["id", "user_id", "Total"]
        );
        assert_eq!(labels(&items, CompletionKind::Struct).len(), 0);
        // A mixed-case name is quoted for Postgres.
        let total = items.iter().find(|item| item.label == "Total");
        assert_eq!(
            total.map(|item| item.insert_text.as_str()),
            Some("\"Total\"")
        );
    }

    #[test]
    fn completes_the_statements_columns_tables_and_keywords() {
        let schema = schema();
        let text = "select em from auth_user;\nselect 1";
        let items = completions(&schema, Engine::Postgres, text, 9);
        assert_eq!(labels(&items, CompletionKind::Field), ["id", "email"]);
        assert_eq!(
            labels(&items, CompletionKind::Struct),
            ["auth_user", "shop_order"]
        );
        assert!(items.iter().any(|item| item.label == "where"));
    }

    #[test]
    fn describes_the_name_under_the_caret() {
        let schema = schema();
        let text = "select u.email from auth_user u";
        let column = describe(&schema, Engine::Postgres, text, 10).map(|(_, line)| line);
        assert_eq!(column.as_deref(), Some("auth_user.email: text"));
        let table = describe(&schema, Engine::Postgres, text, 22).map(|(_, line)| line);
        assert_eq!(
            table.as_deref(),
            Some("public.auth_user: table with 2 columns")
        );
        assert_eq!(describe(&schema, Engine::Postgres, text, 6), None);
    }
}
