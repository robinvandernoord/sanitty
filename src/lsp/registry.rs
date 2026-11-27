//! Which language servers exist: the built-in ones (Python, ruff, Rust,
//! TypeScript) and any added or changed in `settings.toml`, one `[lsp.<name>]`
//! section each:
//!
//! ```toml
//! [lsp.go]
//! command = "gopls"                  # program on PATH, or a path
//! args = []                          # its arguments (default none)
//! extensions = ["go"]                # files it serves
//! install = "`go install golang.org/x/tools/gopls@latest`"   # shown when missing
//! # label = "Go"                     # name in messages (default: the section name)
//! # language_id = "go"               # what the server is told (default: the section name)
//! # language_ids = { tsx = "typescriptreact" }   # per extension, when they differ
//! # linter = false                   # problems only, alongside the main server
//! # enabled = true
//! # init_options = { ... }           # the server's initializationOptions
//! ```
//!
//! A section named after a built-in (`python`, `ruff`, `rust`, `ts`) changes
//! it: `enabled = false` turns it off, `extensions`, `label`, `linter` and
//! `install` replace its own. Giving `python`, `ruff` or `ts` a `command`
//! replaces their lookup (the project's venv, the fallback servers, `tsc
//! --lsp`) with just that program.
//!
//! An extension already served keeps its language id, so a linter added for
//! `py` files serves `python`. Each language has one main server, the first
//! enabled non-linter (built-ins first, then sections by name); linters come
//! with it.
//!
//! Keys and language ids are `&'static str` everywhere else: the registry
//! borrows them from the settings, which are loaded once and never dropped.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::settings::LspEntry;
use crate::settings::Settings;

/// How a server's program is found.
#[derive(Clone, Debug, PartialEq)]
pub enum Resolver {
    /// The preferred Python server (settings `python.server`), then the
    /// others; the project's venv first.
    Python,
    /// ruff, the project's venv first.
    Ruff,
    /// `tsc --lsp` (TypeScript 7), else typescript-language-server.
    TypeScript,
    /// One program, on PATH or by path.
    Command {
        command: String,
        args: Vec<String>,
        init_options: Option<serde_json::Value>,
    },
}

/// One language server.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerDef<'s> {
    /// What it is called in the settings and the top bar: `rust`, `go`.
    pub key: &'s str,
    /// What it is called in messages: "Rust", "Go".
    pub label: &'s str,
    /// Only reports problems, alongside a language's main server.
    pub linter: bool,
    /// The extensions it serves, each with the language id the server is
    /// told for those files.
    pub extensions: Vec<(&'s str, &'s str)>,
    pub resolver: Resolver,
    /// Shown when no working program is found.
    pub missing: String,
}

impl ServerDef<'_> {
    /// The language ids it serves, each once, in order.
    pub fn languages(&self) -> Vec<&str> {
        let mut langs: Vec<&str> = Vec::new();
        for (_, lang) in &self.extensions {
            if !langs.contains(lang) {
                langs.push(lang);
            }
        }
        langs
    }

    fn serves(&self, lang: &str) -> bool {
        self.extensions.iter().any(|(_, served)| *served == lang)
    }
}

/// Every enabled server, in order: built-ins, then settings sections by name.
#[derive(Clone, Debug, PartialEq)]
pub struct Registry<'s> {
    servers: Vec<ServerDef<'s>>,
}

impl<'s> Registry<'s> {
    /// The built-ins with `settings` applied; problems come back as messages
    /// for the status line (a section with one is left out, or partly kept).
    pub fn build(settings: &'s Settings) -> (Self, Vec<String>) {
        let mut servers = builtins(settings);
        let mut problems = Vec::new();
        for (name, entry) in &settings.lsp {
            let name = name.as_str();
            if servers.iter().any(|def| def.key == name) {
                let known = known_extensions(&servers);
                servers.retain_mut(|def| {
                    if def.key != name {
                        return true;
                    }
                    if entry.enabled == Some(false) {
                        return false;
                    }
                    apply(def, entry, &known, &mut problems);
                    true
                });
            } else if entry.enabled != Some(false) {
                let known = known_extensions(&servers);
                if let Some(def) = added(name, entry, &known, &mut problems) {
                    servers.push(def);
                }
            }
        }
        check_overlaps(&mut servers, &mut problems);
        (Self { servers }, problems)
    }

    /// The language id for files with `ext`, if a server serves them.
    pub fn language(&self, ext: &str) -> Option<&'s str> {
        self.servers.iter().find_map(|def| {
            def.extensions
                .iter()
                .find(|(served, _)| *served == ext)
                .map(|(_, lang)| *lang)
        })
    }

    pub fn server(&self, key: &str) -> Option<&ServerDef<'s>> {
        self.servers.iter().find(|def| def.key == key)
    }

    /// `lang`'s main server.
    pub fn main(&self, lang: &str) -> Option<&ServerDef<'s>> {
        self.servers
            .iter()
            .find(|def| !def.linter && def.serves(lang))
    }

    /// Every server for `lang`: the main one first, then linters.
    pub fn keys(&self, lang: &str) -> Vec<&'s str> {
        let main = self.main(lang).map(|def| def.key);
        let linters = self
            .servers
            .iter()
            .filter(|def| def.linter && def.serves(lang))
            .map(|def| def.key);
        main.into_iter().chain(linters).collect()
    }

    /// Every main server, in order.
    pub fn mains(&self) -> impl Iterator<Item = &ServerDef<'s>> {
        self.servers.iter().filter(|def| !def.linter)
    }
}

static REGISTRY: OnceLock<Registry<'static>> = OnceLock::new();

/// The servers in effect, from the settings (the built-ins until those are
/// loaded, as in tests).
pub fn registry() -> &'static Registry<'static> {
    REGISTRY.get_or_init(|| Registry::build(crate::settings::get()).0)
}

fn builtins(settings: &Settings) -> Vec<ServerDef<'static>> {
    let python: Vec<(&str, &str)> = vec![("py", "python"), ("pyi", "python")];
    let mut servers = vec![ServerDef {
        key: "python",
        label: "Python",
        linter: false,
        extensions: python.clone(),
        resolver: Resolver::Python,
        missing: "No Python language server found, in the project's venv or on PATH. \
             Install ty: `uv add --dev ty` in the project, or `uv tool install ty` \
             (basedpyright, pyright and pylsp work too)"
            .into(),
    }];
    if settings.python.ruff {
        servers.push(ServerDef {
            key: "ruff",
            label: "ruff",
            linter: true,
            extensions: python,
            resolver: Resolver::Ruff,
            missing: "ruff not found. Install it: `uv add --dev ruff`".into(),
        });
    }
    servers.push(ServerDef {
        key: "rust",
        label: "Rust",
        linter: false,
        extensions: vec![("rs", "rust")],
        resolver: Resolver::Command {
            command: "rust-analyzer".into(),
            args: Vec::new(),
            init_options: None,
        },
        missing: "No working Rust language server found (rustup's rust-analyzer \
             needs its component). Install it: `rustup component add rust-analyzer`"
            .into(),
    });
    servers.push(ServerDef {
        key: "ts",
        label: "TypeScript",
        linter: false,
        extensions: vec![
            ("ts", "typescript"),
            ("mts", "typescript"),
            ("cts", "typescript"),
            ("tsx", "typescriptreact"),
            ("js", "javascript"),
            ("mjs", "javascript"),
            ("cjs", "javascript"),
            ("jsx", "javascriptreact"),
        ],
        resolver: Resolver::TypeScript,
        missing: "No TypeScript language server found. Install TypeScript 7 \
             (`npm install -D typescript@7`) or typescript-language-server"
            .into(),
    });
    servers
}

/// Every extension served so far, with its language id.
fn known_extensions<'s>(servers: &[ServerDef<'s>]) -> BTreeMap<&'s str, &'s str> {
    let mut known = BTreeMap::new();
    for (ext, lang) in servers.iter().flat_map(|def| &def.extensions) {
        known.entry(*ext).or_insert(*lang);
    }
    known
}

/// A section's extensions with their language ids: its own `language_ids`,
/// else the id the extension already has, else `language_id`, else the
/// section name.
fn extensions<'s>(
    name: &'s str,
    entry: &'s LspEntry,
    exts: &'s [String],
    known: &BTreeMap<&'s str, &'s str>,
    problems: &mut Vec<String>,
) -> Vec<(&'s str, &'s str)> {
    let resolved: Vec<(&str, &str)> = exts
        .iter()
        .map(|ext| {
            let ext = ext.trim_start_matches('.');
            let lang = entry
                .language_ids
                .iter()
                .find(|(key, _)| key.trim_start_matches('.') == ext)
                .map(|(_, lang)| lang.as_str())
                .or_else(|| known.get(ext).copied())
                .or(entry.language_id.as_deref())
                .unwrap_or(name);
            (ext, lang)
        })
        .collect();
    for key in entry.language_ids.keys() {
        let key = key.trim_start_matches('.');
        if !resolved.iter().any(|(ext, _)| *ext == key) {
            problems.push(format!(
                "settings.toml: lsp.{name}.language_ids names .{key}, which is not in its extensions"
            ));
        }
    }
    resolved
}

/// What is shown when a configured program is not found.
fn missing_message(label: &str, command: Option<&str>, install: Option<&str>) -> String {
    let mut msg = format!("No {label} language server found");
    if let Some(command) = command {
        msg.push_str(": `");
        msg.push_str(command);
        msg.push_str("` is not installed or does not run");
    }
    if let Some(hint) = install {
        msg.push_str(". Install it: ");
        msg.push_str(hint);
    }
    msg
}

/// TOML init options as the JSON the server is sent.
fn init_options(
    name: &str,
    entry: &LspEntry,
    problems: &mut Vec<String>,
) -> Option<serde_json::Value> {
    let table = entry.init_options.as_ref()?;
    match serde_json::to_value(table) {
        Ok(value) => Some(value),
        Err(err) => {
            problems.push(format!("settings.toml: lsp.{name}.init_options: {err}"));
            None
        }
    }
}

/// Change a built-in by its section.
fn apply<'s>(
    def: &mut ServerDef<'s>,
    entry: &'s LspEntry,
    known: &BTreeMap<&'s str, &'s str>,
    problems: &mut Vec<String>,
) {
    let name = def.key;
    if let Some(label) = &entry.label {
        def.label = label;
    }
    if let Some(linter) = entry.linter {
        def.linter = linter;
    }
    if let Some(exts) = &entry.extensions {
        // Its own extensions are not "known" to itself: their ids come from
        // the section or from other servers.
        let mut others = known.clone();
        for (ext, _) in &def.extensions {
            others.remove(ext);
        }
        def.extensions = extensions(name, entry, exts, &others, problems);
    }
    let options = init_options(name, entry, problems);
    if let Some(command) = &entry.command {
        def.resolver = Resolver::Command {
            command: command.clone(),
            args: entry.args.clone().unwrap_or_default(),
            init_options: options,
        };
        def.missing = missing_message(def.label, Some(command), entry.install.as_deref());
        return;
    }
    if let Resolver::Command {
        command,
        args,
        init_options,
    } = &mut def.resolver
    {
        if let Some(new) = &entry.args {
            args.clone_from(new);
        }
        if options.is_some() {
            *init_options = options;
        }
        if entry.install.is_some() {
            def.missing = missing_message(def.label, Some(command), entry.install.as_deref());
        }
        return;
    }
    for (field, given) in [
        ("args", entry.args.is_some()),
        ("init_options", entry.init_options.is_some()),
    ] {
        if given {
            problems.push(format!(
                "settings.toml: lsp.{name}.{field} needs a `command` (the built-in \
                 {} lookup picks its own); ignored",
                def.label
            ));
        }
    }
    if entry.install.is_some() {
        def.missing = missing_message(def.label, None, entry.install.as_deref());
    }
}

/// A server a section adds.
fn added<'s>(
    name: &'s str,
    entry: &'s LspEntry,
    known: &BTreeMap<&'s str, &'s str>,
    problems: &mut Vec<String>,
) -> Option<ServerDef<'s>> {
    let Some(command) = &entry.command else {
        problems.push(format!(
            "settings.toml: lsp.{name} needs a `command`; left out"
        ));
        return None;
    };
    let exts = entry.extensions.as_deref().unwrap_or_default();
    if exts.is_empty() {
        problems.push(format!(
            "settings.toml: lsp.{name} needs `extensions` (the files it serves); left out"
        ));
        return None;
    }
    let label = entry.label.as_deref().unwrap_or(name);
    Some(ServerDef {
        key: name,
        label,
        linter: entry.linter.unwrap_or(false),
        extensions: extensions(name, entry, exts, known, problems),
        resolver: Resolver::Command {
            command: command.clone(),
            args: entry.args.clone().unwrap_or_default(),
            init_options: init_options(name, entry, problems),
        },
        missing: missing_message(label, Some(command), entry.install.as_deref()),
    })
}

/// One language id per extension, one main server per language: a later
/// server that disagrees is reported, and the earlier one kept.
fn check_overlaps(servers: &mut [ServerDef<'_>], problems: &mut Vec<String>) {
    let mut ids: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    let mut mains: BTreeMap<&str, &str> = BTreeMap::new();
    for def in servers.iter_mut() {
        for (ext, lang) in &mut def.extensions {
            match ids.get(ext) {
                Some((first, owner)) if first != lang => {
                    problems.push(format!(
                        "settings.toml: lsp.{}: .{ext} files are `{first}` (from {owner}); \
                         using that",
                        def.key
                    ));
                    *lang = first;
                }
                Some(_) => {}
                None => {
                    ids.insert(ext, (lang, def.key));
                }
            }
        }
        if def.linter {
            continue;
        }
        for lang in def.languages() {
            match mains.get(lang) {
                Some(owner) if *owner != def.key => problems.push(format!(
                    "settings.toml: lsp.{}: {lang} already has a main server ({owner}); \
                     turn that off with `enabled = false`, or set `linter = true`",
                    def.key
                )),
                Some(_) => {}
                None => {
                    mains.insert(lang, def.key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// Settings parsed from `toml`, with every problem found (the registry's
    /// included: parsing checks it).
    fn build(toml: &str) -> (Settings, Vec<String>) {
        Settings::parse(toml)
    }

    #[test]
    fn the_builtins_serve_their_files() {
        let settings = Settings::default();
        let (registry, problems) = Registry::build(&settings);
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(registry.language("rs"), Some("rust"));
        assert_eq!(registry.language("tsx"), Some("typescriptreact"));
        assert_eq!(registry.language("go"), None);
        assert_eq!(registry.keys("python"), ["python", "ruff"]);
        assert_eq!(registry.keys("javascript"), ["ts"]);
        // Nothing unknown falls through to TypeScript.
        assert_eq!(registry.keys("go"), Vec::<&str>::new());
        assert!(registry.main("go").is_none());
    }

    #[test]
    fn a_section_adds_a_server() -> TestResult {
        let (settings, problems) = build(
            "[lsp.go]\ncommand = \"gopls\"\nextensions = [\".go\"]\nlabel = \"Go\"\n\
             install = \"`go install golang.org/x/tools/gopls@latest`\"\n\
             [lsp.go.init_options]\nstaticcheck = true\n",
        );
        assert_eq!(problems, Vec::<String>::new());
        let (registry, problems) = Registry::build(&settings);
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(registry.language("go"), Some("go"));
        assert_eq!(registry.keys("go"), ["go"]);
        let gopls = registry.main("go").ok_or("no Go server")?;
        assert_eq!(gopls.label, "Go");
        assert_eq!(
            gopls.resolver,
            Resolver::Command {
                command: "gopls".into(),
                args: Vec::new(),
                init_options: Some(serde_json::json!({ "staticcheck": true })),
            }
        );
        assert!(gopls.missing.contains("`gopls`"), "{}", gopls.missing);
        assert!(gopls.missing.contains("go install"), "{}", gopls.missing);
        Ok(())
    }

    #[test]
    fn a_linter_for_known_files_keeps_their_language() {
        let (settings, _) =
            build("[lsp.mypy]\ncommand = \"mypy-lsp\"\nextensions = [\"py\"]\nlinter = true\n");
        let (registry, problems) = Registry::build(&settings);
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(registry.keys("python"), ["python", "ruff", "mypy"]);
    }

    #[test]
    fn a_section_changes_or_turns_off_a_builtin() -> TestResult {
        let (settings, _) = build(
            "[lsp.rust]\nargs = [\"--log\"]\n[lsp.ruff]\nenabled = false\n\
             [lsp.ts]\ncommand = \"vtsls\"\nargs = [\"--stdio\"]\n",
        );
        let (registry, problems) = Registry::build(&settings);
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(registry.keys("python"), ["python"]);
        let rust = registry.server("rust").ok_or("no rust")?;
        assert!(
            matches!(&rust.resolver, Resolver::Command { command, args, .. }
                if command == "rust-analyzer" && args == &["--log"])
        );
        let typescript = registry.server("ts").ok_or("no ts")?;
        assert!(
            matches!(&typescript.resolver, Resolver::Command { command, .. } if command == "vtsls")
        );
        // Still serves every TypeScript and JavaScript file.
        assert_eq!(registry.language("jsx"), Some("javascriptreact"));
        Ok(())
    }

    #[test]
    fn mistakes_are_reported() {
        let cases = [
            ("[lsp.go]\nextensions = [\"go\"]\n", "needs a `command`"),
            ("[lsp.go]\ncommand = \"gopls\"\n", "needs `extensions`"),
            ("[lsp.python]\nargs = [\"x\"]\n", "needs a `command`"),
            (
                "[lsp.other]\ncommand = \"pyls\"\nextensions = [\"py\"]\n",
                "already has a main server",
            ),
            (
                "[lsp.go]\ncommand = \"gopls\"\nextensions = [\"go\"]\n\
                 language_ids = { mod = \"go.mod\" }\n",
                "not in its extensions",
            ),
        ];
        for (toml, want) in cases {
            let (_, problems) = build(toml);
            assert!(
                problems.iter().any(|problem| problem.contains(want)),
                "{toml}: {problems:?}"
            );
        }
        // An unknown field is a parse error, like elsewhere in the file.
        let (settings, problems) = build("[lsp.go]\ncomand = \"gopls\"\n");
        assert!(settings.lsp.is_empty());
        assert_eq!(problems.len(), 1);
    }
}
