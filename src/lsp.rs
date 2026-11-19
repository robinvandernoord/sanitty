//! Language servers: one per language, started lazily the first time a file of
//! that language opens. All requests run on tokio tasks and report back to the
//! app loop as [`Event`]s, so the UI never blocks on a server.

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use karet_core::LineCol;
use karet_core::Location;
use karet_core::WorkspaceEdit;
use karet_lsp::LspClient;
use karet_lsp::LspError;
use karet_lsp::LspSpec;
use tokio::sync::mpsc::UnboundedSender;

use crate::Event;

/// Which list a location response belongs to.
#[derive(Clone, Copy, Debug)]
pub enum Nav {
    Definition,
    Implementation,
    Usages,
}

#[derive(Default)]
pub struct Hub {
    clients: HashMap<&'static str, Arc<LspClient>>,
    starting: HashMap<&'static str, ()>,
    failed: HashMap<&'static str, String>,
}

/// A server key: TypeScript and JavaScript share one server.
fn server_key(lang: &str) -> &'static str {
    match lang {
        "python" => "python",
        "rust" => "rust",
        _ => "ts",
    }
}

fn on_path(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

/// The first installed server for a key.
fn spec_for(key: &str, root: &Path) -> Option<LspSpec> {
    // TypeScript 7 (the native rewrite) has its own server: `tsc --lsp`.
    if key == "ts"
        && let Some(tsc) = native_tsc(root)
    {
        let langs = [
            "typescript",
            "typescriptreact",
            "javascript",
            "javascriptreact",
        ];
        return Some(LspSpec::new(
            tsc.display().to_string(),
            vec!["--lsp".into(), "--stdio".into()],
            langs.iter().map(|s| (*s).to_string()).collect(),
        ));
    }
    let candidates: &[(&str, &[&str], &[&str])] = match key {
        // ty first (Astral, fast, beta); the others are fallbacks.
        "python" => &[
            ("ty", &["server"], &["python"]),
            ("basedpyright-langserver", &["--stdio"], &["python"]),
            ("pyright-langserver", &["--stdio"], &["python"]),
            ("pylsp", &[], &["python"]),
        ],
        "rust" => &[("rust-analyzer", &[], &["rust"])],
        _ => &[(
            "typescript-language-server",
            &["--stdio"],
            &[
                "typescript",
                "typescriptreact",
                "javascript",
                "javascriptreact",
            ],
        )],
    };
    let spec = candidates
        .iter()
        .find(|(cmd, _, _)| on_path(cmd))
        .map(|(cmd, args, langs)| {
            LspSpec::new(
                *cmd,
                args.iter().map(|s| (*s).to_string()).collect(),
                langs.iter().map(|s| (*s).to_string()).collect(),
            )
        })?;
    if spec.command == "typescript-language-server" {
        // It needs TypeScript itself, and only looks in the project's
        // node_modules. Projects without one (own bundler, no package.json)
        // use a global install, which it must be pointed at.
        let tsserver = find_tsserver(root);
        return Some(spec.with_initialization_options(
            tsserver.map(|p| serde_json::json!({ "tsserver": { "path": p } })),
        ));
    }
    Some(spec)
}

/// A `tsc` of version 7 or later (which serves LSP): the project's own, else
/// the one on PATH. Older versions have no `--lsp`.
fn native_tsc(root: &Path) -> Option<PathBuf> {
    let local = root.join("node_modules/.bin/tsc");
    let candidates = [
        Some(local).filter(|p| p.is_file()),
        on_path("tsc").then(|| PathBuf::from("tsc")),
    ];
    candidates.into_iter().flatten().find(|tsc| {
        std::process::Command::new(tsc)
            .arg("--version")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|v| {
                v.trim()
                    .strip_prefix("Version ")?
                    .split('.')
                    .next()?
                    .parse::<u32>()
                    .ok()
            })
            .is_some_and(|major| major >= 7)
    })
}

/// `typescript/lib/tsserver.js`: the project's own, else a global install
/// (bun, npm with the usual prefixes).
fn find_tsserver(root: &Path) -> Option<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let bun = std::env::var_os("BUN_INSTALL").map_or_else(|| home.join(".bun"), PathBuf::from);
    let candidates = [
        root.join("node_modules"),
        bun.join("install/global/node_modules"),
        home.join(".npm-global/lib/node_modules"),
        PathBuf::from("/usr/local/lib/node_modules"),
        PathBuf::from("/usr/lib/node_modules"),
    ];
    candidates
        .iter()
        .map(|dir| dir.join("typescript/lib/tsserver.js"))
        .find(|p| p.is_file())
}

impl Hub {
    pub fn client(&self, lang: &str) -> Option<Arc<LspClient>> {
        self.clients.get(server_key(lang)).cloned()
    }

    /// Any running server, for workspace-wide queries with no file open.
    pub fn any_client(&self) -> Option<Arc<LspClient>> {
        self.clients.values().next().cloned()
    }

    pub fn status(&self) -> String {
        let mut parts: Vec<String> = self.clients.keys().map(|k| (*k).to_string()).collect();
        parts.extend(self.starting.keys().map(|k| format!("{k}..")));
        parts.extend(self.failed.keys().map(|k| format!("{k}!")));
        parts.sort();
        parts.join(" ")
    }

    /// Start the server for `lang` if it is not running or starting yet.
    pub fn ensure(&mut self, lang: &str, root: &Path, tx: &UnboundedSender<Event>) {
        let key = server_key(lang);
        if self.clients.contains_key(key)
            || self.starting.contains_key(key)
            || self.failed.contains_key(key)
        {
            return;
        }
        let Some(spec) = spec_for(key, root) else {
            self.failed
                .insert(key, format!("no language server installed for {lang}"));
            let _ = tx.send(Event::Status(format!(
                "No language server found for {lang}"
            )));
            return;
        };
        self.starting.insert(key, ());
        let root = root.to_path_buf();
        let tx = tx.clone();
        tokio::spawn(async move {
            let name = spec.command.clone();
            match LspClient::spawn(spec, &root).await {
                Ok(client) => {
                    let client = Arc::new(client);
                    let mut diags = client.diagnostics();
                    let dtx = tx.clone();
                    tokio::spawn(async move {
                        while let Ok(d) = diags.recv().await {
                            if dtx.send(Event::Diagnostics(d.path, d.diagnostics)).is_err() {
                                break;
                            }
                        }
                    });
                    let _ = tx.send(Event::LspReady(key, client));
                }
                Err(e) => {
                    let _ = tx.send(Event::LspFailed(key, format!("{name}: {e}")));
                }
            }
        });
    }

    pub fn ready(&mut self, key: &'static str, client: Arc<LspClient>) {
        self.starting.remove(key);
        self.clients.insert(key, client);
    }

    pub fn failed(&mut self, key: &'static str, msg: String) {
        self.starting.remove(key);
        self.failed.insert(key, msg);
    }

    pub fn is_key(lang: &str, key: &str) -> bool {
        server_key(lang) == key
    }
}

pub fn did_open(
    client: Arc<LspClient>,
    path: PathBuf,
    lang: &'static str,
    version: i32,
    text: String,
    tx: UnboundedSender<Event>,
) {
    tokio::spawn(async move {
        let _ = client.did_open(&path, lang, version, &text).await;
        // Right after start-up a server may not answer yet (or not have
        // registered its features); retry for a few seconds.
        let (mut problems, mut colours) = (false, false);
        for _ in 0..8 {
            problems = problems || pull(&client, &path, &tx).await;
            colours = colours || semantic(&client, &path, version, &tx).await;
            if problems && colours {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    });
}

pub fn did_change(
    client: Arc<LspClient>,
    path: PathBuf,
    version: i32,
    text: String,
    tx: UnboundedSender<Event>,
) {
    tokio::spawn(async move {
        let _ = client.did_change(&path, version, &text).await;
        pull(&client, &path, &tx).await;
        semantic(&client, &path, version, &tx).await;
    });
}

pub fn did_save(client: Arc<LspClient>, path: PathBuf, tx: UnboundedSender<Event>) {
    tokio::spawn(async move {
        let _ = client.did_save(&path, None).await;
        pull(&client, &path, &tx).await;
    });
}

/// Ask for a document's problems, for servers (ty) that only report when
/// asked. Servers that push them instead answer `Unsupported`, which is fine.
/// Returns whether this is settled: answered, or not offered by the server.
async fn pull(client: &LspClient, path: &Path, tx: &UnboundedSender<Event>) -> bool {
    let report = match client.pull_diagnostics(path, None).await {
        Ok(r) => r,
        Err(e) => {
            crate::debug_log(&format!("pull {}: {e}", path.display()));
            return matches!(e, LspError::Unsupported { .. });
        }
    };
    if let Some(diags) = report.diagnostics {
        let _ = tx.send(Event::Diagnostics(path.to_path_buf(), diags));
    }
    for other in report.related {
        let _ = tx.send(Event::Diagnostics(other.path, other.diagnostics));
    }
    true
}

/// Ask for hover text at `pos16` (UTF-16); the answer is tagged with the
/// caret position `pos` (chars) it belongs to.
pub fn hover(
    client: Arc<LspClient>,
    path: PathBuf,
    pos: LineCol,
    pos16: LineCol,
    popup: bool,
    tx: UnboundedSender<Event>,
) {
    tokio::spawn(async move {
        let text = client
            .hover(&path, pos16)
            .await
            .ok()
            .flatten()
            .map(|h| {
                crate::debug_log(&format!(
                    "hover ({:?}): {:?}",
                    h.contents.kind, h.contents.value
                ));
                h.contents.value
            })
            .filter(|t| !t.trim().is_empty());
        let _ = tx.send(Event::Hover(path, pos, text, popup));
    });
}

/// Ask what every name in the document is (function, parameter, type, ...)
/// so the editor can colour it like the server understands it. Tagged with
/// the document `version` it describes; a stale answer is dropped.
async fn semantic(
    client: &LspClient,
    path: &Path,
    version: i32,
    tx: &UnboundedSender<Event>,
) -> bool {
    let Some(legend) = client.semantic_tokens_legend() else {
        return false;
    };
    match client.semantic_tokens(path, None).await {
        Ok(tokens) => {
            let _ = tx.send(Event::Semantic(
                path.to_path_buf(),
                version,
                tokens.decode(&legend),
            ));
            true
        }
        Err(e) => {
            crate::debug_log(&format!("semantic {}: {e}", path.display()));
            matches!(e, LspError::Unsupported { .. })
        }
    }
}

pub fn navigate(
    client: Arc<LspClient>,
    nav: Nav,
    path: PathBuf,
    pos: LineCol,
    tx: UnboundedSender<Event>,
) {
    tokio::spawn(async move {
        let result = match nav {
            Nav::Definition => client.definition(&path, pos).await,
            Nav::Implementation => client.implementations(&path, pos).await,
            Nav::Usages => client.references(&path, pos, false).await,
        };
        let _ = tx.send(match result {
            Ok(locs) => Event::Locations(nav, locs),
            Err(e) => Event::Status(format!("{nav:?}: {e}")),
        });
    });
}

pub fn rename(
    client: Arc<LspClient>,
    path: PathBuf,
    pos: LineCol,
    new_name: String,
    tx: UnboundedSender<Event>,
) {
    tokio::spawn(async move {
        let _ = tx.send(match client.rename(&path, pos, &new_name).await {
            Ok(edit) => Event::Rename(edit),
            Err(e) => Event::Status(format!("Rename: {e}")),
        });
    });
}

/// Turn a definition result into the list a picker shows.
pub fn describe(loc: &Location, root: &Path) -> (String, String) {
    let rel = loc.path.strip_prefix(root).unwrap_or(&loc.path);
    let line = std::fs::read_to_string(&loc.path)
        .ok()
        .and_then(|t| {
            t.lines()
                .nth(loc.range.start.line as usize)
                .map(|l| l.trim().to_string())
        })
        .unwrap_or_default();
    // File name and line first (the directory is in the picker's bottom
    // border), then the code on that line.
    let name = rel.file_name().map_or_else(
        || rel.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    (format!("{name}:{}", loc.range.start.line + 1), line)
}

pub type Edit = WorkspaceEdit;
