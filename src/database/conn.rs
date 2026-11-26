//! Database connections: found in `docker compose config`, overridden or
//! added in `.sanitty/db.toml`.
//!
//! ```toml
//! # .sanitty/db.toml
//! [connections.db]          # same name as a compose service: overrides it
//! port = 5433
//!
//! [connections.reports]     # a connection of its own
//! url = "postgres://reader@reports.internal/reports"
//! password_env = "REPORTS_PASSWORD"
//!
//! [connections.local]
//! engine = "sqlite"
//! path = "pool.sqlite3"       # relative to the workspace
//! ```
//!
//! Discovery runs `docker` and blocks, so callers run it off the app loop.

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

/// The compose files `docker compose` looks for, in its order.
const COMPOSE_FILES: &[&str] = &[
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    MySql,
    Sqlite,
}

impl Engine {
    pub fn name(self) -> &'static str {
        match self {
            Self::Postgres => "postgres",
            Self::MySql => "mysql",
            Self::Sqlite => "sqlite",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" => Some(Self::Postgres),
            "mysql" | "mariadb" => Some(Self::MySql),
            "sqlite" | "sqlite3" => Some(Self::Sqlite),
            _ => None,
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Self::Postgres => 5432,
            Self::MySql => 3306,
            Self::Sqlite => 0,
        }
    }
}

/// Where a connection was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// A `docker compose` service, possibly with overrides from db.toml.
    Compose(String),
    /// `.sanitty/db.toml` alone.
    Config,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    pub name: String,
    pub engine: Engine,
    pub origin: Origin,
    /// A full URL; when set it is used as is and the fields below are ignored
    /// (apart from `password`, filled in when the URL has none).
    pub url: Option<String>,
    /// `None` when it could not be found: a compose service that publishes
    /// no port and is not running. `unreachable` says why.
    pub host: Option<String>,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub database: String,
    /// SQLite's file.
    pub path: Option<PathBuf>,
    pub unreachable: Option<String>,
}

impl Connection {
    /// "postgres, from docker compose: db".
    pub fn describe(&self) -> String {
        let place = match self.engine {
            Engine::Sqlite => self
                .path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            _ if self.url.is_some() => String::new(),
            _ => match &self.host {
                Some(host) => format!("{}@{host}:{}/{}", self.user, self.port, self.database),
                None => String::new(),
            },
        };
        let from = match &self.origin {
            Origin::Compose(service) => format!("from docker compose: {service}"),
            Origin::Config => "from .sanitty/db.toml".into(),
        };
        if place.is_empty() {
            format!("{}, {from}", self.engine.name())
        } else {
            format!("{}, {place}, {from}", self.engine.name())
        }
    }
}

/// All connections for a workspace, compose services first, then db.toml's
/// own, each group by name. Problems (a db.toml that does not parse, docker
/// failing) come back as messages next to whatever was found.
pub fn discover(root: &Path) -> (Vec<Connection>, Vec<String>) {
    let mut problems = Vec::new();
    let mut found = if COMPOSE_FILES.iter().any(|name| root.join(name).is_file()) {
        match compose_config(root) {
            Ok(json) => match from_compose(&json, &|service| container_ip(root, service)) {
                Ok(found) => found,
                Err(err) => {
                    problems.push(format!("docker compose config: unexpected output ({err})"));
                    Vec::new()
                }
            },
            Err(err) => {
                problems.push(format!("docker compose config: {err}"));
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let config_path = root.join(".sanitty").join("db.toml");
    if let Ok(text) = std::fs::read_to_string(&config_path) {
        match toml::from_str::<DbToml>(&text) {
            Ok(config) => {
                problems.extend(apply_config(&mut found, config, root, &|name| {
                    std::env::var(name).ok()
                }));
            }
            Err(err) => problems.push(format!(".sanitty/db.toml: {}", err.message())),
        }
    }
    (found, problems)
}

fn compose_config(root: &Path) -> Result<String, String> {
    let output = Command::new("docker")
        .args(["compose", "config", "--format", "json"])
        .current_dir(root)
        .output()
        .map_err(|err| err.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Compose prints warnings (unset variables) before the real error.
        let last = stderr.lines().rev().find(|line| !line.trim().is_empty());
        return Err(last.unwrap_or("failed").trim().to_string());
    }
    String::from_utf8(output.stdout).map_err(|err| err.to_string())
}

/// The address of a running service's container on its first network, for
/// services that publish no port (sanitty runs on the docker host).
fn container_ip(root: &Path, service: &str) -> Option<String> {
    let run = |args: &[&str]| {
        Command::new("docker")
            .args(args)
            .current_dir(root)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let ids = run(&["compose", "ps", "-q", "--", service])?;
    let id = ids.lines().next()?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    let ips = run(&[
        "inspect",
        "-f",
        "{{range .NetworkSettings.Networks}}{{.IPAddress}} {{end}}",
        &id,
    ])?;
    ips.split_whitespace().next().map(str::to_string)
}

#[derive(Deserialize)]
struct ComposeConfig {
    #[serde(default)]
    services: BTreeMap<String, Service>,
}

#[derive(Deserialize)]
struct Service {
    #[serde(default)]
    image: Option<String>,
    /// `docker compose config` writes it as a map; unset values are null.
    #[serde(default)]
    environment: BTreeMap<String, Option<String>>,
    #[serde(default)]
    ports: Vec<Port>,
}

#[derive(Deserialize)]
struct Port {
    target: u16,
    /// A string, and may be a range ("8000-8001") or missing.
    #[serde(default)]
    published: Option<String>,
    #[serde(default)]
    host_ip: Option<String>,
}

const POSTGRES_KEYS: &[&str] = &["POSTGRES_PASSWORD", "POSTGRES_USER", "POSTGRES_DB"];
const MYSQL_KEYS: &[&str] = &[
    "MYSQL_ROOT_PASSWORD",
    "MYSQL_ALLOW_EMPTY_PASSWORD",
    "MYSQL_RANDOM_ROOT_PASSWORD",
    "MYSQL_DATABASE",
    "MYSQL_USER",
    "MARIADB_ROOT_PASSWORD",
    "MARIADB_ALLOW_EMPTY_ROOT_PASSWORD",
    "MARIADB_RANDOM_ROOT_PASSWORD",
    "MARIADB_DATABASE",
    "MARIADB_USER",
];

/// The database servers among compose services. A service counts by its
/// image (postgres, postgis, timescaledb, pgvector, mysql, mariadb, percona)
/// or, for images built locally, by the server's own environment variables;
/// clients that only have a `POSTGRES_URI` do not.
fn from_compose(
    json: &str,
    ip_of: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<Connection>, String> {
    let config = serde_json::from_str::<ComposeConfig>(json).map_err(|err| err.to_string())?;
    Ok(config
        .services
        .into_iter()
        .filter_map(|(name, service)| server(&name, &service, ip_of))
        .collect())
}

fn server(
    name: &str,
    service: &Service,
    ip_of: &dyn Fn(&str) -> Option<String>,
) -> Option<Connection> {
    let image = service.image.as_deref().unwrap_or("").to_ascii_lowercase();
    // The image name without registry and tag.
    let image = image
        .rsplit('/')
        .next()
        .unwrap_or("")
        .split([':', '@'])
        .next()
        .unwrap_or("");
    let has = |keys: &[&str]| {
        keys.iter()
            .any(|key| service.environment.contains_key(*key))
    };
    // Whole image names only: `acme/celery-postgres-listener` is a client.
    let engine = match image {
        "postgres" | "postgresql" | "postgis" | "timescaledb" | "timescaledb-ha" | "pgvector" => {
            Engine::Postgres
        }
        "mysql" | "mysql-server" | "mariadb" | "percona" | "percona-server" => Engine::MySql,
        _ if has(POSTGRES_KEYS) => Engine::Postgres,
        _ if has(MYSQL_KEYS) => Engine::MySql,
        _ => return None,
    };
    let env = |key: &str| {
        service
            .environment
            .get(key)
            .cloned()
            .flatten()
            .filter(|value| !value.is_empty())
    };
    let (user, password, database) = if engine == Engine::Postgres {
        let user = env("POSTGRES_USER").unwrap_or_else(|| "postgres".into());
        let database = env("POSTGRES_DB").unwrap_or_else(|| user.clone());
        (user, env("POSTGRES_PASSWORD").unwrap_or_default(), database)
    } else {
        let pick = |suffix: &str| {
            env(&format!("MARIADB_{suffix}")).or_else(|| env(&format!("MYSQL_{suffix}")))
        };
        let database = pick("DATABASE").unwrap_or_default();
        match (pick("USER"), pick("PASSWORD")) {
            (Some(user), Some(password)) => (user, password, database),
            _ => (
                "root".into(),
                pick("ROOT_PASSWORD").unwrap_or_default(),
                database,
            ),
        }
    };
    let target = engine.default_port();
    let published = service.ports.iter().find_map(|port| {
        let host_port = port
            .published
            .as_deref()?
            .split('-')
            .next()?
            .parse::<u16>()
            .ok()?;
        (port.target == target).then(|| {
            let host = match port.host_ip.as_deref() {
                None | Some("" | "0.0.0.0" | "::") => "127.0.0.1".to_string(),
                Some(addr) => addr.to_string(),
            };
            (host, host_port)
        })
    });
    let (host, port, unreachable) = match published {
        Some((host, port)) => (Some(host), port, None),
        None => match ip_of(name) {
            Some(addr) => (Some(addr), target, None),
            None => (
                None,
                target,
                Some(format!(
                    "the {name} service publishes no port and is not running"
                )),
            ),
        },
    };
    Some(Connection {
        name: name.to_string(),
        engine,
        origin: Origin::Compose(name.to_string()),
        url: None,
        host,
        port,
        user,
        password,
        database,
        path: None,
        unreachable,
    })
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct DbToml {
    #[serde(default)]
    connections: BTreeMap<String, Entry>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Entry {
    engine: Option<String>,
    url: Option<String>,
    host: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    password: Option<String>,
    /// Read the password from this environment variable, so it need not be
    /// written in the project.
    password_env: Option<String>,
    database: Option<String>,
    path: Option<PathBuf>,
}

/// Merge db.toml into what compose found: an entry named after a service
/// overrides its fields, any other entry is a connection of its own.
fn apply_config(
    found: &mut Vec<Connection>,
    config: DbToml,
    root: &Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    let mut problems = Vec::new();
    for (name, entry) in config.connections {
        let engine = match entry.engine.as_deref().map(Engine::parse) {
            Some(None) => {
                problems.push(format!(
                    ".sanitty/db.toml: {name}: unknown engine (postgres, mysql or sqlite)"
                ));
                continue;
            }
            Some(Some(engine)) => Some(engine),
            None => entry.url.as_deref().and_then(url_engine),
        };
        let password = match &entry.password_env {
            Some(var) => env(var).or_else(|| {
                problems.push(format!(".sanitty/db.toml: {name}: ${var} is not set"));
                entry.password.clone()
            }),
            None => entry.password.clone(),
        };
        let path = entry.path.as_ref().map(|path| root.join(path));
        if let Some(conn) = found.iter_mut().find(|conn| conn.name == name) {
            if let Some(engine) = engine {
                conn.engine = engine;
            }
            if entry.url.is_some() {
                conn.url.clone_from(&entry.url);
            }
            if let Some(host) = entry.host {
                conn.host = Some(host);
                conn.unreachable = None;
            }
            if let Some(port) = entry.port {
                conn.port = port;
                // A port given here is reachable on the host itself.
                if conn.host.is_none() {
                    conn.host = Some("127.0.0.1".into());
                    conn.unreachable = None;
                }
            }
            if let Some(user) = entry.user {
                conn.user = user;
            }
            if let Some(password) = password {
                conn.password = password;
            }
            if let Some(database) = entry.database {
                conn.database = database;
            }
            if path.is_some() {
                conn.path = path;
            }
            continue;
        }
        let Some(engine) = engine.or_else(|| path.as_ref().map(|_| Engine::Sqlite)) else {
            problems.push(format!(
                ".sanitty/db.toml: {name}: needs an engine, a url or a path"
            ));
            continue;
        };
        if engine == Engine::Sqlite && path.is_none() && entry.url.is_none() {
            problems.push(format!(".sanitty/db.toml: {name}: sqlite needs a path"));
            continue;
        }
        let user = entry.user.unwrap_or_else(|| match engine {
            Engine::MySql => "root".into(),
            _ => "postgres".into(),
        });
        found.push(Connection {
            name,
            engine,
            origin: Origin::Config,
            url: entry.url,
            host: Some(entry.host.unwrap_or_else(|| "127.0.0.1".into())),
            port: entry.port.unwrap_or_else(|| engine.default_port()),
            database: entry.database.unwrap_or_else(|| user.clone()),
            user,
            password: password.unwrap_or_default(),
            path,
            unreachable: None,
        });
    }
    problems
}

fn url_engine(url: &str) -> Option<Engine> {
    Engine::parse(url.split(':').next()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPOSE: &str = r#"{
      "services": {
        "pg-0": {
          "build": {"context": "."},
          "environment": {"POSTGRES_DB": "app", "POSTGRES_PASSWORD": "secret",
                          "POSTGRES_USER": "app", "POSTGRES_MAX_CONNECTIONS": "200"},
          "ports": [{"mode": "ingress", "target": 5432, "published": "5460", "protocol": "tcp"}]
        },
        "web": {"build": {"context": "."}, "environment": {"POSTGRES_URI": "postgres://x"}},
        "listener": {"image": "acme/celery-postgres-listener:latest",
                     "environment": {"POSTGRES_URI": "postgres://x"}},
        "maria": {"image": "docker.io/library/mariadb:11", "environment":
                  {"MARIADB_ROOT_PASSWORD": "root", "MARIADB_DATABASE": "shop"}},
        "redis": {"image": "redis", "ports": [{"target": 6379, "published": "6404"}]}
      }
    }"#;

    fn addr(service: &str) -> Option<String> {
        (service == "maria").then(|| "172.18.0.5".to_string())
    }

    fn named<'a>(found: &'a [Connection], name: &str) -> Result<&'a Connection, String> {
        found
            .iter()
            .find(|conn| conn.name == name)
            .ok_or_else(|| format!("no connection {name}"))
    }

    #[test]
    fn finds_servers_by_image_or_environment() -> Result<(), String> {
        let found = from_compose(COMPOSE, &addr)?;
        let names: Vec<&str> = found.iter().map(|conn| conn.name.as_str()).collect();
        assert_eq!(names, ["maria", "pg-0"]);
        let postgres = named(&found, "pg-0")?;
        assert_eq!(postgres.engine, Engine::Postgres);
        assert_eq!(postgres.host.as_deref(), Some("127.0.0.1"));
        assert_eq!(
            (
                postgres.port,
                postgres.user.as_str(),
                postgres.database.as_str()
            ),
            (5460, "app", "app")
        );
        assert_eq!(postgres.password, "secret");
        // Not published: the container's own address and port.
        let maria = named(&found, "maria")?;
        assert_eq!(maria.engine, Engine::MySql);
        assert_eq!(
            (maria.host.as_deref(), maria.port),
            (Some("172.18.0.5"), 3306)
        );
        assert_eq!(
            (maria.user.as_str(), maria.database.as_str()),
            ("root", "shop")
        );
        Ok(())
    }

    #[test]
    fn db_toml_overrides_services_and_adds_its_own() -> Result<(), String> {
        let mut found = from_compose(COMPOSE, &|_| None)?;
        assert!(named(&found, "maria")?.unreachable.is_some());
        let config: DbToml = toml::from_str(
            r#"
            [connections.maria]
            port = 3307
            [connections.reports]
            url = "postgres://reader@reports/reports"
            password_env = "REPORTS_PW"
            [connections.local]
            path = "db.sqlite3"
            [connections.broken]
            host = "x"
            "#,
        )
        .map_err(|err| err.to_string())?;
        let env = |name: &str| (name == "REPORTS_PW").then(|| "pw".to_string());
        let problems = apply_config(&mut found, config, Path::new("/ws"), &env);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems.iter().all(|problem| problem.contains("broken")));
        let maria = named(&found, "maria")?;
        assert_eq!(
            (maria.host.as_deref(), maria.port),
            (Some("127.0.0.1"), 3307)
        );
        assert!(maria.unreachable.is_none());
        let local = named(&found, "local")?;
        assert_eq!(local.engine, Engine::Sqlite);
        assert_eq!(local.path, Some(PathBuf::from("/ws/db.sqlite3")));
        let reports = named(&found, "reports")?;
        assert_eq!(reports.engine, Engine::Postgres);
        assert_eq!(reports.password, "pw");
        Ok(())
    }
}
