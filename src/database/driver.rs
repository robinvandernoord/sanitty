//! Talking to a database through `sqlx`: connecting, running SQL, reading
//! the schema.
//!
//! SQL typed by the user goes through each server's plain text protocol
//! (`raw_sql`), so several statements can run at once and every value
//! arrives as text, whatever its type: the grid only shows text.

use std::fmt::Write as _;
use std::str::FromStr;
use std::time::Duration;
use std::time::Instant;

use futures_util::TryStreamExt;
use sqlx::Column as _;
use sqlx::Either;
use sqlx::Executor as _;
use sqlx::Statement as _;
use sqlx::mysql::MySqlConnectOptions;
use sqlx::mysql::MySqlPool;
use sqlx::mysql::MySqlPoolOptions;
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPool;
use sqlx::postgres::PgPoolOptions;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;

use super::conn::Connection;
use super::conn::Engine;

/// How long connecting may take before it counts as failed.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// An open connection pool. Cheap to clone.
#[derive(Clone, Debug)]
pub enum Pool {
    Postgres(PgPool),
    MySql(MySqlPool),
    Sqlite(SqlitePool),
}

impl Pool {
    pub fn engine(&self) -> Engine {
        match self {
            Self::Postgres(_) => Engine::Postgres,
            Self::MySql(_) => Engine::MySql,
            Self::Sqlite(_) => Engine::Sqlite,
        }
    }
}

/// What running some SQL gave: the rows of the last statement that returned
/// any, or else how many rows the statements changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResultSet {
    pub columns: Vec<String>,
    /// The table column each result column comes from, when it is a plain
    /// column of a table (not an expression), as the server reports it.
    pub origins: Vec<Option<Origin>>,
    /// `None` is SQL NULL.
    pub rows: Vec<Vec<Option<String>>>,
    /// Rows changed by statements that return none.
    pub affected: u64,
    /// More rows were there than `database.max_rows` (settings).
    pub truncated: bool,
    /// Statements run.
    pub statements: usize,
    /// Every statement only read: no rows means none matched, not none
    /// changed.
    pub reads: bool,
    /// The last statement returned rows (else its columns are looked up
    /// separately, when it is a read).
    pub last_had_rows: bool,
    pub elapsed: Duration,
}

/// A table column a result column comes from. The schema is empty when the
/// server did not say (then the connection's default applies).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub schema: String,
    pub table: String,
    pub column: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Schema {
    pub tables: Vec<Table>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Table {
    /// The schema (Postgres), database (MySQL) or `main` (SQLite).
    pub schema: String,
    pub name: String,
    pub view: bool,
    pub columns: Vec<Column>,
}

impl Table {
    /// The primary key's columns, in table order; empty without one.
    pub fn primary_key(&self) -> Vec<&str> {
        self.columns
            .iter()
            .filter(|column| column.primary)
            .map(|column| column.name.as_str())
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub name: String,
    pub sql_type: String,
    pub nullable: bool,
    pub primary: bool,
}

pub async fn connect(conn: &Connection) -> Result<Pool, String> {
    if let Some(why) = &conn.unreachable {
        return Err(format!("{}: {why}", conn.name));
    }
    let failed = |err: sqlx::Error| format!("{}: {}", conn.name, error_text(&err));
    match conn.engine {
        Engine::Postgres => {
            let mut options = match &conn.url {
                Some(url) => PgConnectOptions::from_str(url).map_err(failed)?,
                None => PgConnectOptions::new_without_pgpass()
                    .host(conn.host.as_deref().unwrap_or("127.0.0.1"))
                    .port(conn.port)
                    .username(&conn.user)
                    .database(&conn.database),
            };
            if !conn.password.is_empty() && (conn.url.is_none() || !url_has_password(conn)) {
                options = options.password(&conn.password);
            }
            let options = options.application_name("sanitty");
            probe(&options).await.map_err(failed)?;
            Ok(Pool::Postgres(
                PgPoolOptions::new()
                    .max_connections(2)
                    .acquire_timeout(CONNECT_TIMEOUT)
                    .connect_lazy_with(options),
            ))
        }
        Engine::MySql => {
            let mut options = if let Some(url) = &conn.url {
                MySqlConnectOptions::from_str(url).map_err(failed)?
            } else {
                let options = MySqlConnectOptions::new()
                    .host(conn.host.as_deref().unwrap_or("127.0.0.1"))
                    .port(conn.port)
                    .username(&conn.user);
                if conn.database.is_empty() {
                    options
                } else {
                    options.database(&conn.database)
                }
            };
            if !conn.password.is_empty() && (conn.url.is_none() || !url_has_password(conn)) {
                options = options.password(&conn.password);
            }
            probe(&options).await.map_err(failed)?;
            Ok(Pool::MySql(
                MySqlPoolOptions::new()
                    .max_connections(2)
                    .acquire_timeout(CONNECT_TIMEOUT)
                    .connect_lazy_with(options),
            ))
        }
        Engine::Sqlite => {
            let options = match (&conn.url, &conn.path) {
                (Some(url), _) => SqliteConnectOptions::from_str(url).map_err(failed)?,
                (None, Some(path)) => SqliteConnectOptions::new().filename(path),
                (None, None) => return Err(format!("{}: no file", conn.name)),
            };
            // Never create a file that is not there: a typo would look like
            // an empty database.
            let options = options.create_if_missing(false);
            probe(&options).await.map_err(failed)?;
            Ok(Pool::Sqlite(
                SqlitePoolOptions::new()
                    .max_connections(2)
                    .acquire_timeout(CONNECT_TIMEOUT)
                    .connect_lazy_with(options),
            ))
        }
    }
}

/// Open one connection and close it again: a pool only reports that it
/// timed out, this gives the server's own reason (wrong password, no such
/// database, connection refused).
async fn probe<O>(options: &O) -> Result<(), sqlx::Error>
where
    O: sqlx::ConnectOptions,
    O::Connection: Sized,
{
    use sqlx::Connection as _;
    let conn = tokio::time::timeout(CONNECT_TIMEOUT, options.connect())
        .await
        .map_err(|_| sqlx::Error::PoolTimedOut)??;
    conn.close().await
}

fn url_has_password(conn: &Connection) -> bool {
    conn.url.as_deref().is_some_and(|url| {
        // scheme://user:password@host
        url.split_once("://")
            .and_then(|(_, rest)| rest.split_once('@'))
            .is_some_and(|(auth, _)| auth.contains(':'))
    })
}

/// Run every statement in `sql`, keeping at most `database.max_rows`
/// (settings) of the result: SQL someone typed.
pub async fn run(pool: &Pool, sql: &str) -> Result<ResultSet, String> {
    run_keeping(pool, sql, crate::settings::get().database.max_rows).await
}

/// [`run`], keeping at most `limit` rows.
async fn run_keeping(pool: &Pool, sql: &str, limit: usize) -> Result<ResultSet, String> {
    let started = Instant::now();
    let text = sql;
    let reads = super::sql::reads_only(pool.engine(), text);
    let sql = sqlx::AssertSqlSafe(text.to_string());
    let mut result = match pool {
        Pool::Postgres(pool) => collect(sqlx::raw_sql(sql).fetch_many(pool), limit).await,
        Pool::MySql(pool) => collect(sqlx::raw_sql(sql).fetch_many(pool), limit).await,
        Pool::Sqlite(pool) => run_sqlite(pool, sql, limit).await,
    }
    .map_err(|err| error_text(&err))?;
    // A read at the end that found nothing still shows its columns.
    let engine = pool.engine();
    if !result.last_had_rows
        && let Some(last) = super::sql::split(engine, text)
            .last()
            .and_then(|range| text.get(range.clone()))
        && super::sql::reads(engine, last)
        && let Some((columns, origins)) = describe_columns(pool, last).await
    {
        result.columns = columns;
        result.origins = origins;
        result.rows.clear();
        result.truncated = false;
    }
    if let Pool::Postgres(pg_pool) = pool {
        resolve_relations(pg_pool, &mut result).await;
    }
    result.elapsed = started.elapsed();
    result.reads = reads;
    Ok(result)
}

/// Postgres reports a result column's table as an OID and column number;
/// look the names up in the catalog. Failing that, the columns just stay
/// without an origin (and cannot be edited).
async fn resolve_relations(pool: &PgPool, result: &mut ResultSet) {
    let mut wanted: Vec<u32> = result
        .origins
        .iter()
        .flatten()
        .filter_map(|origin| origin.table.strip_prefix(OID_MARK)?.parse().ok())
        .collect();
    wanted.sort_unstable();
    wanted.dedup();
    if wanted.is_empty() {
        return;
    }
    let list: Vec<String> = wanted.iter().map(u32::to_string).collect();
    let sql = format!(
        "select c.oid::text, a.attnum::text, n.nspname, c.relname, a.attname
         from pg_attribute a
         join pg_class c on c.oid = a.attrelid
         join pg_namespace n on n.oid = c.relnamespace
         where a.attrelid in ({}) and a.attnum > 0",
        list.join(", ")
    );
    let found = collect(
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql)).fetch_many(pool),
        usize::MAX,
    )
    .await;
    let names: std::collections::HashMap<(String, String), Origin> = found
        .map_or_default(|found| found.rows)
        .into_iter()
        .filter_map(|row| {
            let mut fields = row.into_iter().map(Option::unwrap_or_default);
            let (oid, num) = (fields.next()?, fields.next()?);
            let origin = Origin {
                schema: fields.next()?,
                table: fields.next()?,
                column: fields.next()?,
            };
            Some(((oid, num), origin))
        })
        .collect();
    for slot in &mut result.origins {
        *slot = slot.take().and_then(|origin| {
            let oid = origin.table.strip_prefix(OID_MARK)?.to_string();
            names.get(&(oid, origin.column)).cloned()
        });
    }
}

/// Marks a Postgres origin still to be looked up: the table field holds
/// `OID_MARK` and the OID, the column field the column number.
const OID_MARK: &str = "\u{0}oid:";

/// A cell edit's UPDATE: in a transaction that is committed only when it
/// changed exactly one row, and undone otherwise, so an edit can never
/// change several rows (or a row other than the one shown, if its key
/// changed meanwhile). MySQL counts only rows whose value really changed,
/// so there writing the value a cell already has changes zero rows, which
/// is fine.
pub async fn update_one_row(pool: &Pool, sql: &str) -> Result<ResultSet, String> {
    let started = Instant::now();
    let text = || sqlx::AssertSqlSafe(sql.to_string());
    let failed = |err: sqlx::Error| error_text(&err);
    macro_rules! in_transaction {
        ($pool:expr) => {{
            let mut tx = $pool.begin().await.map_err(failed)?;
            let done = sqlx::raw_sql(text())
                .execute(&mut *tx)
                .await
                .map_err(failed)?;
            let changed = done.rows_affected();
            let unchanged_in_mysql = changed == 0 && pool.engine() == Engine::MySql;
            if changed == 1 || unchanged_in_mysql {
                tx.commit().await.map_err(failed)?;
            } else {
                tx.rollback().await.map_err(failed)?;
            }
            (changed, unchanged_in_mysql)
        }};
    }
    let (changed, unchanged) = match pool {
        Pool::Postgres(pool) => in_transaction!(pool),
        Pool::MySql(pool) => in_transaction!(pool),
        Pool::Sqlite(pool) => in_transaction!(pool),
    };
    match changed {
        1 => {}
        0 if unchanged => {}
        0 => {
            return Err(
                "The row was not found (changed or deleted since?); nothing was written".into(),
            );
        }
        many => {
            return Err(format!(
                "The update would change {many} rows instead of one, so nothing was written"
            ));
        }
    }
    Ok(ResultSet {
        affected: changed,
        statements: 1,
        elapsed: started.elapsed(),
        ..ResultSet::default()
    })
}

/// SQLite reports the last insert, update or delete's count again for a
/// statement that changes no rows (`CREATE`), so the count comes from
/// `total_changes()` before and after, on the same connection.
async fn run_sqlite(
    pool: &SqlitePool,
    sql: sqlx::AssertSqlSafe<String>,
    limit: usize,
) -> Result<ResultSet, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let total = "select total_changes()";
    let before: i64 = sqlx::query_scalar(total).fetch_one(&mut *conn).await?;
    let mut result = collect(sqlx::raw_sql(sql).fetch_many(&mut *conn), limit).await?;
    let after: i64 = sqlx::query_scalar(total).fetch_one(&mut *conn).await?;
    result.affected = u64::try_from(after.saturating_sub(before)).unwrap_or(0);
    Ok(result)
}

/// Gather a statement stream: the rows of the last statement with rows,
/// at most `limit` of them (the rest are counted as cut off, not kept).
async fn collect<R, Q>(
    mut stream: futures_util::stream::BoxStream<'_, Result<Either<Q, R>, sqlx::Error>>,
    limit: usize,
) -> Result<ResultSet, sqlx::Error>
where
    R: sqlx::Row + Origins,
    Q: RowsAffected,
    usize: sqlx::ColumnIndex<R>,
    String: for<'r> sqlx::Decode<'r, R::Database>,
    Vec<u8>: for<'r> sqlx::Decode<'r, R::Database>,
{
    let mut result = ResultSet::default();
    // Rows of the statement still running; replaces the result's rows when
    // it ends, if it had any.
    let mut columns: Vec<String> = Vec::new();
    let mut origins: Vec<Option<Origin>> = Vec::new();
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    let mut truncated = false;
    while let Some(item) = stream.try_next().await? {
        match item {
            Either::Left(done) => {
                result.statements += 1;
                result.last_had_rows = !columns.is_empty();
                if columns.is_empty() {
                    result.affected += done.rows_affected();
                } else {
                    result.columns = std::mem::take(&mut columns);
                    result.origins = std::mem::take(&mut origins);
                    result.rows = std::mem::take(&mut rows);
                    result.truncated = std::mem::take(&mut truncated);
                }
            }
            Either::Right(row) => {
                if columns.is_empty() {
                    columns = row
                        .columns()
                        .iter()
                        .map(|column| column.name().to_string())
                        .collect();
                    origins = (0..row.len()).map(|idx| row.origin(idx)).collect();
                }
                if rows.len() >= limit {
                    truncated = true;
                    continue;
                }
                rows.push((0..row.len()).map(|idx| cell(&row, idx)).collect());
            }
        }
    }
    // A stream that ends without a final result (some drivers) still
    // counts its rows.
    if !columns.is_empty() {
        result.last_had_rows = true;
        result.columns = columns;
        result.origins = origins;
        result.rows = rows;
        result.truncated = truncated;
    }
    Ok(result)
}

/// A value as text: NULL is `None`; bytes that are not text show as hex.
fn cell<R>(row: &R, idx: usize) -> Option<String>
where
    R: sqlx::Row,
    usize: sqlx::ColumnIndex<R>,
    String: for<'r> sqlx::Decode<'r, R::Database>,
    Vec<u8>: for<'r> sqlx::Decode<'r, R::Database>,
{
    match row.try_get_unchecked::<Option<String>, _>(idx) {
        Ok(value) => value,
        Err(_) => match row.try_get_unchecked::<Option<Vec<u8>>, _>(idx) {
            Ok(Some(bytes)) => {
                let mut hex = String::with_capacity(2 + bytes.len() * 2);
                hex.push_str("\\x");
                for byte in bytes {
                    let _ = write!(hex, "{byte:02x}");
                }
                Some(hex)
            }
            Ok(None) => None,
            Err(err) => Some(format!("<{err}>")),
        },
    }
}

/// Where each engine says a result column comes from.
trait Origins {
    fn origin(&self, idx: usize) -> Option<Origin>;
}

/// Where a column comes from, as each engine's column says.
trait Source {
    fn source(&self) -> Option<Origin>;
}

impl Source for sqlx::postgres::PgColumn {
    fn source(&self) -> Option<Origin> {
        let oid = self.relation_id()?;
        let num = self.relation_attribute_no()?;
        // Looked up by name afterwards, see `resolve_relations`.
        Some(Origin {
            schema: String::new(),
            table: format!("{OID_MARK}{}", oid.0),
            column: num.to_string(),
        })
    }
}

/// MySQL and SQLite name the table, as `schema.table` or `table`.
fn named_origin(origin: &sqlx::ColumnOrigin) -> Option<Origin> {
    let found = origin.table_column()?;
    let (schema, table) = match found.table.split_once('.') {
        Some((schema, table)) => (schema.to_string(), table.to_string()),
        None => (String::new(), found.table.to_string()),
    };
    Some(Origin {
        schema,
        table,
        column: found.name.to_string(),
    })
}

impl Source for sqlx::mysql::MySqlColumn {
    fn source(&self) -> Option<Origin> {
        named_origin(&self.origin())
    }
}

impl Source for sqlx::sqlite::SqliteColumn {
    fn source(&self) -> Option<Origin> {
        named_origin(&self.origin())
    }
}

impl<R> Origins for R
where
    R: sqlx::Row,
    <R::Database as sqlx::Database>::Column: Source,
{
    fn origin(&self, idx: usize) -> Option<Origin> {
        self.columns().get(idx)?.source()
    }
}

/// The columns a statement would return, without running it (the server
/// prepares it): for a read that found no rows, so the grid still shows
/// its headers. `None` when the server cannot prepare it (`EXPLAIN` in
/// Postgres, say).
async fn describe_columns(
    pool: &Pool,
    statement: &str,
) -> Option<(Vec<String>, Vec<Option<Origin>>)> {
    fn split<C: sqlx::Column + Source>(columns: &[C]) -> (Vec<String>, Vec<Option<Origin>>) {
        columns
            .iter()
            .map(|column| (column.name().to_string(), column.source()))
            .unzip()
    }
    let sql = || sqlx::SqlSafeStr::into_sql_str(sqlx::AssertSqlSafe(statement.to_string()));
    let found = match pool {
        Pool::Postgres(pool) => split(pool.prepare(sql()).await.ok()?.columns()),
        Pool::MySql(pool) => split(pool.prepare(sql()).await.ok()?.columns()),
        Pool::Sqlite(pool) => split(pool.prepare(sql()).await.ok()?.columns()),
    };
    (!found.0.is_empty()).then_some(found)
}

/// Each engine's query result, for the rows a statement changed.
trait RowsAffected {
    fn rows_affected(&self) -> u64;
}

impl RowsAffected for sqlx::postgres::PgQueryResult {
    fn rows_affected(&self) -> u64 {
        self.rows_affected()
    }
}

impl RowsAffected for sqlx::mysql::MySqlQueryResult {
    fn rows_affected(&self) -> u64 {
        self.rows_affected()
    }
}

impl RowsAffected for sqlx::sqlite::SqliteQueryResult {
    fn rows_affected(&self) -> u64 {
        self.rows_affected()
    }
}

/// Every table and view with its columns, system schemas left out. Each
/// query returns: schema, table, type (`VIEW` or not), column, column type,
/// nullable (`YES`/`NO`), primary key column (`YES`/`NO`).
pub async fn schema(pool: &Pool) -> Result<Schema, String> {
    let sql = match pool {
        Pool::Postgres(_) => {
            "select n.nspname, c.relname,
                    case when c.relkind in ('v', 'm') then 'VIEW' else 'BASE TABLE' end,
                    a.attname, format_type(a.atttypid, a.atttypmod),
                    case when a.attnotnull then 'NO' else 'YES' end,
                    case when exists (select 1 from pg_index i where i.indrelid = c.oid
                                      and i.indisprimary and a.attnum = any(i.indkey))
                         then 'YES' else 'NO' end
             from pg_class c
             join pg_namespace n on n.oid = c.relnamespace
             join pg_attribute a on a.attrelid = c.oid and a.attnum > 0 and not a.attisdropped
             where c.relkind in ('r', 'p', 'v', 'm', 'f')
               and n.nspname not in ('pg_catalog', 'information_schema')
               and n.nspname not like 'pg\\_toast%'
               and n.nspname not like 'pg\\_temp%'
             order by n.nspname, c.relname, a.attnum"
        }
        Pool::MySql(_) => {
            "select c.table_schema, c.table_name, t.table_type, c.column_name, c.column_type,
                    c.is_nullable, if(c.column_key = 'PRI', 'YES', 'NO')
             from information_schema.columns c
             join information_schema.tables t
               on t.table_schema = c.table_schema and t.table_name = c.table_name
             where c.table_schema not in ('mysql', 'information_schema', 'performance_schema', 'sys')
             order by c.table_schema, c.table_name, c.ordinal_position"
        }
        Pool::Sqlite(_) => {
            "select 'main', m.name, case m.type when 'view' then 'VIEW' else 'BASE TABLE' end,
                    p.name, p.type, case when p.\"notnull\" then 'NO' else 'YES' end,
                    case when p.pk > 0 then 'YES' else 'NO' end
             from sqlite_master m join pragma_table_info(m.name) p
             where m.type in ('table', 'view') and m.name not like 'sqlite\\_%' escape '\\'
             order by m.name, p.cid"
        }
    };
    // The whole schema, however many columns: the row limit is for results
    // shown in the grid.
    let result = run_keeping(pool, sql, usize::MAX).await?;
    Ok(schema_from_rows(result.rows))
}

fn schema_from_rows(rows: Vec<Vec<Option<String>>>) -> Schema {
    let mut tables: Vec<Table> = Vec::new();
    for row in rows {
        let mut fields = row.into_iter().map(Option::unwrap_or_default);
        let mut next = || fields.next().unwrap_or_default();
        let (schema, name, kind) = (next(), next(), next());
        let column = Column {
            name: next(),
            sql_type: next(),
            nullable: next() == "YES",
            primary: next() == "YES",
        };
        match tables.last_mut() {
            Some(table) if table.schema == schema && table.name == name => {
                table.columns.push(column);
            }
            _ => tables.push(Table {
                schema,
                name,
                view: kind == "VIEW",
                columns: vec![column],
            }),
        }
    }
    Schema { tables }
}

/// The server's message, without sqlx's "error returned from database:".
fn error_text(err: &sqlx::Error) -> String {
    match err {
        sqlx::Error::Database(server) => server.message().to_string(),
        sqlx::Error::PoolTimedOut => "timed out connecting".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::conn::Origin;

    fn sqlite(path: &std::path::Path) -> Connection {
        Connection {
            name: "test".into(),
            engine: Engine::Sqlite,
            origin: Origin::Config,
            url: None,
            host: None,
            port: 0,
            user: String::new(),
            password: String::new(),
            database: String::new(),
            path: Some(path.to_path_buf()),
            unreachable: None,
        }
    }

    #[tokio::test]
    async fn runs_statements_and_reads_the_schema() -> Result<(), String> {
        let dir = std::env::temp_dir().join(format!("sanitty-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
        let file = dir.join("test.sqlite");
        let _ = std::fs::remove_file(&file);
        // Missing files are not created.
        assert!(connect(&sqlite(&file)).await.is_err());
        std::fs::write(&file, b"").map_err(|err| err.to_string())?;
        let pool = connect(&sqlite(&file)).await?;
        let made = run(
            &pool,
            "create table item (id integer primary key, name text not null, price real, raw blob);
             insert into item (name, price, raw) values ('pen', 1.5, x'00ff'), ('ink', null, null);
             create view cheap as select name from item where price < 2;",
        )
        .await?;
        assert_eq!((made.statements, made.affected), (3, 2));
        assert_eq!(made.columns.len(), 0);

        let got = run(
            &pool,
            "select 1; select id, name, price, raw from item order by id",
        )
        .await?;
        assert_eq!(got.columns, ["id", "name", "price", "raw"]);
        assert_eq!(
            got.rows,
            [
                vec![
                    Some("1".into()),
                    Some("pen".into()),
                    Some("1.5".into()),
                    Some("\\x00ff".into())
                ],
                vec![Some("2".into()), Some("ink".into()), None, None],
            ]
        );

        // Where each column comes from: a table column (renamed or not) or
        // an expression.
        let named = run(
            &pool,
            "select i.id, i.name as label, upper(i.name) from item i",
        )
        .await?;
        let origin = |column: &str| {
            Some(super::Origin {
                schema: String::new(),
                table: "item".into(),
                column: column.into(),
            })
        };
        assert_eq!(named.origins, [origin("id"), origin("name"), None]);

        // A read that finds nothing still has its columns (and where they
        // come from); the last statement's result wins.
        let empty = run(&pool, "select 1 as one; select id, name from item where 0").await?;
        assert_eq!(empty.columns, ["id", "name"]);
        assert_eq!(empty.rows.len(), 0);
        assert_eq!(empty.origins.first(), Some(&origin("id")));

        // A cell edit changes one row or nothing: two rows with a NULL text
        // key both match `IS NULL`, so it is undone.
        run(
            &pool,
            "create table loose (k text primary key, v text);
             insert into loose values (null, 'a'), (null, 'b');",
        )
        .await?;
        let many = update_one_row(&pool, "update loose set v = 'x' where k is null").await;
        assert!(many.is_err_and(|err| err.contains("2 rows")));
        let kept = run(&pool, "select v from loose order by v").await?;
        assert_eq!(kept.rows, [vec![Some("a".into())], vec![Some("b".into())]]);
        let one = update_one_row(&pool, "update item set name = 'pencil' where id = 1").await?;
        assert_eq!(one.affected, 1);
        let none = update_one_row(&pool, "update item set name = 'x' where id = 99").await;
        assert!(none.is_err());

        // The row limit cuts off what is shown, never the schema.
        let cut = run_keeping(
            &pool,
            "select * from loose union all select * from loose",
            2,
        )
        .await?;
        assert_eq!((cut.rows.len(), cut.truncated), (2, true));

        let schema = schema(&pool).await?;
        let listed: Vec<(&str, bool)> = schema
            .tables
            .iter()
            .map(|table| (table.name.as_str(), table.view))
            .collect();
        assert_eq!(listed, [("cheap", true), ("item", false), ("loose", false)]);
        let item = schema.tables.iter().find(|table| table.name == "item");
        assert_eq!(item.map(Table::primary_key), Some(vec!["id"]));
        assert_eq!(
            item.and_then(|table| table.columns.get(1))
                .map(|col| col.nullable),
            Some(false)
        );

        assert!(run(&pool, "select nope from item").await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
