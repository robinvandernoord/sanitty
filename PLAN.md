# sanitty: plan

A PyCharm-flavoured terminal IDE built on the published `karet-*` crates. It runs on
the office PC over SSH (from Ghostty, sometimes the T3 integrated terminal), so files,
language servers, git, docker and databases are all local to it.

Status: approved plan; shortcuts are starting defaults. Milestone 1 is built.

## Principles

- One full-screen view at a time. Everything else (file finder, project tree, usages,
  actions) is a popup that disappears when you are done with it.
- Not modal. PyCharm key bindings first. Every shifted chord also has a fallback that
  survives terminals without the kitty keyboard protocol (T3's terminal, tmux, byobu),
  where `Ctrl+Shift+N` arrives as `Ctrl+N`.
- Keyboard only. Mouse capture stays off so the terminal keeps normal text selection.
- Everything is rebindable in `~/.config/sanitty/keymap.toml`.

## Views

Three top-level views, each taking the whole screen.

| View     | Key     | Purpose                                      |
|----------|---------|----------------------------------------------|
| Code     | `Alt+C` | Editing and navigation (default)             |
| Review   | `Alt+R` | Diffs and review comments                    |
| Database | `Alt+D` | Schema, queries, result grids, row editing   |

Like all bindings, these are defaults to tweak after real use.

### Code view

No tab line. `Ctrl+E` (recent files) is the file switcher; `Ctrl+E Enter` jumps back to
the previous file. Open buffers stay alive in the background, so unsaved changes are
never lost by switching.

```
 src/app.rs *   2 errors  3 warnings   |  1 other file with errors          rust
  12 | fn main() {
  13 |     let x: u32 = "text";   mismatched types: expected u32, found &str
  14 | }
 mismatched types: expected u32, found &str                                13:18
 Ctrl+K Search   F8 Next problem   Ctrl+F Find   Alt+B Definition   F1 All keys
```

- Top bar, what you are looking at: file (with `*` when unsaved), its error and warning
  counts in colour (or "no problems"; a CSV file also shows the column under the
  caret), other files with errors (else with warnings), running language servers.
- Bottom, what is happening: the last message (it lasts one key press) or the problem
  under the caret, and the caret position.
- Hint bar: the keys for whatever has focus.

CSV and TSV files get a colour per column, like `bat`: the palette cycles through the
theme's syntax colours, separators are dimmed, and quoted fields (including separators
and line breaks inside quotes) stay in one column. The separator is detected from the
first lines (comma, semicolon for Dutch Excel exports, tab, pipe). The top bar shows the
column under the caret with its header name, e.g. `col 4: aanleverroute`. There is no
CSV language server, so sanitty checks the structure itself: rows with more fields than
the header (the extra fields are underlined), rows with fewer, and a quote that is never
closed. These count as problems like any other (top bar, `F8`).

Popups over the Code view:

```
      +- Go to File (3/1204) -------------------+    +- project -----------+
      |> modpy                                  |    | v apps              |
      |models.py        apps/shop               |    |   v shop            |
      |models.py        apps/users              |    |       models.py  <  |
      |mod_python.py    vendor/legacy           |    |       views.py      |
      +-----------------------------------------+    +---------------------+
```

### Review view

tuicr-like: a list of changed files, a diff, line comments, and export.

```
 [Review]  working tree vs HEAD  (3 files, +41 -12)
+- files ------------++- apps/shop/models.py ------------------ side-by-side -+
| v models.py   +12  || 40   def total(self):        | 40   def total(self):  |
|   views.py    +27  || 41 -     return sum(...)     | 41 +     return sum(   |
|   urls.py      +2  ||                              | 42 +       l.price ... |
|                    ||   [issue] use Decimal here                            |
+--------------------++- c comment  v viewed  \ inline/split  ]/[ next file --+
```

- Sources: working tree vs HEAD, branch vs main, a single commit, a GitHub PR (via `gh`).
- Layout: side-by-side, switching to inline when the terminal is narrow; `\` toggles.
- Comments on a line or range, typed as issue / suggestion / note / praise.
- Export: the whole review is copied to the clipboard as Markdown (OSC 52, works over
  SSH), ready to paste into Claude.
- Viewed-file marks persist per source in `.sanitty/review-state.json`.

### Database view

```
 [Database]  shop-db (postgres, from docker compose: db)
+- schema ---------++- query.sql ----------------------------------------------+
| v public         || select id, email, created_at from auth_user              |
|   > auth_user    || where created_at > now() - interval '1 day';             |
|   > shop_order   |+---------------------------------------- Ctrl+Enter run -+
|   > shop_product |+- 3 rows, 12 ms ------------------------------------------+
| > audit          || id | email               | created_at                    |
|                  || 41 | a@example.nl        | 2026-11-19 09:12              |
+------------------++- Enter edit cell, Ctrl+S write (shows SQL first) -------+
```

- Engines: PostgreSQL, MySQL/MariaDB, SQLite (one driver layer, `sqlx`).
- Connections: discovered from `docker compose config` (service image plus its
  `POSTGRES_*` / `MYSQL_*` / `MARIADB_*` env vars and published ports), overridden or
  extended in `.sanitty/db.toml`.
- Safety: every write (inline edit, UPDATE, DELETE, DDL) shows the exact SQL and asks for
  confirmation. Inline edits need a primary key and generate `UPDATE ... WHERE pk = ...`.
- From the Code view, `Ctrl+Enter` in a `.sql` file runs the selection (or the statement
  under the caret) against the active connection and shows the grid in a bottom popup.

### Terminal

`Alt+F12` suspends sanitty and hands the terminal to `$SHELL` in the workspace root;
exiting the shell returns. No embedded terminal for now.

## Finding your way

- **Hint bar**: the bottom line shows the keys that matter right now and changes with
  context (editor, find bar, launcher, tree, rename). It shows one key per action: the
  first binding this terminal can actually send, else the launcher route (`Ctrl+K >`).
- **Launcher** (`Ctrl+K`, Search Everywhere): files (recent first; build output, caches
  and virtualenvs are never listed), actions with their
  key, workspace symbols (from the language server) and text in files, all in one list.
  `Tab` cycles the filters All / Files / Actions / Symbols / Text, and the prefixes `>`
  actions, `@` symbols, `/` text, `:` line narrow the same way; neither is ever needed,
  since All searches everything. Every action is reachable here (this replaces PyCharm's
  Find Action), which is the safety net for keys a terminal swallows. Ranking prefers
  names that start with or contain the query, and actions also match synonyms ("jump to
  source", "references"); letters scattered through a long name rank last.
- **Find bar** (`Ctrl+F`, `Ctrl+R` for replace): incremental, smart case, toggles for
  case, whole word and regex. `F3` / `Shift+F3` repeat the last search with the bar closed.
- **Problems**: an underlined error shows its message at the end of the line (errors on
  every line, anything on the caret's line) and in the status line. `F8` / `Shift+F8`
  jump to the next / previous problem, continuing into the next file with problems.
  Problems are kept for files that are not open too: the top bar counts them and the
  launcher has a Problems tab (`Alt+6`, or `!` as prefix). Status messages last until the next key, so they
  never hide the problem under the caret.
- **Colours from the language server**: names are coloured by what the server says they
  are (function, parameter, type, ...), on top of tree-sitter highlighting, so the editor
  and the hover text use the same colours.
- **Ctrl+C** copies a selection; without one it leaves the file for the home screen,
  with the project tree open on that file (files stay open, `Ctrl+E` brings them
  back); on the home screen it quits, also while the tree is open. The tree always
  reopens as it was left: same open folders, same selection.
- **Style files**: a swatch in the actual colour in front of every `#hex`, `rgb()` and
  `hsl()` value (CSS, SCSS, Sass, Less, HTML, Vue, Svelte). SCSS and Less are
  highlighted with the CSS grammar (karet's own SCSS rules colour almost nothing), plus
  their `$`/`@` variables.
- **Types and docs**: resting the caret on a name shows its type in the bottom line
  (`g: Greeter`) when there is no message or problem to show. `Alt+H` opens a panel
  under the caret with the full signature and docstring; any key closes it.
- **All keys** (`F1`): every action grouped, filterable; Enter runs it.
- **Check keys** (in the launcher): asks for each default key in turn and records whether
  it arrives, arrives as something else, or is swallowed. Writes
  `~/.config/sanitty/keycheck-<terminal>.md` and copies it. The results decide the final
  keymap; no automatic per-terminal profiles, so muscle memory stays the same everywhere.

## Key bindings (defaults)

The shown key works in both T3's terminal and Ghostty (from the key check reports of
2026-11-19). PyCharm keys stay bound as extras where a terminal delivers them.

| Action                   | Shown key        | Extra (PyCharm)    |
|--------------------------|------------------|--------------------|
| Search everywhere        | `Ctrl+K`         |                    |
| Go to file               | `Alt+N`          | `Ctrl+Shift+N` (T3) |
| Go to symbol             | `Ctrl+K @`       | `Ctrl+Alt+Shift+N` |
| Go to line               | `Ctrl+G`         |                    |
| Recent files             | `Ctrl+E`         |                    |
| Previous file            | `Ctrl+E Enter`   |                    |
| Project tree             | `Alt+1`          |                    |
| Find / replace in file   | `Ctrl+F` / `Ctrl+R` |                 |
| Find next / previous     | `F3` / `Shift+F3` |                   |
| Find in files            | `Alt+F`          | `Ctrl+Shift+F` (T3) |
| All keys                 | `F1`             |                    |
| Go to definition         | `Alt+B`          | `Ctrl+B` (Ghostty) |
| Go to implementation     | `Alt+I`          | `Ctrl+Alt+B` (Ghostty) |
| Find usages              | `Alt+F7`         |                    |
| Rename                   | `F2`             | `Shift+F6` (Ghostty) |
| Problems (all files)     | `Alt+6`          |                    |
| Next / previous problem  | `F8` / `Shift+F8` (crosses files) |   |
| Quick documentation      | `Alt+H`          | (`Ctrl+Q` avoided: T3 uses it to quit) |
| Reformat                 | `Ctrl+Alt+L`     |                    |
| Duplicate line           | `Ctrl+D`         |                    |
| Delete line              | `Ctrl+Y`         |                    |
| Comment line             | `Ctrl+/`         |                    |
| Undo / redo              | `Ctrl+Z` / `Ctrl+Shift+Z` | `Alt+Z`   |
| Navigate back / forward  | `Ctrl+Alt+Left/Right` |               |
| Shell                    | `Alt+F12`        | `Alt+T`            |
| Save                     | `Ctrl+S`         |                    |
| Close file               | `Alt+W`          | `Ctrl+F4`          |
| Copy (quit without selection) | `Ctrl+C`    |                    |
| Quit                     | `Alt+Q`          | `Ctrl+Shift+Q` (T3) |

Find Action is not a separate action: actions show up in the launcher's All results,
and `>` only narrows to them. In T3, `Alt+Left` arrives as `Alt+B` (go to definition).
`Ctrl+Alt+L` is not checked yet (milestone 2). All of these remain
defaults, to be tweaked after real use.

## Architecture

One Rust binary.

- `karet-text`, `karet-editor`, `karet-syntax`, `karet-theme`: buffers, the editor widget,
  tree-sitter highlighting.
- `karet-lsp`: language servers. One per language, started on first use. Python uses
  Astral's ty (`ty server`) for types and navigation, with ruff (`ruff server`) alongside
  for lint and format. ty is still beta, so basedpyright stays a configured fallback and
  the choice is a setting. Rust uses rust-analyzer. TypeScript/JavaScript use
  TypeScript 7's own server (`tsc --lsp --stdio`, the project's `node_modules` first,
  else the `tsc` on PATH, e.g. from `bun i -g typescript`), with
  typescript-language-server as the fallback for older TypeScript (pointed at a global
  `tsserver.js` when the project has none). No `package.json` is needed. Servers run on the host with the project's own venv.
- `karet-diff` (with its `view` feature) and `karet-vcs`: diffs and git facts.
- `sqlx`: databases.
- Own code: views, popups, keymap, config, review model, compose discovery.

Config lives in `~/.config/sanitty/` (keymap, theme, settings) and per project in
`.sanitty/` (db connections, review state).

## Milestones

1. **Editor shell** (done, awaiting demo): editing, highlighting, go to file, recent
   files, previous file, project tree, find action, keymap file, shell, rainbow CSV.
2. **LSP** (partly in prototype): definition, implementation, usages and rename work.
   Remaining: completion popup, diagnostics list, reformat, ruff as a
   second Python server, ty as the default Python server, test with rust-analyzer and typescript-language-server on large
   repos.
3. **Review**: Review view, sources, comments, Markdown export.
4. **Database**: compose discovery, schema tree, query editor, result grid, confirmed
   writes, `Ctrl+Enter` from `.sql` files.
5. **Polish**: search in files, multi-caret, VS Code theme loading, settings file.

Each milestone ends with a short demo for approval before the next one starts.
