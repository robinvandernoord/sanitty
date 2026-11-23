# sanitty

A little sanity for your tty.

sanitty is a non-modal IDE written in Rust that runs entirely in the terminal. Just type to edit. It uses the `karet` crates and `ratatui` and is designed to run over SSH and in terminals built into other apps. Most shortcuts use Alt because host apps can intercept Ctrl combinations.

## Features

- Search Everywhere finds files, actions, workspace symbols and text. Jump directly to a file, symbol or line, reopen recent files, or browse the project tree.
- Find and replace in the current file, or search across the project with Find in Files.
- Language servers provide definition and implementation jumps, usages, rename, problems, quick documentation, formatting and completion. Completion applies auto-imports when the server supplies them.
- Tree-sitter syntax highlighting (edits reparse only what changed, except in TypeScript), rainbow columns for CSV and TSV, and colour swatches next to colour values in style files.
- A Review view (`Alt+9`) for your uncommitted changes or this branch against main: a side-by-side or inline diff, comments on lines and ranges, viewed marks, and the whole review copied to the clipboard as Markdown. Comments are kept between runs, outside the project.
- Check Keys records which shortcuts reach sanitty.

## Status

sanitty is an early personal project. The editor and language-server milestones are complete, and the review view is in progress. The database view, multi-caret editing and a settings file are planned. See [PLAN.md](PLAN.md).

## Build and run

You need Rust 1.92 or newer and a C compiler for the Tree-sitter parsers.

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
| Python | `ty` (`uv tool install ty`), plus `ruff` (`uv tool install ruff`) for lint warnings and formatting. When `ty` is not installed, sanitty uses the first of `basedpyright-langserver`, `pyright-langserver` and `pylsp` it finds. |
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
| Edit | Duplicate Line | `Alt+D` |
| | Delete Line | `Alt+Y` |
| | Undo | `Ctrl+Z` |
| | Redo | `Alt+Z` |
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

On Linux, sanitty uses `$XDG_CONFIG_HOME/sanitty`, with `~/.config/sanitty` as the default. Put a JSON colour theme in `theme.json` (the common editor theme format, with `colors` and `tokenColors`; unknown keys fall back to the built-in dark theme), or override shortcuts in `keymap.toml`. For example, to move Go to Definition from `Alt+B` to `F12`:

```toml
[bindings]
"f12" = "goto-definition"
"alt+b" = "none"
```

Set `SANITTY_LOG` to a file path to record debug output, including language-server messages.

## License

sanitty is released under the MIT license. See [LICENSE](LICENSE).

The vendored `karet-lsp` 0.8.0 is used under MIT and carries two fixes for hover Markdown and diagnostic requests. See [SANITTY-PATCH.md](vendor/karet-lsp/SANITTY-PATCH.md).
