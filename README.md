# sanitty

A little sanity for your tty.

sanitty is a non-modal IDE written in Rust that runs entirely in the terminal. Just type to edit. It uses the `karet` crates and `ratatui` and is designed to run over SSH and in terminals built into other apps. Most shortcuts use Alt because host apps can intercept Ctrl combinations.

## Features

- Search Everywhere finds files, actions, workspace symbols and text. Jump directly to a file, symbol or line, reopen recent files, or browse the project tree.
- Find and replace in the current file, or search across the project with Find in Files.
- Language servers provide definition and implementation jumps, usages, rename, problems, quick documentation, formatting and completion. Completion applies auto-imports when the server supplies them.
- Tree-sitter syntax highlighting (edits reparse only what changed, except in TypeScript), rainbow columns for CSV and TSV, and colour swatches next to colour values in style files.
- A Review view (`Alt+9`) for your uncommitted changes, this branch against main, one or more commits, or an open GitHub pull request (through `gh`): a side-by-side or inline diff in syntax colours, comments on lines and ranges, viewed marks, and the whole review copied to the clipboard as Markdown. Comments are kept between runs, outside the project.
- A Database view (`Alt+8`) for PostgreSQL, MySQL/MariaDB and SQLite: connections found in `docker compose` (or set in `.sanitty/db.toml`), a schema tree, a query editor with completion (`Alt+J`) and documentation (`Alt+H`) from the schema, a result grid, and cell edits in any result that includes the table's primary key. Every statement that changes the database is shown and confirmed before it runs. `Alt+Enter` also runs the statement under the caret in any file, with the result in a popup.
- Multi-caret editing: add carets above and below, at the next or every occurrence of a word, or at the ends of selected lines, then type, delete, paste or comment at all of them as one undo step.
- Check Keys records which shortcuts reach sanitty.

## Status

sanitty is an early personal project. The editor, language-server, review and database milestones are complete, and so are multi-caret editing and a settings file. See [PLAN.md](PLAN.md).

## Build and run

You need Rust 1.94 or newer and a C compiler for the Tree-sitter parsers.

```sh
git clone https://github.com/robinvandernoord/sanitty.git
cd sanitty
cargo build --release --locked
./target/release/sanitty /path/to/project
```

Pass a directory to open it as the workspace, or a file to open that file. With no path, sanitty starts in the current directory. Inside a Git checkout, the workspace is the whole repository. Press `F1` to see every action and its key.

Language servers are optional and installed separately on the machine running sanitty. A server starts when you open the first file of its language; if it is missing, sanitty works without it for the rest of the session, so restart sanitty after installing one.

| Language | Server |
| --- | --- |
| Python | `ty` (`uv tool install ty`), plus `ruff` (`uv tool install ruff`) for lint warnings and formatting. When `ty` is not installed, sanitty uses the first of `basedpyright-langserver`, `pyright-langserver` and `pylsp` it finds. `settings.toml` can prefer another server or turn ruff off. |
| Rust | `rust-analyzer` (`rustup component add rust-analyzer`). |
| TypeScript and JavaScript | TypeScript 7 or newer: `node_modules/.bin/tsc` in the workspace (`npm install -D typescript@7`), else `tsc` on `PATH`. Older TypeScript works through `typescript-language-server` on `PATH`. |

## Default keys

In Recent Files, `Enter` opens the preselected previous file. Copy Line and Check Keys are available in Search Everywhere.

| Group | Action | Key |
| --- | --- | --- |
| Navigate | Search Everywhere | `Alt+K` |
| | Go to File | `Alt+N` |
| | Go to Symbol | `Alt+S` |
| | Go to Line | `Alt+G` |
| | Recent Files | `Alt+E` |
| | Review Changes | `Alt+9` |
| | Database | `Alt+8` |
| | Project Tree | `Alt+1` |
| | Navigate Back | `Alt+,` |
| | Navigate Forward | `Alt+.` |
| Search | Find in File | `Ctrl+F` |
| | Replace in File | `Alt+R` |
| | Find Next | `F3` |
| | Find Previous | `Shift+F3` |
| | Find in Files | `Alt+F` |
| Code | Go to Definition | `Alt+B` |
| | Go to Implementation | `Alt+I` |
| | Find Usages | `Alt+F7` |
| | Quick Documentation | `Alt+H` |
| | Rename Symbol | `F2` |
| | Problems | `Alt+6` |
| | Next Problem | `F8` |
| | Previous Problem | `Shift+F8` |
| | Toggle Line Comment | `Alt+/` |
| | Reformat File | `Alt+L` |
| | Code Completion | `Alt+J` |
| | Run SQL | `Alt+Enter` |
| Edit | Duplicate Line | `Alt+D` |
| | Delete Line | `Alt+Y` |
| | Add Caret Above / Below | `Alt+Up` / `Alt+Down` |
| | Add Next Occurrence | `Alt+A` |
| | Remove Last Added Caret | `Alt+U` |
| | Select All Occurrences | `Ctrl+Alt+J` |
| | Carets at Ends of Selected Lines | `Ctrl+Alt+G` |
| | Undo | `Ctrl+Z` |
| | Redo | `Ctrl+Shift+Z` |
| | Copy | `Ctrl+C` |
| | Cut | `Ctrl+X` |
| | Paste (from sanitty's clipboard) | `Ctrl+V` |
| | Select All | `Ctrl+A` |
| File | Save | `Ctrl+S` |
| | Close File | `Alt+W` |
| App | All Keys | `F1` |
| | Shell | `Alt+T` |
| | Quit | `Alt+Q` |

With no selection, `Ctrl+C` returns from a file to the project tree. From the home screen it quits, asking first when files have unsaved changes. To paste text from outside sanitty, use your terminal's own paste shortcut.

## Terminal notes

The default keys were tested in Ghostty and T3 Code's terminal. Launch sanitty with `--check-keys` or choose Check Keys in Search Everywhere to see which keys arrive as expected, arrive as a different key, or get intercepted. The check writes `keycheck-<terminal>.md` in the config directory.

## Configuration

On Linux, sanitty uses `$XDG_CONFIG_HOME/sanitty`, with `~/.config/sanitty` as the default. Override shortcuts in `keymap.toml`. For example, to move Go to Definition from `Alt+B` to `F12`:

```toml
[bindings]
"f12" = "goto-definition"
"alt+b" = "none"
```

Other settings go in `settings.toml`. Every key is optional; a mistake is shown in the status line and the default is used. Changes apply when sanitty starts.

```toml
[editor]
tab_size = 4          # spaces Tab inserts

[python]
server = "ty"         # ty, basedpyright, pyright or pylsp; if it is missing, the next one installed
ruff = true           # ruff alongside, for lint warnings and formatting

[database]
table_rows = 500      # rows when opening a table from the tree
max_rows = 10000      # rows a result keeps
```

The Database view finds PostgreSQL and MySQL/MariaDB services in `docker compose config` by their image or their `POSTGRES_*`, `MYSQL_*` and `MARIADB_*` variables. Override a service or add connections in `.sanitty/db.toml` in the project:

```toml
[connections.db]          # a compose service of that name: change its port
port = 5433

[connections.reports]
url = "postgres://reader@reports.internal/reports"
password_env = "REPORTS_PASSWORD"

[connections.local]
path = "db.sqlite3"       # SQLite, relative to the project
```

Queries are kept per connection in sanitty's state directory (`~/.local/state/sanitty/`), not in the project.

Set `SANITTY_LOG` to a file path to record debug output, including language-server messages.

## License

sanitty is released under the MIT license. See [LICENSE](LICENSE).

The vendored `karet-lsp` 0.8.0 is used under MIT and carries two fixes for hover Markdown and diagnostic requests. See [SANITTY-PATCH.md](vendor/karet-lsp/SANITTY-PATCH.md).
