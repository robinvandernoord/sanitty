//! Language servers, started lazily the first time a file of their language
//! opens: one main server per language (hover, navigation, colours, problems)
//! plus, for Python, ruff as a linter (problems only). All requests run on tokio
//! tasks and report back to the app loop as [`Event`]s, so the UI never blocks
//! on a server.

use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Deref;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use karet_core::LineCol;
use karet_core::Location;
use karet_core::WorkspaceEdit;
use karet_lsp::LspClient;
use karet_lsp::LspError;
use karet_lsp::LspSpec;
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::Event;
use crate::util::on_path;

/// Which list a location response belongs to.
#[derive(Clone, Copy, Debug)]
pub enum Nav {
    Definition,
    Implementation,
    Usages,
}

/// Shown when a request is not sent because its server lost an edit.
const STUCK: &str = "The language server stopped taking edits; restart sanitty to resync it";

/// A document notification, sent to a server in the order it was queued.
enum Note {
    Open {
        path: PathBuf,
        lang: &'static str,
        version: i32,
        text: String,
    },
    Change {
        path: PathBuf,
        version: i32,
        text: String,
    },
    Save(PathBuf),
    Close(PathBuf),
    /// Answered once every note queued before it has been sent.
    Flushed(oneshot::Sender<()>),
}

/// A running language server. Document notifications go through one queue,
/// so the server sees them in order, and a request first waits for the notes
/// queued before it: a request never overtakes the edit it is about.
#[derive(Clone)]
pub struct Server {
    client: Arc<LspClient>,
    notes: UnboundedSender<Note>,
    /// Set when a notification could not be sent even after retrying: the
    /// server no longer has the editor's text, so no request is sent to it.
    stuck: Arc<AtomicBool>,
}

impl Deref for Server {
    type Target = LspClient;

    fn deref(&self) -> &LspClient {
        &self.client
    }
}

impl Server {
    fn new(client: Arc<LspClient>) -> Self {
        let (notes, mut queue) = mpsc::unbounded_channel();
        let worker = Arc::clone(&client);
        let stuck = Arc::new(AtomicBool::new(false));
        let mark_stuck = Arc::clone(&stuck);
        tokio::spawn(async move {
            while let Some(note) = queue.recv().await {
                if let Note::Flushed(done) = note {
                    let _ = done.send(());
                    continue;
                }
                // A full outbound queue (a server that stopped reading) fails
                // a notification at once; it is retried rather than lost, so
                // the server never misses an edit. Everything queued after it
                // waits, requests included.
                let mut sent_ok = false;
                for attempt in 0..100 {
                    let sent = match &note {
                        Note::Open {
                            path,
                            lang,
                            version,
                            text,
                        } => worker.did_open(path, lang, *version, text).await,
                        Note::Change {
                            path,
                            version,
                            text,
                        } => worker.did_change(path, *version, text).await,
                        Note::Save(path) => worker.did_save(path, None).await,
                        Note::Close(path) => worker.did_close(path).await,
                        Note::Flushed(_) => Ok(()),
                    };
                    match sent {
                        Ok(()) => {
                            sent_ok = true;
                            break;
                        }
                        Err(LspError::Protocol(msg)) if msg.contains("queue is full") => {
                            crate::debug_log(&format!("notification (try {attempt}): {msg}"));
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                        // Anything else (an exited server, a path it cannot
                        // take) will not go better on a retry; it concerns
                        // that one document, not the order of the rest.
                        Err(err) => {
                            crate::debug_log(&format!("notification: {err}"));
                            sent_ok = true;
                            break;
                        }
                    }
                }
                if !sent_ok {
                    mark_stuck.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });
        Self {
            client,
            notes,
            stuck,
        }
    }

    fn note(&self, note: Note) {
        let _ = self.notes.send(note);
    }

    /// Resolves once every notification queued so far has been sent. Taken
    /// when the request is made (on the app loop), awaited in its task.
    /// `false` when the server lost an edit (see `stuck`) or is gone: the
    /// request is then not sent.
    fn flushed(&self) -> impl Future<Output = bool> + Send + 'static {
        let (done, wait) = oneshot::channel();
        self.note(Note::Flushed(done));
        let stuck = Arc::clone(&self.stuck);
        async move { wait.await.is_ok() && !stuck.load(std::sync::atomic::Ordering::Relaxed) }
    }

    /// Tell the server a document is closed.
    pub fn did_close(&self, path: PathBuf) {
        self.note(Note::Close(path));
    }
}

#[derive(Default)]
pub struct Hub {
    clients: HashMap<&'static str, Server>,
    /// The program each running server is (`ty`, `rust-analyzer`), for the
    /// top bar: the keys only say which language a server is for.
    names: HashMap<&'static str, String>,
    starting: HashSet<&'static str>,
    failed: HashMap<&'static str, String>,
}

/// The main server's key: TypeScript and JavaScript share one server.
fn server_key(lang: &str) -> &'static str {
    match lang {
        "python" => "python",
        "rust" => "rust",
        _ => "ts",
    }
}

/// The project's Python environment, the first of:
/// 1. `VIRTUAL_ENV` (an activated venv);
/// 2. `UV_PROJECT_ENVIRONMENT` (where uv keeps it; relative to the workspace);
/// 3. the one directory directly in the workspace root that is a venv,
///    whatever its name;
/// 4. `.venv`, then `venv`, when that finds none or several (and with
///    several but neither name, the first by name).
///
/// A venv is a directory with a `pyvenv.cfg`.
pub fn project_venv(root: &Path) -> Option<PathBuf> {
    let from_env = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.is_empty())
            .map(|value| root.join(value))
            .filter(|dir| is_venv(dir))
    };
    from_env("VIRTUAL_ENV")
        .or_else(|| from_env("UV_PROJECT_ENVIRONMENT"))
        .or_else(|| venv_in(root))
}

fn is_venv(dir: &Path) -> bool {
    dir.join("pyvenv.cfg").is_file()
}

/// The venv directly in `root` (see [`project_venv`], steps 3 and 4).
fn venv_in(root: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(root).map_or_default(|entries| {
        entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|dir| is_venv(dir))
            .collect()
    });
    found.sort();
    if let [only] = found.as_slice() {
        return Some(only.clone());
    }
    [".venv", "venv"]
        .iter()
        .map(|name| root.join(name))
        .find(|dir| is_venv(dir))
        .or_else(|| found.into_iter().next())
}

/// A server started in `venv`, as `activate` would: `VIRTUAL_ENV` set and
/// its `bin` first on PATH, through `env` (the spec has no environment of
/// its own).
fn in_venv(venv: &Path, program: &str, args: Vec<String>, langs: Vec<String>) -> LspSpec {
    let bin = venv.join("bin");
    let path = std::env::var_os("PATH").map_or_else(
        || bin.display().to_string(),
        |path| format!("{}:{}", bin.display(), path.to_string_lossy()),
    );
    let mut command = vec![
        format!("VIRTUAL_ENV={}", venv.display()),
        format!("PATH={path}"),
        program.to_string(),
    ];
    command.extend(args);
    LspSpec::new("env", command, langs)
}

/// The server a spec starts, for messages: the program `env` runs, when it
/// runs in a venv.
fn program_name(spec: &LspSpec) -> String {
    if spec.command == "env" {
        spec.args
            .iter()
            .find(|arg| !arg.contains('='))
            .cloned()
            .unwrap_or_else(|| spec.command.clone())
    } else {
        spec.command.clone()
    }
}

/// No server for `lang` is installed: which to install, and how.
fn not_found(lang: &str) -> String {
    match server_key(lang) {
        "python" => "No Python language server found, in the project's venv or on PATH. \
             Install ty: `uv add --dev ty` in the project, or `uv tool install ty` \
             (basedpyright, pyright and pylsp work too)"
            .into(),
        "rust" => "No Rust language server found. Install rust-analyzer: \
             `rustup component add rust-analyzer`"
            .into(),
        _ => "No TypeScript language server found. Install TypeScript 7 \
             (`npm install -D typescript@7`) or typescript-language-server"
            .into(),
    }
}

/// Every server for a language: the main one first, then linters.
fn server_keys(lang: &str) -> &'static [&'static str] {
    match server_key(lang) {
        // ruff alongside unless the settings turn it off.
        "python" if crate::settings::get().python.ruff => &["python", "ruff"],
        "python" => &["python"],
        "rust" => &["rust"],
        _ => &["ts"],
    }
}

/// Linters only report problems; their absence is not worth a message.
pub fn is_linter(key: &str) -> bool {
    key == "ruff"
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
            langs.iter().map(|lang| (*lang).to_string()).collect(),
        ));
    }
    // The preferred Python server (ty unless the settings say otherwise),
    // then the others in the usual order.
    let python: Vec<(&str, &[&str], &[&str])> = crate::settings::get()
        .python
        .server
        .order()
        .into_iter()
        .map(|server| {
            let (cmd, args) = server.command();
            (cmd, args, &["python"][..])
        })
        .collect();
    let candidates: &[(&str, &[&str], &[&str])] = match key {
        "ruff" => &[("ruff", &["server"], &["python"])],
        "python" => &python,
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
    // Python servers run in the project's environment: one installed there
    // (`uv add --dev ty`) wins over one on PATH, and either way the server
    // gets that environment, so it resolves the project's imports.
    let venv = matches!(key, "python" | "ruff")
        .then(|| project_venv(root))
        .flatten();
    let spec = candidates.iter().find_map(|(cmd, args, langs)| {
        let program = venv
            .as_ref()
            .map(|venv| venv.join("bin").join(cmd))
            .filter(|path| path.is_file())
            .map(|path| path.display().to_string())
            .or_else(|| on_path(cmd).then(|| (*cmd).to_string()))?;
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        let langs = langs.iter().map(|lang| (*lang).to_string()).collect();
        Some(match &venv {
            Some(venv) => in_venv(venv, &program, args, langs),
            None => LspSpec::new(program, args, langs),
        })
    })?;
    if spec.command == "typescript-language-server" {
        // It needs TypeScript itself, and only looks in the project's
        // node_modules. Projects without one (own bundler, no package.json)
        // use a global install, which it must be pointed at.
        let tsserver = find_tsserver(root);
        return Some(spec.with_initialization_options(
            tsserver.map(|path| serde_json::json!({ "tsserver": { "path": path } })),
        ));
    }
    Some(spec)
}

/// A `tsc` of version 7 or later (which serves LSP): the project's own, else
/// the one on PATH. Older versions have no `--lsp`.
fn native_tsc(root: &Path) -> Option<PathBuf> {
    let local = root.join("node_modules/.bin/tsc");
    let candidates = [
        Some(local).filter(|path| path.is_file()),
        on_path("tsc").then(|| PathBuf::from("tsc")),
    ];
    candidates.into_iter().flatten().find(|tsc| {
        version_output(tsc)
            .and_then(|version| {
                version
                    .trim()
                    .strip_prefix("Version ")?
                    .split('.')
                    .next()?
                    .parse::<u32>()
                    .ok()
            })
            .is_some_and(|major| major >= 7)
    })
}

/// What `<program> --version` prints, or `None` if it fails or takes longer
/// than a few seconds (it is then killed).
fn version_output(program: &Path) -> Option<String> {
    use std::io::Read;
    use std::process::Command;
    use std::process::Stdio;
    let mut child = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Read on a thread of its own: a wrapper script can leave a child holding
    // the pipe open after it exits, and the deadline must still hold.
    let mut stdout = child.stdout.take()?;
    let (done, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let read = stdout.read_to_string(&mut out).map(|_| out);
        let _ = done.send(read);
    });
    let read = output.recv_timeout(std::time::Duration::from_secs(5));
    let _ = child.kill();
    let _ = child.wait();
    read.ok()?.ok()
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
        .find(|path| path.is_file())
}

impl Hub {
    /// The main server for `lang`.
    pub fn client(&self, lang: &str) -> Option<Server> {
        self.clients.get(server_key(lang)).cloned()
    }

    /// Every running server for `lang` (main and linters), with its key.
    pub fn running(&self, lang: &str) -> Vec<(&'static str, Server)> {
        server_keys(lang)
            .iter()
            .filter_map(|key| Some((*key, self.clients.get(key)?.clone())))
            .collect()
    }

    /// Any running main server, for workspace-wide queries with no file open.
    pub fn any_client(&self) -> Option<Server> {
        self.clients
            .iter()
            .find(|(key, _)| !is_linter(key))
            .map(|(_, client)| client.clone())
    }

    pub fn status(&self) -> String {
        // Running servers by program; starting and failed ones by language,
        // as which program it will be is not known yet.
        let mut parts: Vec<String> = self
            .clients
            .keys()
            .map(|key| {
                self.names
                    .get(key)
                    .cloned()
                    .unwrap_or_else(|| (*key).to_string())
            })
            .collect();
        parts.extend(self.starting.iter().map(|key| format!("{key}..")));
        parts.extend(
            self.failed
                .iter()
                .filter(|(_, msg)| !msg.is_empty())
                .map(|(key, _)| format!("{key} failed")),
        );
        parts.sort();
        parts.join(" ")
    }

    /// Why `lang`'s main server cannot answer, for a feature that needs it.
    /// One that failed is looked for again, so a server installed since
    /// works without a restart (a failure is otherwise remembered, so it is
    /// not looked up on every key).
    pub fn unavailable(&mut self, lang: &str, root: &Path, tx: &UnboundedSender<Event>) -> String {
        let key = server_key(lang);
        if self.starting.contains(key) {
            return format!("The {lang} language server is still starting");
        }
        let reason = self.failed.remove(key);
        for key in server_keys(lang) {
            self.failed.remove(key);
        }
        self.ensure(lang, root, tx);
        match reason {
            Some(reason) if !reason.is_empty() => format!("{reason}. Looking again..."),
            _ => format!("Starting the {lang} language server..."),
        }
    }

    /// Start the servers for `lang` that are not running or starting yet.
    pub fn ensure(&mut self, lang: &str, root: &Path, tx: &UnboundedSender<Event>) {
        for key in server_keys(lang) {
            self.ensure_key(key, lang, root, tx);
        }
    }

    fn ensure_key(
        &mut self,
        key: &'static str,
        lang: &str,
        root: &Path,
        tx: &UnboundedSender<Event>,
    ) {
        if self.clients.contains_key(key)
            || self.starting.contains(key)
            || self.failed.contains_key(key)
        {
            return;
        }
        self.starting.insert(key);
        let root = root.to_path_buf();
        let tx = tx.clone();
        let lang = lang.to_string();
        tokio::spawn(async move {
            // Finding the server runs programs (`tsc --version`), so it stays
            // off the app loop.
            let found = {
                let root = root.clone();
                tokio::task::spawn_blocking(move || spec_for(key, &root)).await
            };
            let Ok(Some(spec)) = found else {
                // A missing linter is not worth a message (an empty one is
                // not shown); it is remembered, so it is not looked up again.
                let msg = if is_linter(key) {
                    String::new()
                } else {
                    not_found(&lang)
                };
                let _ = tx.send(Event::LspFailed(key, msg));
                return;
            };
            let name = program_name(&spec);
            match LspClient::spawn(spec, &root).await {
                Ok(client) => {
                    let client = Arc::new(client);
                    let mut diags = client.diagnostics();
                    let diag_tx = tx.clone();
                    tokio::spawn(async move {
                        loop {
                            let report = match diags.recv().await {
                                Ok(report) => report,
                                // Fell behind a burst: some reports were
                                // skipped, later ones still come.
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    continue;
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            };
                            let event = Event::Diagnostics(
                                key,
                                report.path,
                                report.version,
                                false,
                                report.diagnostics,
                            );
                            if diag_tx.send(event).is_err() {
                                break;
                            }
                        }
                    });
                    let program = Path::new(&name)
                        .file_name()
                        .map_or_else(|| name.clone(), |file| file.to_string_lossy().into_owned());
                    let _ = tx.send(Event::LspReady(key, program, client));
                }
                Err(err) => {
                    let _ = tx.send(Event::LspFailed(key, format!("{name}: {err}")));
                }
            }
        });
    }

    /// Take a server that finished starting; returns it, ready for use.
    pub fn ready(&mut self, key: &'static str, program: String, client: Arc<LspClient>) -> Server {
        self.starting.remove(key);
        self.names.insert(key, program);
        let server = Server::new(client);
        self.clients.insert(key, server.clone());
        server
    }

    pub fn failed(&mut self, key: &'static str, msg: String) {
        self.starting.remove(key);
        self.failed.insert(key, msg);
    }

    pub fn is_key(lang: &str, key: &str) -> bool {
        server_keys(lang).contains(&key)
    }
}

pub fn did_open(
    (key, server): (&'static str, Server),
    path: PathBuf,
    lang: &'static str,
    version: i32,
    text: String,
    tx: UnboundedSender<Event>,
) {
    server.note(Note::Open {
        path: path.clone(),
        lang,
        version,
        text,
    });
    let flushed = server.flushed();
    tokio::spawn(async move {
        if !flushed.await {
            let _ = tx.send(Event::Status(STUCK.into()));
            return;
        }
        // Right after start-up a server may not answer yet (or not have
        // registered its features); retry for a few seconds.
        // A server can register features after it starts (ty does), so
        // "unsupported" is only believed after a couple of seconds.
        let (mut problems, mut colours) = (false, is_linter(key));
        for attempt in 0..8 {
            let settled = |answer: Answer| match answer {
                Answer::Done => true,
                Answer::Unsupported => attempt >= 4,
                Answer::Failed => false,
            };
            problems = problems || settled(pull(key, &server, &path, version, &tx).await);
            colours = colours || settled(semantic(&server, &path, version, &tx).await);
            if problems && colours {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    });
}

pub fn did_change(
    (key, server): (&'static str, Server),
    path: PathBuf,
    version: i32,
    text: String,
    tx: UnboundedSender<Event>,
) {
    server.note(Note::Change {
        path: path.clone(),
        version,
        text,
    });
    let flushed = server.flushed();
    tokio::spawn(async move {
        if !flushed.await {
            let _ = tx.send(Event::Status(STUCK.into()));
            return;
        }
        pull(key, &server, &path, version, &tx).await;
        if !is_linter(key) {
            semantic(&server, &path, version, &tx).await;
        }
    });
}

/// `version` is the document version the server has, for tagging problems.
pub fn did_save(
    (key, server): (&'static str, Server),
    path: PathBuf,
    version: i32,
    tx: UnboundedSender<Event>,
) {
    server.note(Note::Save(path.clone()));
    let flushed = server.flushed();
    tokio::spawn(async move {
        if !flushed.await {
            let _ = tx.send(Event::Status(STUCK.into()));
            return;
        }
        pull(key, &server, &path, version, &tx).await;
    });
}

/// How a request a server may not support went.
enum Answer {
    Done,
    Unsupported,
    Failed,
}

impl Answer {
    fn from_error(err: &LspError) -> Self {
        if matches!(err, LspError::Unsupported { .. }) {
            Self::Unsupported
        } else {
            Self::Failed
        }
    }
}

/// Ask for a document's problems, for servers (ty) that only report when
/// asked. Servers that push them instead answer `Unsupported`, which is fine.
/// Problems are tagged with the document `version` they describe.
async fn pull(
    key: &'static str,
    client: &LspClient,
    path: &Path,
    version: i32,
    tx: &UnboundedSender<Event>,
) -> Answer {
    let report = match client.pull_diagnostics(path, None).await {
        Ok(report) => report,
        Err(err) => {
            crate::debug_log(&format!("pull {key} {}: {err}", path.display()));
            return Answer::from_error(&err);
        }
    };
    crate::debug_log(&format!(
        "pull {key} {}: {:?} problems",
        path.display(),
        report.diagnostics.as_ref().map(Vec::len)
    ));
    if let Some(diags) = report.diagnostics {
        let _ = tx.send(Event::Diagnostics(
            key,
            path.to_path_buf(),
            Some(version),
            false,
            diags,
        ));
    }
    for other in report.related {
        let _ = tx.send(Event::Diagnostics(
            key,
            other.path,
            None,
            true,
            other.diagnostics,
        ));
    }
    Answer::Done
}

/// Ask for hover text at `pos16` (UTF-16); the answer is tagged with the
/// caret position `pos` (chars) it belongs to.
pub fn hover(
    client: Server,
    path: PathBuf,
    pos: LineCol,
    pos16: LineCol,
    popup: bool,
    tx: UnboundedSender<Event>,
) {
    let flushed = client.flushed();
    tokio::spawn(async move {
        if !flushed.await {
            let _ = tx.send(Event::Status(STUCK.into()));
            return;
        }
        let text = client
            .hover(&path, pos16)
            .await
            .ok()
            .flatten()
            .map(|hover| {
                crate::debug_log(&format!(
                    "hover ({:?}): {:?}",
                    hover.contents.kind, hover.contents.value
                ));
                hover.contents.value
            })
            .filter(|text| !text.trim().is_empty());
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
) -> Answer {
    let Some(legend) = client.semantic_tokens_legend() else {
        return Answer::Unsupported;
    };
    match client.semantic_tokens(path, None).await {
        Ok(tokens) => {
            let _ = tx.send(Event::Semantic(
                path.to_path_buf(),
                version,
                tokens.decode(&legend),
            ));
            Answer::Done
        }
        Err(err) => {
            crate::debug_log(&format!("semantic {}: {err}", path.display()));
            Answer::from_error(&err)
        }
    }
}

pub fn navigate(client: Server, nav: Nav, path: PathBuf, pos: LineCol, tx: UnboundedSender<Event>) {
    let flushed = client.flushed();
    tokio::spawn(async move {
        if !flushed.await {
            let _ = tx.send(Event::Status(STUCK.into()));
            return;
        }
        let result = match nav {
            Nav::Definition => client.definition(&path, pos).await,
            Nav::Implementation => client.implementations(&path, pos).await,
            Nav::Usages => client.references(&path, pos, false).await,
        };
        let _ = tx.send(match result {
            Ok(locs) => Event::Locations(nav, locs),
            Err(err) => Event::Status(format!("{nav:?}: {err}")),
        });
    });
}

/// Ask the first of `servers` that can format `path` (ruff before ty, which
/// cannot) for the edits that reformat it as it is at `version`.
pub fn format(
    servers: Vec<(&'static str, Server)>,
    path: PathBuf,
    instance: u64,
    version: u64,
    indentation: karet_lsp::Indentation,
    tx: UnboundedSender<Event>,
) {
    let Some(client) = servers
        .into_iter()
        .rev()
        .map(|(_, client)| client)
        .find(|client| client.supports_formatting(&path))
    else {
        let _ = tx.send(Event::Status(
            "No formatter for this file (its language servers cannot format)".into(),
        ));
        return;
    };
    let flushed = client.flushed();
    tokio::spawn(async move {
        if !flushed.await {
            let _ = tx.send(Event::Status(STUCK.into()));
            return;
        }
        let _ = tx.send(match client.formatting(&path, indentation).await {
            Ok(edits) => Event::Formatted(path, instance, version, edits),
            Err(err) => Event::Status(format!("Reformat: {err}")),
        });
    });
}

/// Ask `client` for completions at `pos16` (UTF-16), tagged with `generation`.
pub fn complete(
    client: Server,
    path: PathBuf,
    pos16: LineCol,
    trigger: karet_core::CompletionTrigger,
    generation: u64,
    tx: UnboundedSender<Event>,
) {
    let flushed = client.flushed();
    tokio::spawn(async move {
        if !flushed.await {
            let _ = tx.send(Event::Status(STUCK.into()));
            return;
        }
        let answer = client
            .completion(&path, pos16, trigger)
            .await
            .map_err(|err| err.to_string());
        if let Ok(list) = &answer {
            crate::debug_log(&format!(
                "completion {}: {} items{}",
                path.display(),
                list.items.len(),
                if list.incomplete { " (incomplete)" } else { "" }
            ));
        }
        let _ = tx.send(Event::Completions(generation, answer));
    });
}

/// Ask `client` to fill in what it left out of a completion item (its docs,
/// an import it needs). `list` and `index` say which row it is.
pub fn resolve_completion(
    client: Server,
    item: karet_core::CompletionItem,
    list: u64,
    index: usize,
    tx: UnboundedSender<Event>,
) {
    let flushed = client.flushed();
    tokio::spawn(async move {
        if !flushed.await {
            let _ = tx.send(Event::Status(STUCK.into()));
            return;
        }
        let resolved = client
            .resolve_completion(&item)
            .await
            .map(|found| found.map(Box::new))
            .map_err(|err| err.to_string());
        let _ = tx.send(Event::CompletionResolved(list, index, resolved));
    });
}

/// `id` tags the answer, so only the latest rename's answer is applied.
pub fn rename(
    client: Server,
    id: u64,
    path: PathBuf,
    pos: LineCol,
    new_name: String,
    tx: UnboundedSender<Event>,
) {
    let flushed = client.flushed();
    tokio::spawn(async move {
        if !flushed.await {
            let _ = tx.send(Event::Status(STUCK.into()));
            return;
        }
        let _ = tx.send(match client.rename(&path, pos, &new_name).await {
            Ok(edit) => Event::Rename(id, edit),
            Err(err) => Event::Status(format!("Rename: {err}")),
        });
    });
}

/// Turn a definition result into the list a picker shows.
pub fn describe(loc: &Location, root: &Path) -> (String, String) {
    let rel = loc.path.strip_prefix(root).unwrap_or(&loc.path);
    let line = std::fs::read_to_string(&loc.path)
        .ok()
        .and_then(|text| {
            text.lines()
                .nth(loc.range.start.line as usize)
                .map(|line| line.trim().to_string())
        })
        .unwrap_or_default();
    // File name and line first (the directory is in the picker's bottom
    // border), then the code on that line.
    let name = rel.file_name().map_or_else(
        || rel.display().to_string(),
        |file_name| file_name.to_string_lossy().into_owned(),
    );
    (format!("{name}:{}", loc.range.start.line + 1), line)
}

pub type Edit = WorkspaceEdit;

#[cfg(test)]
mod tests {
    use super::*;

    /// The venv in a workspace: the only one, whatever its name; `.venv` or
    /// `venv` among several; else the first by name.
    #[test]
    fn finds_the_venv_in_the_workspace() -> Result<(), Box<dyn std::error::Error>> {
        let base = std::env::temp_dir().join(format!("sanitty-venvs-{}", std::process::id()));
        let make = |root: &Path, names: &[&str]| -> std::io::Result<()> {
            std::fs::create_dir_all(root)?;
            for name in names {
                std::fs::create_dir_all(root.join(name))?;
                std::fs::write(root.join(name).join("pyvenv.cfg"), "")?;
            }
            // Not a venv: no pyvenv.cfg.
            std::fs::create_dir_all(root.join("src"))
        };
        let cases: &[(&str, &[&str], Option<&str>)] = &[
            ("one", &["env"], Some("env")),
            ("dotvenv", &["other", ".venv"], Some(".venv")),
            ("plain", &["other", "venv"], Some("venv")),
            ("several", &["b-env", "a-env"], Some("a-env")),
            ("none", &[], None),
        ];
        for (case, names, want) in cases {
            let root = base.join(case);
            make(&root, names)?;
            assert_eq!(venv_in(&root), want.map(|name| root.join(name)), "{case}");
        }
        let _ = std::fs::remove_dir_all(&base);
        Ok(())
    }

    /// A server installed in the project's `.venv` is used, and started in
    /// that environment.
    #[test]
    fn python_servers_come_from_the_project_venv() -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("sanitty-venv-{}", std::process::id()));
        let bin = root.join(".venv").join("bin");
        std::fs::create_dir_all(&bin)?;
        std::fs::write(root.join(".venv").join("pyvenv.cfg"), "home = /usr/bin\n")?;
        let ty_path = bin.join("ty");
        std::fs::write(&ty_path, "#!/bin/sh\n")?;
        std::fs::set_permissions(&ty_path, std::fs::Permissions::from_mode(0o755))?;
        if std::env::var_os("VIRTUAL_ENV").is_none() {
            let spec = spec_for("python", &root).ok_or("no spec")?;
            assert_eq!(spec.command, "env");
            let venv = format!("VIRTUAL_ENV={}", root.join(".venv").display());
            assert!(spec.args.contains(&venv), "{:?}", spec.args);
            assert!(
                spec.args
                    .iter()
                    .any(|arg| arg.starts_with(&format!("PATH={}:", bin.display())))
            );
            assert_eq!(program_name(&spec), ty_path.display().to_string());
            assert_eq!(spec.args.last().map(String::as_str), Some("server"));
        }
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
