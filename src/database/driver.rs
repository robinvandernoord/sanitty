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
use sqlx::Row as _;
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

/// A result keeps at most this many rows; the rest are not fetched.
pub const MAX_ROWS: usize = 10_000;

/// How long connecting may take before it counts as failed.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// An open connection pool. Cheap to clone.
#[derive(Clone, Debug)]
pub enum Pool {
    Postgres(PgPool),
    MySql(MySqlPool),
    Sqlite(SqlitePool),
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
    /// More rows were there than [`MAX_ROWS`].
    pub truncated: bool,
    /// Statements run.
    pub statements: usize,
    /// Every statement only read: no rows means none matched, not none
    /// changed.
    pub reads: bool,
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

/// Run every statement in `sql`.
pub async fn run(pool: &Pool, sql: &str) -> Result<ResultSet, String> {
    let started = Instant::now();
    let reads = super::sql::reads_only(sql);
    let sql = sqlx::AssertSqlSafe(sql.to_string());
    let mut result = match pool {
        Pool::Postgres(pool) => collect(sqlx::raw_sql(sql).fetch_many(pool)).await,
        Pool::MySql(pool) => collect(sqlx::raw_sql(sql).fetch_many(pool)).await,
        Pool::Sqlite(pool) => run_sqlite(pool, sql).await,
    }
    .map_err(|err| error_text(&err))?;
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
    let found = collect(sqlx::raw_sql(sqlx::AssertSqlSafe(sql)).fetch_many(pool)).await;
    let names: std::collections::HashMap<(String, String), Origin> = found
        .map(|found| found.rows)
        .unwrap_or_default()
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

/// SQLite reports the last insert, update or delete's count again for a
/// statement that changes no rows (`CREATE`), so the count comes from
/// `total_changes()` before and after, on the same connection.
async fn run_sqlite(
    pool: &SqlitePool,
    sql: sqlx::AssertSqlSafe<String>,
) -> Result<ResultSet, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let total = "select total_changes()";
    let before: i64 = sqlx::query_scalar(total).fetch_one(&mut *conn).await?;
    let mut result = collect(sqlx::raw_sql(sql).fetch_many(&mut *conn)).await?;
    let after: i64 = sqlx::query_scalar(total).fetch_one(&mut *conn).await?;
    result.affected = u64::try_from(after.saturating_sub(before)).unwrap_or(0);
    Ok(result)
}

/// Gather a statement stream: the rows of the last statement with rows.
async fn collect<R, Q>(
    mut stream: futures_util::stream::BoxStream<'_, Result<Either<Q, R>, sqlx::Error>>,
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
                if rows.len() >= MAX_ROWS {
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

impl Origins for sqlx::postgres::PgRow {
    fn origin(&self, idx: usize) -> Option<Origin> {
        let column = self.columns().get(idx)?;
        let oid = column.relation_id()?;
        let num = column.relation_attribute_no()?;
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

impl Origins for sqlx::mysql::MySqlRow {
    fn origin(&self, idx: usize) -> Option<Origin> {
        named_origin(&self.columns().get(idx)?.origin())
    }
}

impl Origins for sqlx::sqlite::SqliteRow {
    fn origin(&self, idx: usize) -> Option<Origin> {
        named_origin(&self.columns().get(idx)?.origin())
    }
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
    let result = run(pool, sql).await?;
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

        let schema = schema(&pool).await?;
        let listed: Vec<(&str, bool)> = schema
            .tables
            .iter()
            .map(|table| (table.name.as_str(), table.view))
            .collect();
        assert_eq!(listed, [("cheap", true), ("item", false)]);
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
